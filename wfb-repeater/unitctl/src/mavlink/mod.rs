use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub mod commands;
pub mod drone;
pub mod sniffer;

/// System IDs below this threshold are considered flight controllers.
/// Matches the Python `get_fc_system_id()` logic.
pub const FC_SYSID_THRESHOLD: u8 = 200;

/// Returns true if the given system ID belongs to a flight controller.
/// Excludes system ID 0 (MAVLink broadcast/reserved address).
pub fn is_fc_sysid(id: u8) -> bool {
    id > 0 && id < FC_SYSID_THRESHOLD
}

/// Sleep 1s or return early if shutdown is requested.
pub async fn backoff_or_cancel(cancel: &CancellationToken) {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
}
