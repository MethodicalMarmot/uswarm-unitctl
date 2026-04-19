use std::sync::Arc;
use std::time::Duration;

use crate::mavlink::MavFrame;
use mavlink::ardupilotmega::*;
use mavlink::{Connection, MavConnection};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, info_span, Instrument};

use super::{backoff_or_cancel, heartbeat_loop, mavlink_connect, wait_for_fc};
use crate::context::Context;
use crate::Task;

const TRACING_SPAN_NAME: &str = "drone-component";

pub struct DroneComponent {
    ctx: Arc<Context>,
    cancel: CancellationToken,
}

impl DroneComponent {
    pub fn new(ctx: Arc<Context>, cancel: CancellationToken) -> Self {
        Self { ctx, cancel }
    }

    /// Main run loop for the MavlinkDroneComponent.
    ///
    /// Connects to mavlink-routerd via TCP, drains the outgoing message queue,
    /// and sends messages over the wire. Reconnects with 1s backoff on failure.
    pub async fn connect(&self) {
        let mut outgoing_rx = self.ctx.outgoing_rx.write().await;
        let conn_addr = self.ctx.config.mavlink.connection_string();

        loop {
            if self.cancel.is_cancelled() {
                info!("drone component: shutdown requested");
                return;
            }

            if let Some(conn) = mavlink_connect(&self.cancel, &conn_addr)
                .instrument(info_span!(TRACING_SPAN_NAME))
                .await
            {
                // Discard stale messages queued during the outage so we start
                // with fresh state on each new connection.
                let stale = self.drain_queue(&mut outgoing_rx);
                if !stale.is_empty() {
                    info!(
                        count = stale.len(),
                        "discarded stale messages from queue on reconnect"
                    );
                }

                // Drain loop: pull messages from outgoing queue and send them
                let disconnected = self.send_loop(&mut outgoing_rx, &conn).await;

                if disconnected {
                    error!("connection lost, reconnecting in 1s");
                    backoff_or_cancel(&self.cancel).await;
                }
            }
        }
    }

    /// Inner send loop: periodically drains messages from the channel and sends them.
    ///
    /// Returns `true` if the connection was lost (send error),
    /// `false` if shutdown was requested or the channel closed.
    async fn send_loop(
        &self,
        rx: &mut mpsc::Receiver<MavFrame>,
        conn: &Connection<MavMessage>,
    ) -> bool {
        let mut interval = tokio::time::interval(Duration::from_millis(
            self.ctx.config.mavlink.iteration_period_ms,
        ));

        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    info!("drone component: shutdown in send loop");
                    return false;
                }
                _ = interval.tick() => {
                    let messages = self.drain_queue(rx);
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

    /// Drain all pending messages from the outgoing channel without blocking.
    /// Returns the drained messages in FIFO order.
    fn drain_queue(&self, rx: &mut mpsc::Receiver<MavFrame>) -> Vec<MavFrame> {
        let mut messages = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            messages.push(frame);
        }
        messages
    }
}

impl Task for DroneComponent {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let component = Arc::clone(&self);
        let drone_handle = tokio::spawn(async move {
            component.connect().await;
        });
        info!("drone component started");

        // Spawn drone heartbeat loop. Resolve self_sysid after FC discovery,
        // supporting autodiscovery (config self_sysid = 0 -> min of available_systems).
        let cancel = self.cancel.clone();
        let ctx = Arc::clone(&self.ctx);
        let compid = self.ctx.config.mavlink.self_compid;
        let drone_hb_handle = tokio::spawn(
            async move {
                if wait_for_fc(&ctx, &cancel).await.is_none() {
                    return;
                }
                let sysid = match ctx.self_sysid().await {
                    Some(v) => v,
                    None => {
                        error!("drone heartbeat: cannot resolve self_sysid");
                        return;
                    }
                };
                info!(self_sysid = sysid, "drone heartbeat: resolved self_sysid");
                heartbeat_loop(&cancel, sysid, compid, ctx).await;
            }
            .instrument(info_span!(TRACING_SPAN_NAME)),
        );
        info!("drone heartbeat loop started");

        vec![drone_handle, drone_hb_handle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::mavlink::build_heartbeat;

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

    fn make_component(ctx: Arc<Context>, cancel: CancellationToken) -> Arc<DroneComponent> {
        Arc::new(DroneComponent::new(ctx, cancel))
    }

    #[tokio::test]
    async fn test_drain_queue_empty() {
        let ctx = Context::new(test_config());
        let component = make_component(ctx, CancellationToken::new());
        let (_tx, mut rx) = mpsc::channel::<MavFrame>(10);
        let drained = component.drain_queue(&mut rx);
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_drain_queue_single_message() {
        let ctx = Context::new(test_config());
        let component = make_component(ctx, CancellationToken::new());
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);
        let frame = build_heartbeat(1, 10);
        tx.send(frame).await.unwrap();

        let drained = component.drain_queue(&mut rx);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0.system_id, 1);
        assert_eq!(drained[0].0.component_id, 10);
    }

    #[tokio::test]
    async fn test_drain_queue_multiple_preserves_order() {
        let ctx = Context::new(test_config());
        let component = make_component(ctx, CancellationToken::new());
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);

        // Queue 5 messages with different sequence numbers
        for seq in 0u8..5 {
            let header = mavlink::MavHeader {
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

        let drained = component.drain_queue(&mut rx);
        assert_eq!(drained.len(), 5);
        for (i, frame) in drained.iter().enumerate() {
            assert_eq!(frame.0.sequence, i as u8, "message order not preserved");
        }
    }

    #[tokio::test]
    async fn test_drain_queue_nonblocking() {
        let ctx = Context::new(test_config());
        let component = make_component(ctx, CancellationToken::new());
        let (_tx, mut rx) = mpsc::channel::<MavFrame>(10);
        // drain_queue should return immediately even when empty
        let start = tokio::time::Instant::now();
        let drained = component.drain_queue(&mut rx);
        let elapsed = start.elapsed();
        assert!(drained.is_empty());
        assert!(elapsed < Duration::from_millis(10), "drain_queue blocked");
    }

    #[tokio::test]
    async fn test_drain_queue_leaves_channel_empty() {
        let ctx = Context::new(test_config());
        let component = make_component(ctx, CancellationToken::new());
        let (tx, mut rx) = mpsc::channel::<MavFrame>(10);
        tx.send(build_heartbeat(1, 10)).await.unwrap();
        tx.send(build_heartbeat(2, 20)).await.unwrap();

        let first = component.drain_queue(&mut rx);
        assert_eq!(first.len(), 2);

        let second = component.drain_queue(&mut rx);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn test_heartbeat_loop_waits_for_fc() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Take the outgoing rx so we can check what gets queued
        let mut rx = ctx.outgoing_rx.write().await;

        let sysid = ctx.config.mavlink.self_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx_clone).await;
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
        let mut rx = ctx.outgoing_rx.write().await;

        // Pre-add an FC system (ID < 200)
        ctx.add_system(1).await;

        let sysid = ctx.config.mavlink.self_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx_clone).await;
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

        // Pre-add FC so the loop starts
        ctx.add_system(1).await;

        let sysid = ctx.config.mavlink.self_sysid;
        let compid = ctx.config.mavlink.self_compid;
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            heartbeat_loop(&cancel_clone, sysid, compid, ctx).await;
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
