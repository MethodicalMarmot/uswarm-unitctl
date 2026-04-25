pub mod lte;
pub mod ping;
pub mod system;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::SensorsConfig;
use crate::context::Context;
use crate::messages::telemetry::{PingTelemetry, SystemTelemetry};
use crate::sensors::lte::LteReading;
use crate::Task;

use self::lte::LteSensor;
use self::ping::PingSensor;
use self::system::SystemSensor;

/// Shared sensor values, updated by sensor tasks and read by telemetry reporters.
pub struct SensorValues {
    pub ping: RwLock<Option<PingTelemetry>>,
    pub lte: RwLock<Option<LteReading>>,
    pub system: RwLock<Option<SystemTelemetry>>,
}

/// A sensor that runs as its own tokio task, gathering telemetry data
/// and storing results in shared Context.
#[async_trait]
pub trait Sensor: Send + Sync {
    /// Human-readable sensor name for logging.
    fn name(&self) -> &str;

    /// Run the sensor loop until cancellation. Implementations should
    /// periodically gather data and store results in `ctx`.
    async fn run(&self, ctx: Arc<Context>, cancel: CancellationToken);
}

/// Manages sensor lifecycle: builds enabled sensors from config
/// and spawns each as a tokio task.
pub struct SensorManager {
    pub ctx: Arc<Context>,
    pub cancel: CancellationToken,
    pub sensors: Mutex<Option<Vec<Box<dyn Sensor>>>>,
}

impl SensorManager {
    /// Build a SensorManager from config. Only enabled sensors are included.
    pub fn new(
        ctx: Arc<Context>,
        cancel: CancellationToken,
        config: &SensorsConfig,
        interface: &str,
    ) -> Self {
        let mut sensors: Vec<Box<dyn Sensor>> = Vec::new();

        if config.ping.enabled {
            info!("sensor enabled: ping");
            sensors.push(Box::new(PingSensor::new(
                &config.ping,
                interface.to_string(),
                config.default_interval_s,
            )));
        }

        if config.lte.enabled {
            info!("sensor enabled: lte");
            sensors.push(Box::new(LteSensor::new(
                &config.lte,
                config.default_interval_s,
            )));
        }

        if config.system.enabled {
            info!("sensor enabled: system");
            sensors.push(Box::new(SystemSensor::new(
                &config.system,
                config.default_interval_s,
            )));
        }

        SensorManager {
            ctx,
            cancel,
            sensors: Mutex::new(Some(sensors)),
        }
    }
}

impl Task for SensorManager {
    /// Spawn all sensors as tokio tasks. Each sensor runs until the
    /// cancellation token is triggered.
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let sensors: Vec<Box<dyn Sensor>> = self
            .sensors
            .lock()
            .expect("sensor mutex poisoned")
            .take()
            .expect("sensors already taken — run() must only be called once");

        let sensor_count = sensors.len();

        let handles: Vec<_> = sensors
            .into_iter()
            .map(|sensor| {
                let ctx = Arc::clone(&self.ctx);
                let cancel = self.cancel.clone();
                let name = sensor.name().to_string();
                tokio::spawn(async move {
                    info!(sensor = %name, "sensor task started");
                    sensor.run(ctx, cancel).await;
                    info!(sensor = %name, "sensor task stopped");
                })
            })
            .collect();

        info!(sensor_count, "sensor manager started");

        handles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LteSensorConfig, PingSensorConfig, SensorsConfig, SystemSensorConfig};

    fn make_manager_from_sensors(sensors: Vec<Box<dyn Sensor>>) -> Arc<SensorManager> {
        let config = crate::config::tests::test_config();
        let ctx = Context::new(config);
        Arc::new(SensorManager {
            ctx,
            cancel: CancellationToken::new(),
            sensors: Mutex::new(Some(sensors)),
        })
    }

    #[test]
    fn test_sensor_manager_empty_when_all_disabled() {
        let config = SensorsConfig {
            default_interval_s: 1.0,
            ping: PingSensorConfig {
                enabled: false,
                ..PingSensorConfig::default()
            },
            lte: LteSensorConfig {
                enabled: false,
                ..LteSensorConfig::default()
            },
            system: SystemSensorConfig {
                enabled: false,
                ..SystemSensorConfig::default()
            },
        };
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 0);
    }

    #[test]
    fn test_sensor_manager_count_with_defaults() {
        // With default config, all sensors are enabled (ping, lte, system).
        let config = SensorsConfig::default();
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_mock_sensor_runs_and_stops() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct MockSensor {
            ran: Arc<AtomicBool>,
        }

        #[async_trait]
        impl Sensor for MockSensor {
            fn name(&self) -> &str {
                "mock"
            }
            async fn run(&self, _ctx: Arc<Context>, cancel: CancellationToken) {
                self.ran.store(true, Ordering::SeqCst);
                cancel.cancelled().await;
            }
        }

        let config = crate::config::tests::test_config();
        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let ran = Arc::new(AtomicBool::new(false));
        let sensor = MockSensor {
            ran: Arc::clone(&ran),
        };

        let manager = Arc::new(SensorManager {
            ctx,
            cancel: cancel.clone(),
            sensors: Mutex::new(Some(vec![Box::new(sensor)])),
        });
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 1);

        manager.run();

        // Give the task a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(ran.load(Ordering::SeqCst), "mock sensor should have run");

        // Cancel should cause the sensor to stop
        cancel.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[test]
    fn test_sensor_manager_only_ping_enabled() {
        let config = SensorsConfig {
            default_interval_s: 1.0,
            ping: PingSensorConfig {
                enabled: true,
                ..PingSensorConfig::default()
            },
            lte: LteSensorConfig {
                enabled: false,
                ..LteSensorConfig::default()
            },
            system: SystemSensorConfig {
                enabled: false,
                ..SystemSensorConfig::default()
            },
        };
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 1);
        assert_eq!(
            manager.sensors.lock().unwrap().as_ref().unwrap()[0].name(),
            "ping"
        );
    }

    #[test]
    fn test_sensor_manager_only_lte_enabled() {
        let config = SensorsConfig {
            default_interval_s: 1.0,
            ping: PingSensorConfig {
                enabled: false,
                ..PingSensorConfig::default()
            },
            lte: LteSensorConfig {
                enabled: true,
                ..LteSensorConfig::default()
            },
            system: SystemSensorConfig {
                enabled: false,
                ..SystemSensorConfig::default()
            },
        };
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 1);
        assert_eq!(
            manager.sensors.lock().unwrap().as_ref().unwrap()[0].name(),
            "lte"
        );
    }

    #[test]
    fn test_sensor_manager_only_system_enabled() {
        let config = SensorsConfig {
            default_interval_s: 1.0,
            ping: PingSensorConfig {
                enabled: false,
                ..PingSensorConfig::default()
            },
            lte: LteSensorConfig {
                enabled: false,
                ..LteSensorConfig::default()
            },
            system: SystemSensorConfig {
                enabled: true,
                ..SystemSensorConfig::default()
            },
        };
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 1);
        assert_eq!(
            manager.sensors.lock().unwrap().as_ref().unwrap()[0].name(),
            "system"
        );
    }

    #[test]
    fn test_sensor_manager_two_of_three_enabled() {
        let config = SensorsConfig {
            default_interval_s: 1.0,
            ping: PingSensorConfig {
                enabled: true,
                ..PingSensorConfig::default()
            },
            lte: LteSensorConfig {
                enabled: false,
                ..LteSensorConfig::default()
            },
            system: SystemSensorConfig {
                enabled: true,
                ..SystemSensorConfig::default()
            },
        };
        let app_config = crate::config::tests::test_config();
        let ctx = Context::new(app_config);
        let manager = SensorManager::new(ctx, CancellationToken::new(), &config, "eth0");
        assert_eq!(manager.sensors.lock().unwrap().as_ref().unwrap().len(), 2);
        let guard = manager.sensors.lock().unwrap();
        let sensors = guard.as_ref().unwrap();
        assert_eq!(sensors[0].name(), "ping");
        assert_eq!(sensors[1].name(), "system");
    }

    #[tokio::test]
    async fn test_spawn_all_with_no_sensors() {
        let manager = make_manager_from_sensors(vec![]);
        // Should not panic with empty sensor list
        manager.run();
    }
}
