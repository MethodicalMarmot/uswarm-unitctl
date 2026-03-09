use std::collections::HashSet;
use std::sync::Arc;

use mavlink::ardupilotmega::MavMessage;
use mavlink::MavHeader;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::config::Config;

/// A received MAVLink message with its header.
pub type MavFrame = (MavHeader, MavMessage);

/// Shared application context.
///
/// Holds the configuration, communication channels, and runtime state
/// that is shared across all async tasks (drone component, sniffer, etc.).
pub struct Context {
    /// Application configuration (read-only after startup).
    pub config: Config,

    /// Sender side of the broadcast channel for incoming MAVLink messages.
    /// The sniffer publishes messages here; other tasks subscribe via `rx_broadcast`.
    pub tx_broadcast: broadcast::Sender<MavFrame>,

    /// Sender side of the mpsc channel for outgoing MAVLink messages.
    /// Tasks enqueue messages here; the drone component drains them to the wire.
    pub tx_outgoing: mpsc::Sender<MavFrame>,

    /// Receiver side of the mpsc channel for outgoing MAVLink messages.
    /// Only the drone component holds this (via `take_outgoing_rx`).
    outgoing_rx: RwLock<Option<mpsc::Receiver<MavFrame>>>,

    /// Set of discovered flight controller system IDs (from heartbeats).
    pub available_systems: RwLock<HashSet<u8>>,
}

const BROADCAST_CAPACITY: usize = 256;
const OUTGOING_CAPACITY: usize = 500;

impl Context {
    /// Create a new context from the given configuration.
    pub fn new(config: Config) -> Arc<Self> {
        let (tx_broadcast, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (tx_outgoing, rx_outgoing) = mpsc::channel(OUTGOING_CAPACITY);

        Arc::new(Self {
            config,
            tx_broadcast,
            tx_outgoing,
            outgoing_rx: RwLock::new(Some(rx_outgoing)),
            available_systems: RwLock::new(HashSet::new()),
        })
    }

    /// Subscribe to the broadcast channel for incoming MAVLink messages.
    #[allow(dead_code)] // Used by tests and future tasks (switcher, telemetry)
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<MavFrame> {
        self.tx_broadcast.subscribe()
    }

    /// Take the outgoing message receiver. Can only be called once (by the drone component).
    /// Returns `None` if already taken.
    pub async fn take_outgoing_rx(&self) -> Option<mpsc::Receiver<MavFrame>> {
        self.outgoing_rx.write().await.take()
    }

    /// Register a discovered system ID. Returns `true` if the system was newly added.
    pub async fn add_system(&self, system_id: u8) -> bool {
        self.available_systems.write().await.insert(system_id)
    }

    /// Check if a system ID has been discovered.
    #[allow(dead_code)] // Used by tests; useful for future components
    pub async fn has_system(&self, system_id: u8) -> bool {
        self.available_systems.read().await.contains(&system_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mavlink::ardupilotmega::*;

    fn test_config() -> Config {
        let toml_str = "[mavlink]\n";
        toml::from_str(toml_str).unwrap()
    }

    fn make_heartbeat(mavtype: MavType) -> MavMessage {
        MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype,
            autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: MavModeFlag::empty(),
            system_status: MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        })
    }

    #[tokio::test]
    async fn test_context_creation() {
        let ctx = Context::new(test_config());
        assert_eq!(ctx.config.mavlink.port, 5760);
        assert!(ctx.available_systems.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_broadcast_channel() {
        let ctx = Context::new(test_config());
        let mut rx = ctx.subscribe_broadcast();

        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let msg = make_heartbeat(MavType::MAV_TYPE_QUADROTOR);

        ctx.tx_broadcast.send((header, msg.clone())).unwrap();

        let (rx_header, rx_msg) = rx.recv().await.unwrap();
        assert_eq!(rx_header.system_id, 1);
        match rx_msg {
            MavMessage::HEARTBEAT(_) => {}
            _ => panic!("expected HEARTBEAT message"),
        }
    }

    #[tokio::test]
    async fn test_outgoing_channel() {
        let ctx = Context::new(test_config());

        let header = MavHeader {
            system_id: 1,
            component_id: 10,
            sequence: 0,
        };
        let msg = make_heartbeat(MavType::MAV_TYPE_ONBOARD_CONTROLLER);

        ctx.tx_outgoing.send((header, msg)).await.unwrap();

        let mut rx = ctx.take_outgoing_rx().await.unwrap();
        let (rx_header, _rx_msg) = rx.recv().await.unwrap();
        assert_eq!(rx_header.system_id, 1);
        assert_eq!(rx_header.component_id, 10);
    }

    #[tokio::test]
    async fn test_take_outgoing_rx_only_once() {
        let ctx = Context::new(test_config());
        let first = ctx.take_outgoing_rx().await;
        assert!(first.is_some());
        let second = ctx.take_outgoing_rx().await;
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn test_available_systems() {
        let ctx = Context::new(test_config());
        assert!(!ctx.has_system(1).await);

        assert!(ctx.add_system(1).await); // newly inserted
        assert!(ctx.has_system(1).await);

        assert!(ctx.add_system(2).await); // newly inserted
        assert!(ctx.has_system(2).await);

        // Adding same system again is idempotent
        assert!(!ctx.add_system(1).await); // already existed
        assert_eq!(ctx.available_systems.read().await.len(), 2);
    }

    #[tokio::test]
    async fn test_outgoing_channel_capacity() {
        let ctx = Context::new(test_config());
        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let msg = make_heartbeat(MavType::MAV_TYPE_QUADROTOR);

        // Fill up to capacity (500) should not block
        for _ in 0..500 {
            ctx.tx_outgoing.send((header, msg.clone())).await.unwrap();
        }

        // The 501st should not succeed immediately (try_send)
        let result = ctx.tx_outgoing.try_send((header, msg));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_broadcast_multiple_subscribers() {
        let ctx = Context::new(test_config());
        let mut rx1 = ctx.subscribe_broadcast();
        let mut rx2 = ctx.subscribe_broadcast();

        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let msg = make_heartbeat(MavType::MAV_TYPE_QUADROTOR);

        ctx.tx_broadcast.send((header, msg)).unwrap();

        // Both subscribers receive the message
        let (h1, _) = rx1.recv().await.unwrap();
        let (h2, _) = rx2.recv().await.unwrap();
        assert_eq!(h1.system_id, 1);
        assert_eq!(h2.system_id, 1);
    }
}
