use std::sync::Arc;
use std::time::Duration;

use mavlink::ardupilotmega::*;
use mavlink::{MavConnection, MavHeader, MavlinkVersion};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::backoff_or_cancel;
use crate::context::{Context, MavFrame};

/// Build a MAVLink v2 HEARTBEAT message for the onboard controller.
///
/// Uses MAV_TYPE_ONBOARD_CONTROLLER and MAV_AUTOPILOT_INVALID, matching
/// the Python MavlinkDroneComponent behavior.
pub fn build_heartbeat(sysid: u8, compid: u8) -> MavFrame {
    let header = MavHeader {
        system_id: sysid,
        component_id: compid,
        sequence: 0,
    };
    let msg = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
        custom_mode: 0,
        mavtype: MavType::MAV_TYPE_ONBOARD_CONTROLLER,
        autopilot: MavAutopilot::MAV_AUTOPILOT_INVALID,
        base_mode: MavModeFlag::empty(),
        system_status: MavState::MAV_STATE_ACTIVE,
        mavlink_version: 3,
    });
    (header, msg)
}

/// Drain all pending messages from the outgoing channel without blocking.
/// Returns the drained messages in FIFO order.
pub fn drain_queue(rx: &mut mpsc::Receiver<MavFrame>) -> Vec<MavFrame> {
    let mut messages = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        messages.push(frame);
    }
    messages
}

/// Main run loop for the MavlinkDroneComponent.
///
/// Connects to mavlink-routerd via TCP, drains the outgoing message queue,
/// and sends messages over the wire. Reconnects with 1s backoff on failure.
pub async fn run(ctx: Arc<Context>, cancel: CancellationToken) {
    let mut outgoing_rx = match ctx.take_outgoing_rx().await {
        Some(rx) => rx,
        None => {
            error!("drone component: outgoing rx already taken");
            return;
        }
    };

    let iteration_period = Duration::from_millis(ctx.config.mavlink.iteration_period_ms);
    let conn_addr = ctx.config.mavlink.connection_string();

    loop {
        if cancel.is_cancelled() {
            info!("drone component: shutdown requested");
            return;
        }

        info!(address = %conn_addr, "connecting to mavlink-routerd");

        // Blocking TCP connect via spawn_blocking
        let addr = conn_addr.clone();
        let conn_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                info!("drone component: shutdown during connect");
                return;
            }
            result = tokio::task::spawn_blocking(move || {
                mavlink::connect::<MavMessage>(&addr)
            }) => result,
        };

        let mut conn = match conn_result {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                error!(error = %e, "connection failed, retrying in 1s");
                backoff_or_cancel(&cancel).await;
                continue;
            }
            Err(e) => {
                error!(error = %e, "connect task panicked");
                backoff_or_cancel(&cancel).await;
                continue;
            }
        };

        conn.set_protocol_version(MavlinkVersion::V2);
        info!("mavlink connection established (MAVLink v2)");

        // Drain loop: pull messages from outgoing queue and send them
        let disconnected = send_loop(&mut outgoing_rx, &*conn, iteration_period, &cancel).await;

        if disconnected {
            error!("connection lost, reconnecting in 1s");
            backoff_or_cancel(&cancel).await;
        }
    }
}

/// Heartbeat loop: enqueues a heartbeat message every 1 second via the outgoing channel.
///
/// Waits for flight controller discovery before starting, matching the Python
/// MavlinkDroneComponent.heartbeat() which calls get_fc_system_id() first.
/// Runs as a separate task alongside the main `run` loop.
pub async fn heartbeat_loop(ctx: Arc<Context>, cancel: CancellationToken) {
    let sysid = ctx.config.mavlink.self_sysid;
    let compid = ctx.config.mavlink.self_compid;

    // Wait for at least one FC system to be discovered before starting heartbeats.
    // This prevents self-discovery: without this wait, mavlink-routerd would route
    // our own heartbeats back to the sniffer, which would mistakenly register them
    // as an FC discovery (since self_sysid is typically < 200).
    info!("drone heartbeat: waiting for flight controller discovery");
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                info!("drone heartbeat: shutdown while waiting for FC");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                let systems = ctx.available_systems.read().await;
                if systems.iter().any(|&id| super::is_fc_sysid(id)) {
                    info!("drone heartbeat: flight controller found, starting heartbeats");
                    break;
                }
            }
        }
    }

    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                info!("heartbeat loop: shutdown");
                return;
            }
            _ = interval.tick() => {
                let frame = build_heartbeat(sysid, compid);
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        info!("heartbeat loop: shutdown while sending");
                        return;
                    }
                    result = ctx.tx_outgoing.send(frame) => {
                        if result.is_err() {
                            error!("heartbeat loop: outgoing channel closed");
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Inner send loop: periodically drains messages from the channel and sends them.
///
/// Returns `true` if the connection was lost (send error),
/// `false` if shutdown was requested or the channel closed.
async fn send_loop(
    rx: &mut mpsc::Receiver<MavFrame>,
    conn: &(dyn MavConnection<MavMessage> + Send + Sync),
    period: Duration,
    cancel: &CancellationToken,
) -> bool {
    let mut interval = tokio::time::interval(period);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                info!("drone component: shutdown in send loop");
                return false;
            }
            _ = interval.tick() => {
                let messages = drain_queue(rx);
                for (header, msg) in messages {
                    if let Err(e) = conn.send(&header, &msg) {
                        error!(error = %e, "send failed");
                        return true;
                    }
                }
            }
        }
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

    // -- Heartbeat construction tests --

    #[test]
    fn test_build_heartbeat_default_ids() {
        let (header, msg) = build_heartbeat(1, 10);
        assert_eq!(header.system_id, 1);
        assert_eq!(header.component_id, 10);
        assert_eq!(header.sequence, 0);

        match msg {
            MavMessage::HEARTBEAT(data) => {
                assert_eq!(data.mavtype, MavType::MAV_TYPE_ONBOARD_CONTROLLER);
                assert_eq!(data.autopilot, MavAutopilot::MAV_AUTOPILOT_INVALID);
                assert_eq!(data.base_mode, MavModeFlag::empty());
                assert_eq!(data.system_status, MavState::MAV_STATE_ACTIVE);
                assert_eq!(data.custom_mode, 0);
                assert_eq!(data.mavlink_version, 3);
            }
            _ => panic!("expected HEARTBEAT message"),
        }
    }

    #[test]
    fn test_build_heartbeat_custom_ids() {
        let (header, msg) = build_heartbeat(42, 99);
        assert_eq!(header.system_id, 42);
        assert_eq!(header.component_id, 99);

        match msg {
            MavMessage::HEARTBEAT(data) => {
                assert_eq!(data.mavtype, MavType::MAV_TYPE_ONBOARD_CONTROLLER);
            }
            _ => panic!("expected HEARTBEAT message"),
        }
    }

    // -- Message queue drain tests --

    #[tokio::test]
    async fn test_drain_queue_empty() {
        let (_tx, mut rx) = mpsc::channel::<MavFrame>(10);
        let drained = drain_queue(&mut rx);
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_drain_queue_single_message() {
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);
        let frame = build_heartbeat(1, 10);
        tx.send(frame).await.unwrap();

        let drained = drain_queue(&mut rx);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0.system_id, 1);
        assert_eq!(drained[0].0.component_id, 10);
    }

    #[tokio::test]
    async fn test_drain_queue_multiple_preserves_order() {
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);

        // Queue 5 messages with different sequence numbers
        for seq in 0u8..5 {
            let header = MavHeader {
                system_id: 1,
                component_id: 10,
                sequence: seq,
            };
            let msg = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
                custom_mode: 0,
                mavtype: MavType::MAV_TYPE_ONBOARD_CONTROLLER,
                autopilot: MavAutopilot::MAV_AUTOPILOT_INVALID,
                base_mode: MavModeFlag::empty(),
                system_status: MavState::MAV_STATE_ACTIVE,
                mavlink_version: 3,
            });
            tx.send((header, msg)).await.unwrap();
        }

        let drained = drain_queue(&mut rx);
        assert_eq!(drained.len(), 5);
        for (i, frame) in drained.iter().enumerate() {
            assert_eq!(frame.0.sequence, i as u8, "message order not preserved");
        }
    }

    #[tokio::test]
    async fn test_drain_queue_nonblocking() {
        let (_tx, mut rx) = mpsc::channel::<MavFrame>(10);
        // drain_queue should return immediately even when empty
        let start = tokio::time::Instant::now();
        let drained = drain_queue(&mut rx);
        let elapsed = start.elapsed();
        assert!(drained.is_empty());
        assert!(elapsed < Duration::from_millis(10), "drain_queue blocked");
    }

    #[tokio::test]
    async fn test_drain_queue_leaves_channel_empty() {
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);
        tx.send(build_heartbeat(1, 10)).await.unwrap();
        tx.send(build_heartbeat(2, 20)).await.unwrap();

        let first = drain_queue(&mut rx);
        assert_eq!(first.len(), 2);

        let second = drain_queue(&mut rx);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn test_heartbeat_loop_waits_for_fc() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Take the outgoing rx so we can check what gets queued
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(ctx_clone, cancel_clone).await;
        });

        // No FC discovered yet - should not send heartbeats
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            result.is_err(),
            "should not send heartbeats before FC discovered"
        );

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_heartbeat_loop_starts_after_fc_discovery() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Take the outgoing rx so we can check what gets queued
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        // Pre-add an FC system (ID < 200)
        ctx.add_system(1).await;

        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(ctx_clone, cancel_clone).await;
        });

        // Wait for at least one heartbeat to be enqueued
        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for heartbeat")
            .expect("channel closed");

        assert_eq!(frame.0.system_id, ctx.config.mavlink.self_sysid);
        assert_eq!(frame.0.component_id, ctx.config.mavlink.self_compid);
        match frame.1 {
            MavMessage::HEARTBEAT(data) => {
                assert_eq!(data.mavtype, MavType::MAV_TYPE_ONBOARD_CONTROLLER);
            }
            _ => panic!("expected HEARTBEAT"),
        }

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_heartbeat_loop_stops_on_cancel() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();
        let _rx = ctx.take_outgoing_rx().await.unwrap();

        // Pre-add FC so the loop starts
        ctx.add_system(1).await;

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            heartbeat_loop(ctx, cancel_clone).await;
        });

        // Cancel immediately
        cancel.cancel();

        // Should complete promptly
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("heartbeat loop didn't stop on cancel")
            .unwrap();
    }
}
