use std::sync::{Arc, Mutex};

use chrono::Utc;
use rumqttc::QoS;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::messages::status::{NodeStatusEnvelope, OnlineStatusData, StatusData};
use crate::Task;

use super::transport::{MqttEvent, MqttTransport};

/// Publishes an online status message (retained) on each MQTT connection.
///
/// Listens for `MqttEvent::Connected` from the transport broadcast channel
/// and publishes a `NodeStatusEnvelope` with `StatusData::Online` to the
/// status topic. Works alongside the LWT (offline) message set in transport.
pub struct StatusPublisher {
    transport: Arc<MqttTransport>,
    cancel: CancellationToken,
    /// Pre-created event receiver to avoid missing the first ConnAck.
    /// Taken once by `run()` via `Option::take()`.
    event_rx: Mutex<Option<tokio::sync::broadcast::Receiver<MqttEvent>>>,
}

impl StatusPublisher {
    pub fn new(transport: Arc<MqttTransport>, cancel: CancellationToken) -> Self {
        let event_rx = transport.subscribe_events();
        Self {
            transport,
            cancel,
            event_rx: Mutex::new(Some(event_rx)),
        }
    }

    /// Build and publish an online status message (retained, QoS 1).
    async fn publish_online(&self) {
        let envelope = NodeStatusEnvelope {
            ts: Utc::now(),
            data: StatusData::Online(OnlineStatusData {
                session: self.transport.session_id().to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        };

        match serde_json::to_string(&envelope) {
            Ok(payload) => {
                let topic = self.transport.status_topic();
                if let Err(e) = self
                    .transport
                    .publish(&topic, payload.as_bytes(), QoS::AtLeastOnce, true)
                    .await
                {
                    warn!(error = %e, topic = %topic, "failed to publish online status");
                } else {
                    debug!(topic = %topic, "published online status");
                }
            }
            Err(e) => warn!(error = %e, "failed to serialize online status"),
        }
    }
}

impl Task for StatusPublisher {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let mut event_rx = self
            .event_rx
            .lock()
            .expect("event_rx mutex poisoned")
            .take()
            .expect("event_rx already taken — run() must only be called once");

        vec![tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        debug!("status publisher cancelled");
                        break;
                    }
                    event = event_rx.recv() => {
                        match event {
                            Ok(MqttEvent::Connected) => {
                                self.publish_online().await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                debug!("MQTT event channel closed, stopping status publisher");
                                break;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!(skipped = n, "status publisher lagged, publishing online status defensively");
                                self.publish_online().await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `publish_online()` would produce a correct JSON payload.
    #[test]
    fn test_online_payload_structure() {
        let envelope = NodeStatusEnvelope {
            ts: Utc::now(),
            data: StatusData::Online(OnlineStatusData {
                session: "abc123".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed.get("ts").is_some());
        let data = parsed.get("data").expect("missing data field");
        assert_eq!(data.get("type").unwrap(), "Online");
        assert_eq!(data.get("session").unwrap(), "abc123");
        assert!(data.get("version").is_some());
        // No offline fields
        assert!(data.get("last_session").is_none());
        assert!(data.get("last_online").is_none());
    }

    /// Verify the published topic matches the status_topic() format.
    #[test]
    fn test_status_topic_matches_transport() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 1);
        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        assert_eq!(transport.status_topic(), "prod/nodes/drone-42/status");

        let cancel = CancellationToken::new();
        let publisher = StatusPublisher::new(transport.clone(), cancel);
        // The publisher uses transport.status_topic() internally,
        // so the topic it publishes to is the same.
        assert_eq!(
            publisher.transport.status_topic(),
            "prod/nodes/drone-42/status"
        );
    }

    /// Verify StatusPublisher can be wrapped in Arc and used as Task.
    #[test]
    fn test_task_trait_implementation() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 1);
        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "test-node".to_string(),
            "test".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let publisher = Arc::new(StatusPublisher::new(transport, cancel));
        let _task: Arc<dyn Task> = publisher;
    }
}
