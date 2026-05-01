use unitctl::config::{self, Cli};
use unitctl::context::Context;
use unitctl::env::{CameraEnvWriter, FluentbitEnvWriter, MavlinkEnvWriter};
use unitctl::mavlink::drone_component::DroneComponent;
use unitctl::mavlink::sniffer_component::MavlinkSniffer;
use unitctl::mavlink::telemetry_reporter::TelemetryReporter;
use unitctl::sensors::SensorManager;
use unitctl::services::modem_access::ModemAccessService;
use unitctl::services::mqtt::commands::CommandProcessor;
use unitctl::services::mqtt::handlers::restart::RestartCompletionPublisher;
use unitctl::services::mqtt::status::StatusPublisher;
use unitctl::services::mqtt::telemetry::TelemetryPublisher;
use unitctl::services::mqtt::transport::MqttTransport;
use unitctl::Task;

use clap::Parser;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Load config before initializing tracing so config.general.debug can enable debug logging
    let config = match config::load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "error: failed to load configuration from {:?}: {e}",
                cli.config
            );
            std::process::exit(1);
        }
    };

    let debug_enabled = cli.debug || config.general.debug;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if debug_enabled {
            EnvFilter::new("debug")
        } else {
            EnvFilter::new("info")
        }
    });

    tracing_subscriber::fmt().with_env_filter(filter).init();

    info!("unitctl starting");

    // Fail fast if the configured interface has no IPv4 address.
    // This catches misconfiguration before any tasks are spawned.
    // Runtime failures during MQTT reconnect are handled gracefully in StatusPublisher.
    match unitctl::net::resolve_ipv4(&config.general.interface) {
        Ok(ip) => {
            info!(
                interface = %config.general.interface,
                ip = %ip,
                "interface IP resolved"
            );
        }
        Err(e) => {
            tracing::error!(
                interface = %config.general.interface,
                error = %e,
                "failed to resolve interface IP at startup"
            );
            std::process::exit(1);
        }
    }

    info!(
        host = %config.mavlink.host,
        local_mavlink_port = config.mavlink.local_mavlink_port,
        remote_mavlink_port = config.mavlink.remote_mavlink_port,
        self_sysid = config.mavlink.self_sysid,
        self_compid = config.mavlink.self_compid,
        "configuration loaded"
    );

    let ctx = Context::new(config);
    info!("context initialized");

    let cancel = CancellationToken::new();

    // Spawn signal handler for graceful shutdown
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("shutdown signal received, initiating graceful shutdown");
        cancel_signal.cancel();
    });

    let mut handles = vec![];

    // Spawn modem discovery as a background task.
    // Once the modem is found, it is stored in Context so sensors can use it.
    let modem_ctx = Arc::clone(&ctx);
    let modem_cancel = cancel.clone();
    let modem_handle = tokio::spawn(async move {
        match ModemAccessService::start(&modem_ctx.config.sensors.lte, &modem_cancel).await {
            Ok(service) => {
                info!("modem access service started, storing in context");
                modem_ctx.set_modem(service).await;
            }
            Err(e) => {
                warn!(error = %e, "modem access service failed to start");
            }
        }
    });
    handles.push(modem_handle);

    // Spawn env file writers first — they write once and exit
    let mavlink_env = Arc::new(MavlinkEnvWriter::new(Arc::clone(&ctx)));
    handles.extend(mavlink_env.run());

    let camera_env = Arc::new(CameraEnvWriter::new(Arc::clone(&ctx)));
    handles.extend(camera_env.run());

    let fluentbit_env = Arc::new(FluentbitEnvWriter::new(Arc::clone(&ctx)));
    handles.extend(fluentbit_env.run());

    let sensor_manager = Arc::new(SensorManager::new(
        Arc::clone(&ctx),
        cancel.clone(),
        &ctx.config.sensors,
        &ctx.config.general.interface,
    ));
    handles.extend(sensor_manager.run());

    let drone_component = Arc::new(DroneComponent::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(drone_component.run());

    let sniffer = Arc::new(MavlinkSniffer::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(sniffer.run());

    let telemetry = Arc::new(TelemetryReporter::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(telemetry.run());

    // MQTT service — only started when mqtt.enabled is true
    if ctx.config.mqtt.enabled {
        match MqttTransport::new(&ctx.config, cancel.clone()) {
            Ok(transport) => {
                let transport = Arc::new(transport);

                info!(
                    host = %ctx.config.mqtt.host,
                    port = ctx.config.mqtt.port,
                    node_id = %transport.node_id(),
                    "MQTT transport initialized"
                );

                // Create command processor BEFORE spawning the event loop so its
                // broadcast receiver exists when the first ConnAck/Publish arrives.
                // With clean_session=false the broker may replay queued messages
                // immediately on connect; without an active receiver those
                // broadcasts would be silently dropped.
                let mut processor = CommandProcessor::new(Arc::clone(&transport), cancel.clone());
                processor.register_commands(&ctx);
                processor.subscribe_commands().await;
                handles.extend(Arc::new(processor).run());

                // Create status publisher BEFORE transport.run() so its
                // broadcast receiver is ready for the first ConnAck.
                let status_publisher = Arc::new(StatusPublisher::new(
                    Arc::clone(&transport),
                    cancel.clone(),
                    ctx.config.general.interface.clone(),
                ));
                handles.extend(status_publisher.run());

                // Create restart completion publisher BEFORE transport.run() so
                // its broadcast receiver is registered before the first ConnAck;
                // otherwise the deferred Completed publish for a self-restart
                // can be silently dropped when MQTT connects in the gap between
                // transport.run() and this constructor.
                let restart_completion = Arc::new(RestartCompletionPublisher::new(
                    Arc::clone(&transport),
                    std::path::PathBuf::from(&ctx.config.general.env_dir),
                    cancel.clone(),
                ));
                handles.extend(restart_completion.run());

                // Run transport after all subscribers have been registered
                handles.extend(Arc::clone(&transport).run());

                // Run telemetry publisher after transport is initialized
                let mqtt_telemetry = Arc::new(TelemetryPublisher::new(
                    Arc::clone(&transport),
                    Arc::clone(&ctx),
                    Duration::from_secs_f64(ctx.config.mqtt.telemetry_interval_s),
                    cancel.clone(),
                ));
                handles.extend(mqtt_telemetry.run());
            }
            Err(e) => {
                warn!(error = %e, "failed to initialize MQTT transport, MQTT disabled");
            }
        }
    }

    // Wait for a shutdown signal (tasks run until canceled)
    cancel.cancelled().await;
    info!("shutdown initiated, waiting for tasks to complete");

    // Wait for all tasks to finish
    for handle in handles {
        let _ = handle.await;
    }
    info!("unitctl shutdown complete");
}

/// Wait for a shutdown signal (SIGINT or SIGTERM).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let ctrl_c = tokio::signal::ctrl_c();
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to register Ctrl+C handler");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use unitctl::config::tests::test_config;

    fn test_config_with_port(port: u16) -> unitctl::config::Config {
        let mut config = test_config();
        config.mavlink.host = "127.0.0.1".to_string();
        config.mavlink.local_mavlink_port = port;
        config
    }

    #[tokio::test]
    async fn test_sensor_manager_integration_spawns_and_stops() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;

        use async_trait::async_trait;

        use unitctl::sensors::Sensor;
        use unitctl::Task;

        struct TestSensor {
            started: Arc<AtomicBool>,
            stopped: Arc<AtomicBool>,
        }

        #[async_trait]
        impl Sensor for TestSensor {
            fn name(&self) -> &str {
                "test_sensor"
            }
            async fn run(&self, _ctx: Arc<Context>, cancel: CancellationToken) {
                self.started.store(true, Ordering::SeqCst);
                cancel.cancelled().await;
                self.stopped.store(true, Ordering::SeqCst);
            }
        }

        let config = test_config();
        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let started = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));

        // Build a SensorManager with a test sensor directly
        let manager = Arc::new(SensorManager {
            ctx: Arc::clone(&ctx),
            cancel: cancel.clone(),
            sensors: Mutex::new(Some(vec![Box::new(TestSensor {
                started: Arc::clone(&started),
                stopped: Arc::clone(&stopped),
            })])),
        });
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 1);

        // Spawn sensors — same pattern as main.rs wiring
        manager.run();

        // Verify sensor task started
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(started.load(Ordering::SeqCst), "sensor should have started");
        assert!(
            !stopped.load(Ordering::SeqCst),
            "sensor should still be running"
        );

        // Cancel and verify graceful shutdown
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            stopped.load(Ordering::SeqCst),
            "sensor should have stopped after cancel"
        );
    }

    #[tokio::test]
    async fn test_sensor_manager_from_config_integration() {
        // Verify SensorManager can be created from config and spawned,
        // matching the exact pattern used in main.rs
        use unitctl::Task;

        let config = test_config();
        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let sensor_manager = Arc::new(SensorManager::new(
            Arc::clone(&ctx),
            cancel.clone(),
            &ctx.config.sensors,
            &ctx.config.general.interface,
        ));
        // Default config has all 3 sensors enabled
        assert_eq!(
            sensor_manager
                .sensors
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .len(),
            3
        );

        // Spawn all — should not panic
        sensor_manager.run();

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Clean shutdown
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn test_integration_heartbeat_exchange_with_mock_server() {
        use ::mavlink::ardupilotmega::*;
        use ::mavlink::peek_reader::PeekReader;
        use ::mavlink::MavHeader;

        // Bind a TCP listener on a random port to act as mock mavlink-routerd
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Create config and context pointing to the mock server
        let config = test_config_with_port(port);
        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        // Pre-add an FC system so the drone heartbeat loop starts immediately.
        // In production, the sniffer discovers the FC first; here we simulate that.
        ctx.add_system(1).await;

        // Spawn drone component (connects as tcpout client)
        let component = Arc::new(DroneComponent::new(Arc::clone(&ctx), cancel.clone()));
        let c = Arc::clone(&component);
        tokio::spawn(async move {
            c.connect().await;
        });

        // Spawn drone heartbeat loop (enqueues heartbeats)
        let hb_cancel = cancel.clone();
        let hb_ctx = Arc::clone(&ctx);
        let sysid = ctx.config.mavlink.self_sysid;
        let compid = ctx.config.mavlink.self_compid;
        tokio::spawn(async move {
            unitctl::mavlink::heartbeat_loop(&hb_cancel, sysid, compid, hb_ctx).await;
        });

        // Accept the drone's connection in a blocking thread
        let mock_result = tokio::task::spawn_blocking(move || {
            listener.set_nonblocking(false).unwrap();
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();

            // Read a MAVLink v2 heartbeat from the drone
            let mut reader = PeekReader::new(&stream);
            let (header, msg): (MavHeader, MavMessage) =
                ::mavlink::read_v2_msg(&mut reader).expect("failed to read MAVLink message");

            // Send an FC heartbeat back to the drone
            let fc_header = MavHeader {
                system_id: 1,
                component_id: 1,
                sequence: 0,
            };
            let fc_hb = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
                custom_mode: 0,
                mavtype: MavType::MAV_TYPE_QUADROTOR,
                autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
                base_mode: MavModeFlag::empty(),
                system_status: MavState::MAV_STATE_ACTIVE,
                mavlink_version: 3,
            });
            let mut writer = &stream;
            ::mavlink::write_v2_msg::<MavMessage, _>(&mut writer, fc_header, &fc_hb)
                .expect("failed to write MAVLink message");

            (header, msg)
        })
        .await
        .expect("mock server thread panicked");

        let (header, msg) = mock_result;

        // Verify the drone sent a proper heartbeat
        assert_eq!(header.system_id, 1); // default self_sysid
        assert_eq!(header.component_id, 10); // default self_compid
        match msg {
            MavMessage::HEARTBEAT(data) => {
                assert_eq!(data.mavtype, MavType::MAV_TYPE_ONBOARD_CONTROLLER);
                assert_eq!(data.autopilot, MavAutopilot::MAV_AUTOPILOT_INVALID);
                assert_eq!(data.system_status, MavState::MAV_STATE_ACTIVE);
                assert_eq!(data.mavlink_version, 3);
            }
            _ => panic!("expected HEARTBEAT message from drone"),
        }

        // Verify message routing: simulate sniffer behavior by manually
        // broadcasting an FC heartbeat through the context
        let mut rx = ctx.subscribe_broadcast();
        let fc_header = ::mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let fc_msg = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: MavType::MAV_TYPE_QUADROTOR,
            autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: MavModeFlag::empty(),
            system_status: MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        });
        ctx.tx_broadcast
            .send((fc_header, fc_msg))
            .expect("broadcast send failed");

        // Verify the message was routed
        let (rx_header, rx_msg) = rx.recv().await.expect("broadcast recv failed");
        assert_eq!(rx_header.system_id, 1);
        assert!(matches!(rx_msg, MavMessage::HEARTBEAT(_)));

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_modem_discovery_background_task_sets_context() {
        // Simulates the main.rs startup pattern: a background task discovers
        // the modem and stores it in Context via set_modem().
        use unitctl::services::modem_access::{ModemAccess, ModemError};

        struct FakeModem;

        #[async_trait::async_trait]
        impl ModemAccess for FakeModem {
            async fn model(&self) -> Result<String, ModemError> {
                Ok("TEST_MODEM".to_string())
            }
            async fn command(&self, _cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
                Ok("OK".to_string())
            }
        }

        let ctx = Context::new(test_config());
        assert!(ctx.get_modem().await.is_none());

        // Simulate the background modem discovery task from main.rs
        let modem_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            let modem: Arc<dyn ModemAccess> = Arc::new(FakeModem);
            modem_ctx.set_modem(modem).await;
        });

        // Wait for the background task to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify modem is now available in context (same path sensors use)
        let modem = ctx.get_modem().await;
        assert!(modem.is_some(), "modem should be set by background task");
        let model = modem.unwrap().model().await.unwrap();
        assert_eq!(model, "TEST_MODEM");
    }

    #[tokio::test]
    async fn test_modem_discovery_failure_does_not_block_startup() {
        // If modem discovery fails, the rest of the startup should proceed.
        // Context modem remains None, and sensors that need it will keep waiting.
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Cancel immediately to simulate discovery failure (cancelled during retry)
        cancel.cancel();

        let modem_ctx = Arc::clone(&ctx);
        let lte = unitctl::config::LteSensorConfig {
            enabled: true,
            interval_s: None,
            neighbor_expiry_s: 30.0,
            modem_type: "dbus".to_string(),
        };
        let handle = tokio::spawn(async move {
            match unitctl::services::modem_access::ModemAccessService::start(&lte, &cancel).await {
                Ok(service) => {
                    modem_ctx.set_modem(service).await;
                }
                Err(_) => {
                    // Expected — discovery was cancelled
                }
            }
        });

        // Background task should complete without blocking
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("background task should complete")
            .expect("background task should not panic");

        // Context modem should still be None
        assert!(
            ctx.get_modem().await.is_none(),
            "modem should remain None when discovery fails"
        );
    }
}
