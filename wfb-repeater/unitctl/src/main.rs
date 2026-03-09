mod config;
mod context;
mod mavlink;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Cli;
use context::Context;

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

    // Spawn drone component (outgoing message sender)
    let drone_cancel = cancel.clone();
    let drone_ctx = Arc::clone(&ctx);
    let drone_handle = tokio::spawn(async move {
        mavlink::drone::run(drone_ctx, drone_cancel).await;
    });
    info!("drone component started");

    // Spawn drone heartbeat loop
    let drone_hb_cancel = cancel.clone();
    let drone_hb_ctx = Arc::clone(&ctx);
    let drone_hb_handle = tokio::spawn(async move {
        mavlink::drone::heartbeat_loop(drone_hb_ctx, drone_hb_cancel).await;
    });
    info!("drone heartbeat loop started");

    // Spawn sniffer (incoming message receiver)
    let sniffer_cancel = cancel.clone();
    let sniffer_ctx = Arc::clone(&ctx);
    let sniffer_handle = tokio::spawn(async move {
        mavlink::sniffer::run(sniffer_ctx, sniffer_cancel).await;
    });
    info!("sniffer started");

    // Spawn sniffer heartbeat loop
    let sniffer_hb_cancel = cancel.clone();
    let sniffer_hb_ctx = Arc::clone(&ctx);
    let sniffer_hb_handle = tokio::spawn(async move {
        mavlink::sniffer::heartbeat_loop(sniffer_hb_ctx, sniffer_hb_cancel).await;
    });
    info!("sniffer heartbeat loop started");

    // Wait for flight controller discovery
    info!("waiting for flight controller...");
    if wait_for_fc(&ctx, &cancel).await {
        let systems = ctx.available_systems.read().await;
        let fc_ids: Vec<u8> = systems
            .iter()
            .copied()
            .filter(|&id| mavlink::is_fc_sysid(id))
            .collect();
        info!(?fc_ids, "flight controller detected, system operational");
    } else {
        info!("shutdown requested before flight controller detected");
    }

    // Wait for shutdown signal (tasks run until cancelled)
    cancel.cancelled().await;
    info!("shutdown initiated, waiting for tasks to complete");

    // Wait for all tasks to finish
    let _ = tokio::join!(
        drone_handle,
        drone_hb_handle,
        sniffer_handle,
        sniffer_hb_handle,
    );
    info!("unitctl shutdown complete");
}

/// Wait for a flight controller to be discovered (system ID < 200).
///
/// Returns `true` if an FC was found, `false` if shutdown was requested first.
/// Matches Python's `get_fc_system_id()` behavior which waits for a heartbeat
/// from a system with ID < 200.
async fn wait_for_fc(ctx: &Arc<Context>, cancel: &CancellationToken) -> bool {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            _ = interval.tick() => {
                let systems = ctx.available_systems.read().await;
                if let Some(&fc_id) = systems.iter().find(|&&id| mavlink::is_fc_sysid(id)) {
                    info!(system_id = fc_id, "flight controller discovered");
                    return true;
                }
            }
        }
    }
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
    use crate::config::Config;

    fn test_config() -> Config {
        let toml_str = "[mavlink]\n";
        toml::from_str(toml_str).unwrap()
    }

    fn test_config_with_port(port: u16) -> Config {
        let toml_str = format!("[mavlink]\nhost = \"127.0.0.1\"\nport = {}\n", port);
        toml::from_str(&toml_str).unwrap()
    }

    #[tokio::test]
    async fn test_wait_for_fc_returns_true_when_fc_discovered() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Pre-add an FC system (ID < 200)
        ctx.add_system(1).await;

        let result = wait_for_fc(&ctx, &cancel).await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_wait_for_fc_returns_false_on_cancel() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        let ctx_clone = Arc::clone(&ctx);
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move { wait_for_fc(&ctx_clone, &cancel_clone).await });

        // Cancel after a short delay
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("wait_for_fc didn't stop on cancel")
            .unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_wait_for_fc_ignores_non_fc_systems() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Add non-FC systems (ID >= 200)
        ctx.add_system(200).await;
        ctx.add_system(255).await;

        let ctx_clone = Arc::clone(&ctx);
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move { wait_for_fc(&ctx_clone, &cancel_clone).await });

        // Should not return true for non-FC systems
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Now add a real FC
        ctx.add_system(1).await;

        let result = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("wait_for_fc didn't detect FC")
            .unwrap();
        assert!(result);
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
        let drone_ctx = Arc::clone(&ctx);
        let drone_cancel = cancel.clone();
        tokio::spawn(async move {
            crate::mavlink::drone::run(drone_ctx, drone_cancel).await;
        });

        // Spawn drone heartbeat loop (enqueues heartbeats)
        let hb_ctx = Arc::clone(&ctx);
        let hb_cancel = cancel.clone();
        tokio::spawn(async move {
            crate::mavlink::drone::heartbeat_loop(hb_ctx, hb_cancel).await;
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
