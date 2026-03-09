use std::sync::Arc;
use std::time::Duration;

use mavlink::ardupilotmega::*;
use mavlink::error::MessageReadError;
use mavlink::{Connection, MavConnection, MavHeader};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, Instrument};

use super::{backoff_or_cancel, heartbeat_loop, mavlink_connect};
use crate::context::Context;
use crate::Task;

const TRACING_SPAN_NAME: &str = "sniffer-component";

pub struct MavlinkSniffer {
    ctx: Arc<Context>,
    cancel: CancellationToken,
}

impl MavlinkSniffer {
    pub fn new(ctx: Arc<Context>, cancel: CancellationToken) -> Self {
        Self { ctx, cancel }
    }

    /// Main receive loop for the MavlinkSniffer.
    ///
    /// Connects to mavlink-routerd via TCP using the sniffer system ID (default 199),
    /// continuously receives MAVLink messages, discovers flight controller system IDs
    /// from heartbeats, and broadcasts all received messages on the broadcast channel.
    pub async fn connect(&self) {
        let conn_addr = self.ctx.config.mavlink.connection_string();

        loop {
            if self.cancel.is_cancelled() {
                info!("sniffer: shutdown requested");
                return;
            }

            if let Some(conn) = mavlink_connect(&self.cancel, &conn_addr)
                .instrument(info_span!(TRACING_SPAN_NAME))
                .await
            {
                // Wrap connection in Arc so recv can be called from spawn_blocking
                let conn: Arc<Connection<MavMessage>> = Arc::new(conn);

                // Receive loop: read messages and broadcast them
                let disconnected = self.recv_loop(&conn).await;

                if disconnected {
                    error!("sniffer connection lost, reconnecting in 1s");
                    backoff_or_cancel(&self.cancel).await;
                }
            }
        }
    }

    /// Inner receive loop: reads messages from the connection, handles heartbeats
    /// for system discovery, and broadcasts all messages.
    ///
    /// Uses spawn_blocking for conn.recv() so the async runtime is not blocked
    /// by the synchronous MAVLink read, and cancellation can be observed promptly.
    ///
    /// Returns `true` if the connection was lost, `false` if shutdown was requested.
    async fn recv_loop(&self, conn: &Arc<Connection<MavMessage>>) -> bool {
        loop {
            let conn_clone = Arc::clone(conn);
            let recv_handle = tokio::task::spawn_blocking(move || conn_clone.recv());

            // Pin the handle so we can await it after cancellation instead of
            // detaching the blocking task (which would keep the connection Arc alive).
            tokio::pin!(recv_handle);

            let recv_result = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => None,
                result = &mut recv_handle => result.ok(),
            };

            let recv_result = match recv_result {
                Some(result) => result,
                None => {
                    info!("sniffer: shutdown in recv loop");
                    // Wait briefly for the blocking recv task to finish rather
                    // than detaching it. This prevents orphaned blocking tasks
                    // from holding the connection Arc. Bounded by the protocol's
                    // read timeout (tcpout: ~100ms in the mavlink crate).
                    let _ =
                        tokio::time::timeout(Duration::from_millis(500), &mut recv_handle).await;
                    return false;
                }
            };

            match recv_result {
                Ok((header, msg)) => {
                    // Handle HEARTBEAT messages for system discovery
                    if let MavMessage::HEARTBEAT(_) = &msg {
                        self.handle_heartbeat(&header).await;
                    }

                    // Broadcast the message to all subscribers
                    self.broadcast_message(header, msg);
                }
                Err(e) => {
                    if Self::is_transient_io_error(&e) {
                        // WouldBlock or timeout — yield briefly and retry
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }
                    error!(error = %e, "sniffer recv error");
                    return true;
                }
            }
        }
    }

    /// Check if a MessageReadError is a transient I/O error (WouldBlock or TimedOut)
    /// that should be retried rather than treated as a connection failure.
    fn is_transient_io_error(e: &MessageReadError) -> bool {
        match e {
            MessageReadError::Io(io_err) => matches!(
                io_err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            _ => false,
        }
    }

    /// Handle a received HEARTBEAT message by discovering the sender's system ID.
    ///
    /// Filters out heartbeats from known non-FC components (sniffer, base station)
    /// whose system IDs are >= FC_SYSID_THRESHOLD. Does NOT filter self_sysid
    /// because the drone component waits for FC discovery before sending heartbeats,
    /// and the real FC may use the same system ID as self_sysid (common: both are 1).
    /// This matches the Python sniffer which unconditionally adds all heartbeat sysids.
    async fn handle_heartbeat(&self, header: &MavHeader) {
        let sysid = header.system_id;
        let compid = header.component_id;

        // Skip heartbeats from known non-FC components (sniffer, base station).
        // Note: self_sysid is NOT filtered here because the real FC may share that ID.
        // Self-discovery is prevented by drone::heartbeat_loop waiting for FC first.
        let cfg = &self.ctx.config.mavlink;
        if sysid == cfg.sniffer_sysid || sysid == cfg.bs_sysid || sysid == cfg.gcs_sysid {
            debug!(
                system_id = sysid,
                component_id = compid,
                "sniffer: ignoring heartbeat from internal component"
            );
            return;
        }

        // Atomically insert and check if new in a single lock acquisition
        if self.ctx.add_system(sysid).await {
            info!(
                system_id = sysid,
                component_id = compid,
                "sniffer: discovered new system"
            );
        } else {
            debug!(
                system_id = sysid,
                component_id = compid,
                "sniffer: heartbeat from known system"
            );
        }
    }

    /// Broadcast a received message to all subscribers via the broadcast channel.
    ///
    /// If no subscribers are listening, the message is silently dropped.
    fn broadcast_message(&self, header: MavHeader, msg: MavMessage) {
        match self.ctx.tx_broadcast.send((header, msg)) {
            Ok(n) => {
                debug!(subscribers = n, "sniffer: broadcast message");
            }
            Err(_) => {
                // No active subscribers — this is normal during startup
                debug!("sniffer: no broadcast subscribers, message dropped");
            }
        }
    }
}

impl Task for MavlinkSniffer {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let component = Arc::clone(&self);
        let sniffer_handle = tokio::spawn(async move {
            component.connect().await;
        });
        info!("sniffer started");

        // Spawn drone heartbeat loop
        let cancel = self.cancel.clone();
        let ctx = Arc::clone(&self.ctx);
        let sysid = self.ctx.config.mavlink.sniffer_sysid;
        let compid = self.ctx.config.mavlink.self_compid;
        let sniffer_hb_handle = tokio::spawn(
            async move {
                heartbeat_loop(&cancel, sysid, compid, ctx).await;
            }
            .instrument(info_span!(TRACING_SPAN_NAME)),
        );
        info!("sniffer heartbeat loop started");

        vec![sniffer_handle, sniffer_hb_handle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;

    fn make_heartbeat_msg(mavtype: MavType) -> MavMessage {
        MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype,
            autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: MavModeFlag::empty(),
            system_status: MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        })
    }

    fn make_header(sysid: u8, compid: u8) -> MavHeader {
        MavHeader {
            system_id: sysid,
            component_id: compid,
            sequence: 0,
        }
    }

    fn make_sniffer(ctx: Arc<Context>, cancel: CancellationToken) -> Arc<MavlinkSniffer> {
        Arc::new(MavlinkSniffer::new(ctx, cancel))
    }

    // -- System ID discovery tests --
    // Note: default self_sysid=1, sniffer_sysid=199, bs_sysid=200 are filtered.
    // Use external system IDs (e.g. 2, 3) for discovery tests.

    #[tokio::test]
    async fn test_handle_heartbeat_discovers_new_system() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        let header = make_header(2, 1);

        assert!(!ctx.has_system(2).await);

        sniffer.handle_heartbeat(&header).await;

        assert!(ctx.has_system(2).await);
    }

    #[tokio::test]
    async fn test_handle_heartbeat_filters_internal_sysids() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());

        // sniffer_sysid (199) and bs_sysid (200) should be filtered
        sniffer.handle_heartbeat(&make_header(199, 1)).await;
        sniffer.handle_heartbeat(&make_header(200, 1)).await;

        assert!(!ctx.has_system(199).await);
        assert!(!ctx.has_system(200).await);

        // self_sysid (1) should NOT be filtered — real FC may use same sysid
        sniffer.handle_heartbeat(&make_header(1, 1)).await;
        assert!(ctx.has_system(1).await);
    }

    #[tokio::test]
    async fn test_handle_heartbeat_multiple_systems() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());

        // Discover system 2
        sniffer.handle_heartbeat(&make_header(2, 1)).await;
        assert!(ctx.has_system(2).await);

        // Discover system 3
        sniffer.handle_heartbeat(&make_header(3, 1)).await;
        assert!(ctx.has_system(3).await);

        // Both should be present
        let systems = ctx.available_systems.read().await;
        assert_eq!(systems.len(), 2);
        assert!(systems.contains(&2));
        assert!(systems.contains(&3));
    }

    #[tokio::test]
    async fn test_handle_heartbeat_idempotent() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        let header = make_header(2, 1);

        sniffer.handle_heartbeat(&header).await;
        sniffer.handle_heartbeat(&header).await;
        sniffer.handle_heartbeat(&header).await;

        let systems = ctx.available_systems.read().await;
        assert_eq!(systems.len(), 1);
    }

    #[tokio::test]
    async fn test_handle_heartbeat_different_components_same_system() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());

        // Same system, different components
        sniffer.handle_heartbeat(&make_header(2, 1)).await;
        sniffer.handle_heartbeat(&make_header(2, 190)).await;

        // Should only have one system entry
        let systems = ctx.available_systems.read().await;
        assert_eq!(systems.len(), 1);
        assert!(systems.contains(&2));
    }

    // -- Message broadcast tests --

    #[tokio::test]
    async fn test_broadcast_message_to_subscriber() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        let mut rx = ctx.subscribe_broadcast();

        let header = make_header(1, 1);
        let msg = make_heartbeat_msg(MavType::MAV_TYPE_QUADROTOR);

        sniffer.broadcast_message(header, msg);

        let (rx_header, rx_msg) = rx.recv().await.unwrap();
        assert_eq!(rx_header.system_id, 1);
        assert_eq!(rx_header.component_id, 1);
        match rx_msg {
            MavMessage::HEARTBEAT(data) => {
                assert_eq!(data.mavtype, MavType::MAV_TYPE_QUADROTOR);
            }
            _ => panic!("expected HEARTBEAT"),
        }
    }

    #[tokio::test]
    async fn test_broadcast_message_multiple_subscribers() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        let mut rx1 = ctx.subscribe_broadcast();
        let mut rx2 = ctx.subscribe_broadcast();

        let header = make_header(42, 1);
        let msg = make_heartbeat_msg(MavType::MAV_TYPE_QUADROTOR);

        sniffer.broadcast_message(header, msg);

        let (h1, _) = rx1.recv().await.unwrap();
        let (h2, _) = rx2.recv().await.unwrap();
        assert_eq!(h1.system_id, 42);
        assert_eq!(h2.system_id, 42);
    }

    #[tokio::test]
    async fn test_broadcast_message_no_subscribers_does_not_panic() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        // No subscribers - should not panic
        let header = make_header(1, 1);
        let msg = make_heartbeat_msg(MavType::MAV_TYPE_QUADROTOR);
        sniffer.broadcast_message(header, msg);
    }

    #[tokio::test]
    async fn test_broadcast_non_heartbeat_message() {
        let ctx = Context::new(test_config());
        let sniffer = make_sniffer(Arc::clone(&ctx), CancellationToken::new());
        let mut rx = ctx.subscribe_broadcast();

        let header = make_header(1, 1);
        let msg = MavMessage::SYS_STATUS(SYS_STATUS_DATA {
            onboard_control_sensors_present: MavSysStatusSensor::empty(),
            onboard_control_sensors_enabled: MavSysStatusSensor::empty(),
            onboard_control_sensors_health: MavSysStatusSensor::empty(),
            load: 500,
            voltage_battery: 12000,
            current_battery: 1000,
            battery_remaining: 75,
            drop_rate_comm: 0,
            errors_comm: 0,
            errors_count1: 0,
            errors_count2: 0,
            errors_count3: 0,
            errors_count4: 0,
        });

        sniffer.broadcast_message(header, msg);

        let (rx_header, rx_msg) = rx.recv().await.unwrap();
        assert_eq!(rx_header.system_id, 1);
        match rx_msg {
            MavMessage::SYS_STATUS(data) => {
                assert_eq!(data.battery_remaining, 75);
                assert_eq!(data.voltage_battery, 12000);
            }
            _ => panic!("expected SYS_STATUS"),
        }
    }

    // -- Heartbeat loop tests --

    #[tokio::test]
    async fn test_heartbeat_loop_waits_for_fc() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        let sysid = ctx.config.mavlink.sniffer_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx_clone).await;
        });

        // Heartbeat loop should be waiting for FC - no messages sent yet
        let mut rx = ctx.take_outgoing_rx().await.unwrap();
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            result.is_err(),
            "should not send heartbeats before FC discovered"
        );

        // Cancel and clean up
        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_heartbeat_loop_starts_after_fc_discovery() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();
        let mut rx = ctx.take_outgoing_rx().await.unwrap();

        // Pre-add a FC system (ID < 200)
        ctx.add_system(1).await;

        let sysid = ctx.config.mavlink.sniffer_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx_clone).await;
        });

        // Should receive a heartbeat now
        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for sniffer heartbeat")
            .expect("channel closed");

        // Should use sniffer_sysid (default 199)
        assert_eq!(frame.0.system_id, 199);
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

        let sysid = ctx.config.mavlink.sniffer_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx).await;
        });

        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("heartbeat loop didn't stop on cancel")
            .unwrap();
    }

    #[tokio::test]
    async fn test_heartbeat_loop_ignores_non_fc_systems() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Add system >= 200 (not a FC)
        ctx.add_system(200).await;
        ctx.add_system(255).await;

        let mut rx = ctx.take_outgoing_rx().await.unwrap();
        let sysid = ctx.config.mavlink.sniffer_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx_clone).await;
        });

        // Should NOT send heartbeats since no FC (id < 200) discovered
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            result.is_err(),
            "should not send heartbeats without FC (id < 200)"
        );

        cancel.cancel();
        handle.await.unwrap();
    }
}
