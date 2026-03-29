use std::sync::Arc;

use chrono::Utc;
use rumqttc::QoS;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::context::Context;
use crate::messages::telemetry::{LteTelemetry, TelemetryData, TelemetryEnvelope};
use crate::Task;

use super::transport::MqttTransport;

/// Publishes sensor telemetry data to MQTT topics at a configurable interval.
///
/// Reads sensor values from Context (LTE, ping, CPU temp), wraps each in a
/// `TelemetryEnvelope` with a UTC timestamp, and publishes to the appropriate
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
        let ts = Utc::now();

        // Publish LTE telemetry (publish signal fields only, matching spec)
        if let Some(ref reading) = *self.ctx.sensors.lte.read().await {
            let envelope = TelemetryEnvelope {
                ts,
                data: TelemetryData::Lte(LteTelemetry::from(reading)),
            };
            self.publish_one("lte", &envelope).await;
        }

        // Publish ping telemetry
        if let Some(reading) = self.ctx.sensors.ping.read().await.clone() {
            let envelope = TelemetryEnvelope {
                ts,
                data: TelemetryData::Ping(reading),
            };
            self.publish_one("ping", &envelope).await;
        }

        // Publish CPU temperature telemetry
        if let Some(reading) = self.ctx.sensors.cpu_temp.read().await.clone() {
            let envelope = TelemetryEnvelope {
                ts,
                data: TelemetryData::CpuTemp(reading),
            };
            self.publish_one("cpu_temp", &envelope).await;
        }
    }

    async fn publish_one(&self, name: &str, envelope: &TelemetryEnvelope) {
        match serde_json::to_string(envelope) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::telemetry::{
        CpuTempTelemetry, LteSignalQuality, LteTelemetry, PingTelemetry, TelemetryEnvelope,
    };
    use chrono::TimeZone;

    fn sample_ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 23, 10, 4, 0).unwrap()
    }

    // --- JSON serialization tests using TelemetryEnvelope ---

    #[test]
    fn test_ping_telemetry_envelope() {
        let envelope = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Ping(PingTelemetry {
                reachable: true,
                latency_ms: 25.5,
                loss_percent: 3,
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["ts"].as_str().unwrap().contains("2026-03-23"));
        let data = &parsed["data"];
        assert_eq!(data["reachable"], true);
        assert_eq!(data["latency_ms"], 25.5);
        assert_eq!(data["loss_percent"], 3);
    }

    #[test]
    fn test_lte_signal_telemetry_envelope() {
        let signal = LteSignalQuality {
            rsrq: -10,
            rsrp: -85,
            rssi: -60,
            rssnr: 15,
            earfcn: 1300,
            tx_power: 23,
            pcid: 42,
        };
        let envelope = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Lte(LteTelemetry {
                signal,
                neighbors: vec![],
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["ts"].as_str().unwrap().contains("2026-03-23"));
        let data = &parsed["data"];
        assert_eq!(data["signal"]["rsrp"], -85);
        assert_eq!(data["signal"]["rssi"], -60);
        assert_eq!(data["signal"]["pcid"], 42);
    }

    #[test]
    fn test_cpu_temp_telemetry_envelope() {
        let envelope = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::CpuTemp(CpuTempTelemetry {
                temperature_c: 42.5,
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["ts"].as_str().unwrap().contains("2026-03-23"));
        assert_eq!(parsed["data"]["temperature_c"], 42.5);
    }

    #[test]
    fn test_telemetry_envelope_has_type_tag() {
        let envelope = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Ping(PingTelemetry {
                reachable: true,
                latency_ms: 10.0,
                loss_percent: 0,
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed.get("ts").is_some());
        assert!(parsed.get("data").is_some());
        assert_eq!(parsed["data"]["type"], "Ping");
    }

    #[test]
    fn test_cpu_temp_envelope_has_type_tag() {
        let envelope = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::CpuTemp(CpuTempTelemetry {
                temperature_c: 50.0,
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed.get("ts").is_some());
        assert_eq!(parsed["data"]["type"], "CpuTemp");
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
        *ctx.sensors.ping.write().await = Some(PingTelemetry {
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
