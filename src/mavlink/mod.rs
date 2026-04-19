use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::context::Context;
use mavlink::ardupilotmega::{
    MavAutopilot, MavMessage, MavModeFlag, MavState, MavType, HEARTBEAT_DATA,
};
use mavlink::{Connection, MavConnection, MavHeader, MavlinkVersion};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub mod commands;
pub mod drone_component;
pub mod sniffer_component;
pub mod telemetry_reporter;

/// System IDs below this threshold are considered flight controllers.
/// Matches the Python `get_fc_system_id()` logic.
pub const FC_SYSID_THRESHOLD: u8 = 200;

/// A received MAVLink message with its header.
pub type MavFrame = (MavHeader, MavMessage);

/// Sleep 1s or return early if shutdown is requested.
pub async fn backoff_or_cancel(cancel: &CancellationToken) {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
}

async fn mavlink_connect(
    cancel: &CancellationToken,
    addr: &String,
) -> Option<Connection<MavMessage>> {
    info!(address = %addr, "connecting to mavlink-routerd");

    // Blocking TCP connect via spawn_blocking
    let addr = addr.clone();
    let conn_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            info!("shutdown during connect");
            return None;
        }
        result = tokio::task::spawn_blocking(move || {
            mavlink::connect::<MavMessage>(&addr)
        }) => result,
    };

    let mut conn = match conn_result {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            error!(error = %e, "connection failed, retrying in 1s");
            backoff_or_cancel(cancel).await;
            return None;
        }
        Err(e) => {
            error!(error = %e, "connect task panicked");
            backoff_or_cancel(cancel).await;
            return None;
        }
    };

    conn.set_protocol_version(MavlinkVersion::V2);
    info!("mavlink connection established (MAVLink v2)");
    Some(conn)
}

/// Returns true if the given system ID belongs to a flight controller.
/// Excludes system ID 0 (MAVLink broadcast/reserved address).
pub fn is_fc_sysid(id: u8) -> bool {
    id > 0 && id < FC_SYSID_THRESHOLD
}

/// Wait for a flight controller to be discovered (system ID < 200).
pub(crate) async fn wait_for_fc(
    ctx: &Arc<Context>,
    cancel: &CancellationToken,
) -> Option<HashSet<u8>> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            _ = interval.tick() => {
                let fc_ids = ctx.get_fc_sysids().await;
                if ! fc_ids.is_empty() {
                    return Some(fc_ids);
                }
            }
        }
    }
}

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

/// Heartbeat loop: enqueues a heartbeat message every 1 second via the outgoing channel.
/// Waits for flight controller discovery before starting.
pub async fn heartbeat_loop(cancel: &CancellationToken, sysid: u8, compid: u8, ctx: Arc<Context>) {
    // Wait for at least one FC system to be discovered before starting heartbeats
    info!("mavlink heartbeat: waiting for flight controller discovery");
    if let Some(fc_ids) = wait_for_fc(&ctx, cancel).await {
        info!(?fc_ids, "flight controller detected, system operational");
    }

    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                info!("mavlink heartbeat: shutdown");
                return;
            }
            _ = interval.tick() => {
                let frame = build_heartbeat(sysid, compid);
                match ctx.tx_outgoing.try_send(frame) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        warn!("mavlink heartbeat: outgoing queue full, dropping heartbeat");
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        warn!("mavlink heartbeat: outgoing channel closed");
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::tests::test_config;
    use crate::context::Context;
    use crate::mavlink::wait_for_fc;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_wait_for_fc_returns_true_when_fc_discovered() {
        let ctx = Context::new(test_config());
        let cancel = CancellationToken::new();

        // Pre-add an FC system (ID < 200)
        ctx.add_system(1).await;

        let result = wait_for_fc(&ctx, &cancel).await;
        assert!(result.is_some());
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
        assert!(result.is_none());
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
        assert!(result.is_some());
    }
}
