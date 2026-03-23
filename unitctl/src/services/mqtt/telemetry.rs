use std::sync::Arc;

use chrono::Utc;
use rumqttc::QoS;
use serde::ser::Error as _;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::context::Context;
use crate::Task;

use super::transport::MqttTransport;

/// Publishes sensor telemetry data to MQTT topics at a configurable interval.
///
/// Reads sensor values from Context (LTE, ping, CPU temp), wraps each in a
/// JSON payload with an ISO 8601 timestamp, and publishes to the appropriate
/// telemetry topic. Sensors with no reading (None) are skipped.
pub struct TelemetryPublisher {
    transport: Arc<MqttTransport>,
    ctx: Arc<Context>,
    interval: Duration,
    cancel: CancellationToken,
}

impl TelemetryPublisher {
    pub fn new(
        transport: Arc<MqttTransport>,
        ctx: Arc<Context>,
        interval: Duration,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            transport,
            ctx,
            interval,
            cancel,
        }
    }

    async fn publish_telemetry(&self) {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        // Publish LTE telemetry (publish signal fields only, matching spec)
        if let Some(reading) = self.ctx.sensors.lte.read().await.clone() {
            self.publish_one("lte", &reading.signal, &ts).await;
        }

        // Publish ping telemetry
        if let Some(reading) = self.ctx.sensors.ping.read().await.clone() {
            self.publish_one("ping", &reading, &ts).await;
        }

        // Publish CPU temperature telemetry
        if let Some(reading) = self.ctx.sensors.cpu_temp.read().await.clone() {
            self.publish_one("cpu_temp", &reading, &ts).await;
        }
    }

    async fn publish_one(&self, name: &str, reading: &impl serde::Serialize, ts: &str) {
        match build_telemetry_json(reading, ts) {
            Ok(payload) => {
                let topic = self.transport.telemetry_topic(name);
                if let Err(e) = self
                    .transport
                    .publish(&topic, payload.as_bytes(), QoS::AtMostOnce, false)
                    .await
                {
                    warn!(error = %e, topic = %topic, "failed to publish {name} telemetry");
                } else {
                    debug!(topic = %topic, "published {name} telemetry");
                }
            }
            Err(e) => warn!(error = %e, "failed to serialize {name} telemetry"),
        }
    }
}

impl Task for TelemetryPublisher {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        debug!("telemetry publisher cancelled");
                        break;
                    }
                    _ = tokio::time::sleep(self.interval) => {
                        self.publish_telemetry().await;
                    }
                }
            }
        })]
    }
}

/// Build a JSON string from a serializable reading, injecting a `ts` field.
///
/// Serializes the reading to a serde_json::Value (must be an Object),
/// then inserts the timestamp at the `ts` key.
pub fn build_telemetry_json<T: serde::Serialize>(
    reading: &T,
    ts: &str,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(reading)?;
    let obj = value.as_object_mut().ok_or_else(|| {
        serde_json::Error::custom("telemetry reading must serialize as a JSON object")
    })?;
    obj.insert("ts".to_string(), serde_json::Value::String(ts.to_string()));
    serde_json::to_string(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::cpu_temp::CpuTempReading;
    use crate::sensors::lte::LteSignalQuality;
    use crate::sensors::ping::PingReading;

    // --- JSON serialization tests ---

    #[test]
    fn test_ping_telemetry_json() {
        let reading = PingReading {
            reachable: true,
            latency_ms: 25.5,
            loss_percent: 3,
        };
        let ts = "2026-03-23T10:04:00Z";
        let json = build_telemetry_json(&reading, ts).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["ts"], "2026-03-23T10:04:00Z");
        assert_eq!(parsed["reachable"], true);
        assert_eq!(parsed["latency_ms"], 25.5);
        assert_eq!(parsed["loss_percent"], 3);
    }

    #[test]
    fn test_lte_telemetry_json() {
        // Telemetry publishes signal fields only (flat), matching the plan spec
        let signal = LteSignalQuality {
            rsrq: -10,
            rsrp: -85,
            rssi: -60,
            rssnr: 15,
            earfcn: 1300,
            tx_power: 23,
            pcid: 42,
        };
        let ts = "2026-03-23T10:04:00Z";
        let json = build_telemetry_json(&signal, ts).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["ts"], "2026-03-23T10:04:00Z");
        assert_eq!(parsed["rsrp"], -85);
        assert_eq!(parsed["rssi"], -60);
        assert_eq!(parsed["pcid"], 42);
        assert_eq!(parsed["rsrq"], -10);
        assert_eq!(parsed["rssnr"], 15);
        assert_eq!(parsed["earfcn"], 1300);
        assert_eq!(parsed["tx_power"], 23);
    }

    #[test]
    fn test_cpu_temp_telemetry_json() {
        let reading = CpuTempReading {
            temperature_c: 42.5,
        };
        let ts = "2026-03-23T10:04:00Z";
        let json = build_telemetry_json(&reading, ts).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["ts"], "2026-03-23T10:04:00Z");
        assert_eq!(parsed["temperature_c"], 42.5);
    }

    #[test]
    fn test_telemetry_json_field_names_match_spec() {
        // Verify the serialized field names match what the plan specifies
        let ping = PingReading {
            reachable: true,
            latency_ms: 10.0,
            loss_percent: 0,
        };
        let json = build_telemetry_json(&ping, "2026-01-01T00:00:00Z").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = parsed.as_object().unwrap();

        assert!(obj.contains_key("ts"));
        assert!(obj.contains_key("reachable"));
        assert!(obj.contains_key("latency_ms"));
        assert!(obj.contains_key("loss_percent"));
    }

    #[test]
    fn test_cpu_temp_json_field_names() {
        let reading = CpuTempReading {
            temperature_c: 50.0,
        };
        let json = build_telemetry_json(&reading, "2026-01-01T00:00:00Z").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = parsed.as_object().unwrap();

        assert!(obj.contains_key("ts"));
        assert!(obj.contains_key("temperature_c"));
    }

    // --- TelemetryPublisher None-skipping tests ---

    #[tokio::test]
    async fn test_publish_skips_none_readings() {
        // Create a context where all sensor readings are None (default)
        let config = crate::config::tests::test_config();
        let ctx = crate::context::Context::new(config);

        // All readings should be None initially
        assert!(ctx.sensors.ping.read().await.is_none());
        assert!(ctx.sensors.lte.read().await.is_none());
        assert!(ctx.sensors.cpu_temp.read().await.is_none());

        // Create a transport with dummy client for testing
        let mqtt_config = crate::config::MqttConfig::default();
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 1);
        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let transport = Arc::new(super::super::transport::MqttTransport::new_for_test(
            client,
            "test-node".to_string(),
            mqtt_config.env_prefix.clone(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let publisher = TelemetryPublisher::new(transport, ctx, Duration::from_secs(1), cancel);

        // publish_telemetry should not panic when all readings are None
        // (it simply skips them)
        publisher.publish_telemetry().await;
        // If we got here without panicking, the test passes
    }

    #[tokio::test]
    async fn test_publish_with_some_readings() {
        let config = crate::config::tests::test_config();
        let ctx = crate::context::Context::new(config);

        // Set only ping reading, leave others as None
        *ctx.sensors.ping.write().await = Some(PingReading {
            reachable: true,
            latency_ms: 10.0,
            loss_percent: 0,
        });

        assert!(ctx.sensors.ping.read().await.is_some());
        assert!(ctx.sensors.lte.read().await.is_none());
        assert!(ctx.sensors.cpu_temp.read().await.is_none());

        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 1);
        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let transport = Arc::new(super::super::transport::MqttTransport::new_for_test(
            client,
            "test-node".to_string(),
            "test".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let publisher = TelemetryPublisher::new(transport, ctx, Duration::from_secs(1), cancel);

        // Should publish ping but skip lte and cpu_temp without error
        publisher.publish_telemetry().await;
    }

    #[test]
    fn test_task_trait_implementation() {
        // Verify TelemetryPublisher can be wrapped in Arc and implements Task
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 1);
        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let transport = Arc::new(super::super::transport::MqttTransport::new_for_test(
            client,
            "test-node".to_string(),
            "test".to_string(),
            event_tx,
        ));

        let config = crate::config::tests::test_config();
        let ctx = crate::context::Context::new(config);
        let cancel = CancellationToken::new();

        let publisher = Arc::new(TelemetryPublisher::new(
            transport,
            ctx,
            Duration::from_secs(1),
            cancel,
        ));

        // Verify it can be used as Arc<dyn Task> — this is a compile-time check
        let _task: Arc<dyn Task> = publisher;
    }
}
