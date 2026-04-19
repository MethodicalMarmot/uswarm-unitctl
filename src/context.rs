use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, RwLock};

use crate::config::Config;
use crate::mavlink::{is_fc_sysid, MavFrame};
use crate::sensors::SensorValues;
use crate::services::modem_access::ModemAccess;

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
    /// Only the drone component should hold the write lock on this.
    pub outgoing_rx: RwLock<mpsc::Receiver<MavFrame>>,

    /// Set of discovered flight controller system IDs (from heartbeats).
    pub available_systems: RwLock<HashSet<u8>>,

    /// Current sensor readings, updated by sensor tasks.
    pub sensors: SensorValues,

    /// Shared modem access service, set after modem discovery completes.
    pub modem: RwLock<Option<Arc<dyn ModemAccess>>>,
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
            outgoing_rx: RwLock::new(rx_outgoing),
            available_systems: RwLock::new(HashSet::new()),
            sensors: SensorValues {
                ping: RwLock::new(None),
                lte: RwLock::new(None),
                cpu_temp: RwLock::new(None),
            },
            modem: RwLock::new(None),
        })
    }

    /// Subscribe to the broadcast channel for incoming MAVLink messages.
    #[allow(dead_code)] // Used by sniffer tests and future switcher component
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<MavFrame> {
        self.tx_broadcast.subscribe()
    }

    /// Resolve the effective MAVLink self system ID.
    ///
    /// Returns the configured `mavlink.self_sysid` when it is non-zero.
    /// When the config value is `0`, autodiscovers the sysid as the minimum
    /// entry of `available_systems` (typically the FC), or `None` if no
    /// systems have been discovered yet.
    pub async fn self_sysid(&self) -> Option<u8> {
        let configured = self.config.mavlink.self_sysid;
        if configured > 0 {
            return Some(configured);
        }
        self.get_fc_sysids().await.into_iter().min()
    }

    /// Retrieves a set of Flight Controller (FC) system IDs.
    pub async fn get_fc_sysids(&self) -> HashSet<u8> {
        let systems = self.available_systems.read().await;
        systems
            .iter()
            .filter(|&&id| is_fc_sysid(id))
            .copied()
            .collect()
    }

    /// Register a discovered system ID. Returns `true` if the system was newly added.
    pub async fn add_system(&self, system_id: u8) -> bool {
        self.available_systems.write().await.insert(system_id)
    }

    /// Check if a system ID has been discovered.
    #[allow(dead_code)] // Used by tests and future switcher component
    pub async fn has_system(&self, system_id: u8) -> bool {
        self.available_systems.read().await.contains(&system_id)
    }

    /// Store the modem access service after discovery completes.
    pub async fn set_modem(&self, modem: Arc<dyn ModemAccess>) {
        *self.modem.write().await = Some(modem);
    }

    /// Get the modem access service, if available.
    pub async fn get_modem(&self) -> Option<Arc<dyn ModemAccess>> {
        self.modem.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::messages::telemetry::{CpuTempTelemetry, PingTelemetry};
    use crate::sensors::lte::{LteReading, LteSignalQuality};
    use mavlink::ardupilotmega::*;
    use mavlink::MavHeader;

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
        assert_eq!(ctx.config.mavlink.local_mavlink_port, 5760);
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

        let mut rx = ctx.outgoing_rx.write().await;
        let (rx_header, _rx_msg) = rx.recv().await.unwrap();
        assert_eq!(rx_header.system_id, 1);
        assert_eq!(rx_header.component_id, 10);
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

    // -- Acceptance: sensor values are stored in Context correctly --

    #[tokio::test]
    async fn test_sensor_values_initially_none() {
        let ctx = Context::new(test_config());
        assert!(ctx.sensors.ping.read().await.is_none());
        assert!(ctx.sensors.lte.read().await.is_none());
        assert!(ctx.sensors.cpu_temp.read().await.is_none());
    }

    #[tokio::test]
    async fn test_sensor_values_ping_write_read() {
        let ctx = Context::new(test_config());

        let reading = PingTelemetry {
            reachable: true,
            latency_ms: 25.5,
            loss_percent: 3,
        };
        *ctx.sensors.ping.write().await = Some(reading);

        let stored = ctx.sensors.ping.read().await;
        let stored = stored.as_ref().unwrap();
        assert!(stored.reachable);
        assert_eq!(stored.latency_ms, 25.5);
        assert_eq!(stored.loss_percent, 3);
    }

    #[tokio::test]
    async fn test_sensor_values_lte_write_read() {
        let ctx = Context::new(test_config());

        let reading = LteReading {
            signal: LteSignalQuality {
                rsrq: -10,
                rsrp: -85,
                rssi: -60,
                rssnr: 15,
                earfcn: 1300,
                tx_power: 23,
                pcid: 42,
            },
            neighbors: std::collections::HashMap::new(),
        };
        *ctx.sensors.lte.write().await = Some(reading);

        let stored = ctx.sensors.lte.read().await;
        let stored = stored.as_ref().unwrap();
        assert_eq!(stored.signal.rsrp, -85);
        assert_eq!(stored.signal.pcid, 42);
    }

    #[tokio::test]
    async fn test_sensor_values_cpu_temp_write_read() {
        let ctx = Context::new(test_config());

        let reading = CpuTempTelemetry {
            temperature_c: 42.5,
        };
        *ctx.sensors.cpu_temp.write().await = Some(reading);

        let stored = ctx.sensors.cpu_temp.read().await;
        let stored = stored.as_ref().unwrap();
        assert_eq!(stored.temperature_c, 42.5);
    }

    // -- Modem access tests --

    #[tokio::test]
    async fn test_modem_initially_none() {
        let ctx = Context::new(test_config());
        assert!(ctx.get_modem().await.is_none());
    }

    #[tokio::test]
    async fn test_set_and_get_modem() {
        use crate::services::modem_access::ModemError;

        struct FakeModem;

        #[async_trait::async_trait]
        impl ModemAccess for FakeModem {
            async fn model(&self) -> Result<String, ModemError> {
                Ok("FAKE_MODEM".to_string())
            }
            async fn command(&self, _cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
                Ok("OK".to_string())
            }
        }

        let ctx = Context::new(test_config());
        let modem: Arc<dyn ModemAccess> = Arc::new(FakeModem);
        ctx.set_modem(modem).await;

        let retrieved = ctx.get_modem().await;
        assert!(retrieved.is_some());
        let model = retrieved.unwrap().model().await.unwrap();
        assert_eq!(model, "FAKE_MODEM");
    }

    #[tokio::test]
    async fn test_sensor_values_concurrent_access() {
        let ctx = Context::new(test_config());
        let ctx = Arc::clone(&ctx);

        // Write from one task
        let ctx2 = Arc::clone(&ctx);
        let writer = tokio::spawn(async move {
            *ctx2.sensors.ping.write().await = Some(PingTelemetry {
                reachable: true,
                latency_ms: 10.0,
                loss_percent: 0,
            });
        });

        writer.await.unwrap();

        // Read from another context reference
        let stored = ctx.sensors.ping.read().await;
        assert!(stored.is_some());
        assert!(stored.as_ref().unwrap().reachable);
    }
}
