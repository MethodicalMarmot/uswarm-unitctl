mod config;
mod context;
mod mavlink;
mod sensors;

use std::sync::Arc;

use crate::mavlink::drone_component::DroneComponent;
use crate::mavlink::sniffer_component::MavlinkSniffer;
use crate::mavlink::telemetry_reporter::TelemetryReporter;
use clap::Parser;
use config::Cli;
use context::Context;
use sensors::SensorManager;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;

pub trait Task: Send + Sync {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>>;
}

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
    let filter = if debug_enabled {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();

    info!("unitctl starting");

    info!(
        host = %config.mavlink.host,
        port = config.mavlink.port,
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

    let sensor_manager = Arc::new(SensorManager::new(
        Arc::clone(&ctx),
        cancel.clone(),
        &ctx.config.sensors,
    ));
    handles.extend(sensor_manager.run());

    let drone_component = Arc::new(DroneComponent::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(drone_component.run());

    let sniffer = Arc::new(MavlinkSniffer::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(sniffer.run());

    let telemetry = Arc::new(TelemetryReporter::new(Arc::clone(&ctx), cancel.clone()));
    handles.extend(telemetry.run());

    // Wait for shutdown signal (tasks run until cancelled)
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
    use crate::config::tests::test_config;
    use std::time::Duration;

    fn test_config_with_port(port: u16) -> crate::config::Config {
        let mut config = test_config();
        config.mavlink.host = "127.0.0.1".to_string();
        config.mavlink.port = port;
        config
    }

    #[tokio::test]
    async fn test_sensor_manager_integration_spawns_and_stops() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;

        use async_trait::async_trait;

        use crate::sensors::Sensor;
        use crate::Task;

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
            sensors: Mutex::new(vec![Box::new(TestSensor {
                started: Arc::clone(&started),
                stopped: Arc::clone(&stopped),
            })]),
        });
        assert_eq!(manager.sensors.lock().unwrap().len(), 1);

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
        use crate::Task;

        let config = test_config();
        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let sensor_manager = Arc::new(SensorManager::new(
            Arc::clone(&ctx),
            cancel.clone(),
            &ctx.config.sensors,
        ));
        // Default config has all 3 sensors enabled
        assert_eq!(sensor_manager.sensors.lock().unwrap().len(), 3);

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
            mavlink::heartbeat_loop(&hb_cancel, sysid, compid, hb_ctx).await;
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
}
