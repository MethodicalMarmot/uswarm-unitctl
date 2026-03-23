use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS, Transport};
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::tls;
use crate::config::MqttConfig;
use crate::Task;

/// Events emitted by the MQTT transport layer.
#[derive(Debug, Clone)]
pub enum MqttEvent {
    Connected,
    Disconnected,
    Message { topic: String, payload: Vec<u8> },
}

/// Errors from the MQTT transport layer.
#[derive(Debug)]
pub enum TransportError {
    Tls(tls::TlsError),
    Client(rumqttc::ClientError),
    InvalidConfig(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Tls(e) => write!(f, "TLS error: {e}"),
            TransportError::Client(e) => write!(f, "MQTT client error: {e}"),
            TransportError::InvalidConfig(e) => write!(f, "invalid config: {e}"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Tls(e) => Some(e),
            TransportError::Client(e) => Some(e),
            TransportError::InvalidConfig(_) => None,
        }
    }
}

impl From<tls::TlsError> for TransportError {
    fn from(e: tls::TlsError) -> Self {
        TransportError::Tls(e)
    }
}

impl From<rumqttc::ClientError> for TransportError {
    fn from(e: rumqttc::ClientError) -> Self {
        TransportError::Client(e)
    }
}

/// MQTT topic special characters that must not appear in node_id or env_prefix.
const MQTT_TOPIC_FORBIDDEN_CHARS: &[char] = &['+', '#', '/', '\0'];

/// Validate that a string is safe for use in MQTT topic segments.
fn validate_topic_segment(value: &str, name: &str) -> Result<(), TransportError> {
    if value.is_empty() {
        return Err(TransportError::InvalidConfig(format!(
            "{name} must not be empty"
        )));
    }
    if let Some(c) = value
        .chars()
        .find(|c| MQTT_TOPIC_FORBIDDEN_CHARS.contains(c))
    {
        return Err(TransportError::InvalidConfig(format!(
            "{name} contains forbidden MQTT character: '{c}'"
        )));
    }
    Ok(())
}

/// Broadcast channel capacity for MQTT events.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// MQTT transport layer handling connection, TLS, reconnection, publish/subscribe.
pub struct MqttTransport {
    client: AsyncClient,
    node_id: String,
    env_prefix: String,
    event_tx: broadcast::Sender<MqttEvent>,
    eventloop: Mutex<EventLoop>,
    cancel: CancellationToken,
}

impl Task for MqttTransport {
    /// Drive the rumqttc event loop. This must be spawned as a tokio task.
    ///
    /// Handles reconnection automatically (rumqttc reconnects when poll() is called
    /// after a connection error). Forwards incoming publish messages as MqttEvents
    /// on the broadcast channel.
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        info!(
            node_id = %self.node_id,
            env_prefix = %self.env_prefix,
            "MQTT event loop starting"
        );

        let transport = Arc::clone(&self);
        vec![tokio::spawn(async move {
            let mut eventloop = transport.eventloop.lock().await;
            loop {
                tokio::select! {
                    _ = transport.cancel.cancelled() => {
                        info!("MQTT event loop cancelled");
                        break;
                    }
                    event = eventloop.poll() => {
                        match event {
                            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                                info!(node_id = %transport.node_id, "MQTT connected");
                                let _ = transport.event_tx.send(MqttEvent::Connected);
                            }
                            Ok(Event::Incoming(Packet::Publish(publish))) => {
                                debug!(
                                    topic = %publish.topic,
                                    payload_len = publish.payload.len(),
                                    "MQTT message received"
                                );
                                let _ = transport.event_tx.send(MqttEvent::Message {
                                    topic: publish.topic.clone(),
                                    payload: publish.payload.to_vec(),
                                });
                            }
                            Ok(Event::Incoming(_)) => {
                                // Other incoming packets (PingResp, SubAck, etc.) — no action needed
                            }
                            Ok(Event::Outgoing(_)) => {
                                // Outgoing events — no action needed
                            }
                            Err(e) => {
                                warn!("MQTT connection error: {e}");
                                let _ = transport.event_tx.send(MqttEvent::Disconnected);
                                // rumqttc will attempt to reconnect on next poll()
                                tokio::time::sleep(Duration::from_secs(1)).await;
                            }
                        }
                    }
                }
            }
        })]
    }
}

impl MqttTransport {
    /// Create a new MqttTransport from configuration.
    ///
    /// Loads TLS certificates, extracts the node ID from the client certificate CN,
    /// and creates the rumqttc AsyncClient + EventLoop.
    ///
    /// The event loop is stored internally and consumed by `run_event_loop`.
    pub fn new(config: &MqttConfig, cancel: CancellationToken) -> Result<Self, TransportError> {
        let tls_config = tls::load_tls_config(
            &config.ca_cert_path,
            &config.client_cert_path,
            &config.client_key_path,
        )?;
        let node_id = tls::extract_node_id(&config.client_cert_path)?;
        validate_topic_segment(&node_id, "node_id (certificate CN)")?;
        validate_topic_segment(&config.env_prefix, "env_prefix")?;

        let mut mqtt_options = MqttOptions::new(&node_id, &config.host, config.port);
        mqtt_options.set_keep_alive(Duration::from_secs(30));
        mqtt_options.set_transport(Transport::tls_with_config(tls_config));
        mqtt_options.set_clean_session(false);

        let (client, eventloop) = AsyncClient::new(mqtt_options, 50);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        Ok(Self {
            client,
            node_id,
            env_prefix: config.env_prefix.clone(),
            event_tx,
            eventloop: Mutex::new(eventloop),
            cancel,
        })
    }

    /// Publish a message to the given topic.
    pub async fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
        retain: bool,
    ) -> Result<(), TransportError> {
        self.client
            .publish(topic, qos, retain, payload.to_vec())
            .await?;
        Ok(())
    }

    /// Subscribe to a topic.
    pub async fn subscribe(&self, topic: &str, qos: QoS) -> Result<(), TransportError> {
        self.client.subscribe(topic, qos).await?;
        Ok(())
    }

    /// Get a receiver for MQTT events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<MqttEvent> {
        self.event_tx.subscribe()
    }

    /// Build a telemetry topic path.
    ///
    /// Returns `{env_prefix}/nodes/{node_id}/telemetry/{name}`
    pub fn telemetry_topic(&self, name: &str) -> String {
        format!(
            "{}/nodes/{}/telemetry/{}",
            self.env_prefix, self.node_id, name
        )
    }

    /// Build a command topic path.
    ///
    /// Returns `{env_prefix}/nodes/{node_id}/cmnd/{cmd}/{suffix}`
    pub fn command_topic(&self, cmd: &str, suffix: &str) -> String {
        if suffix.is_empty() {
            format!("{}/nodes/{}/cmnd/{}", self.env_prefix, self.node_id, cmd)
        } else if suffix.is_empty() && cmd.is_empty() {
            format!("{}/nodes/{}/cmnd", self.env_prefix, self.node_id)
        } else {
            format!(
                "{}/nodes/{}/cmnd/{}/{}",
                self.env_prefix, self.node_id, cmd, suffix
            )
        }
    }

    /// Get the node ID extracted from the client certificate.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Create a transport with pre-built fields for testing (no TLS or broker needed).
    #[cfg(test)]
    pub(crate) fn new_for_test(
        client: AsyncClient,
        node_id: String,
        env_prefix: String,
        event_tx: broadcast::Sender<MqttEvent>,
    ) -> Self {
        let opts = MqttOptions::new("dummy", "localhost", 1883);
        let (_dummy_client, eventloop) = AsyncClient::new(opts, 1);
        Self {
            client,
            node_id,
            env_prefix,
            event_tx,
            eventloop: Mutex::new(eventloop),
            cancel: CancellationToken::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_transport_fields(node_id: &str, env_prefix: &str) -> MqttTransport {
        // Create a transport with a dummy client for testing topic builders.
        let opts = MqttOptions::new("test", "localhost", 1883);
        let (client, eventloop) = AsyncClient::new(opts, 1);
        let (event_tx, _) = broadcast::channel(1);

        MqttTransport {
            client,
            node_id: node_id.to_string(),
            env_prefix: env_prefix.to_string(),
            event_tx,
            eventloop: Mutex::new(eventloop),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn test_telemetry_topic() {
        let transport = make_transport_fields("drone-42", "prod");
        assert_eq!(
            transport.telemetry_topic("lte"),
            "prod/nodes/drone-42/telemetry/lte"
        );
        assert_eq!(
            transport.telemetry_topic("ping"),
            "prod/nodes/drone-42/telemetry/ping"
        );
        assert_eq!(
            transport.telemetry_topic("cpu_temp"),
            "prod/nodes/drone-42/telemetry/cpu_temp"
        );
    }

    #[test]
    fn test_command_topic() {
        let transport = make_transport_fields("drone-42", "prod");
        assert_eq!(
            transport.command_topic("config_update", "in"),
            "prod/nodes/drone-42/cmnd/config_update/in"
        );
        assert_eq!(
            transport.command_topic("config_update", "status"),
            "prod/nodes/drone-42/cmnd/config_update/status"
        );
        assert_eq!(
            transport.command_topic("get_config", "result"),
            "prod/nodes/drone-42/cmnd/get_config/result"
        );
    }

    #[test]
    fn test_telemetry_topic_different_env() {
        let transport = make_transport_fields("node-abc", "staging");
        assert_eq!(
            transport.telemetry_topic("lte"),
            "staging/nodes/node-abc/telemetry/lte"
        );
    }

    #[test]
    fn test_command_topic_wildcard_subscription_pattern() {
        // Verify the topic structure matches what CommandProcessor will subscribe to
        let transport = make_transport_fields("drone-42", "prod");
        let in_topic = transport.command_topic("config_update", "in");
        // The wildcard pattern would be: prod/nodes/drone-42/cmnd/+/in
        let prefix = "prod/nodes/drone-42/cmnd/";
        assert!(in_topic.starts_with(&prefix));
        assert!(in_topic.ends_with("/in"));
    }

    #[test]
    fn test_node_id_accessor() {
        let transport = make_transport_fields("drone-42", "prod");
        assert_eq!(transport.node_id(), "drone-42");
    }

    #[test]
    fn test_event_broadcast() {
        let transport = make_transport_fields("drone-42", "prod");
        let mut rx = transport.subscribe_events();

        // Send an event through the internal sender
        let _ = transport.event_tx.send(MqttEvent::Connected);

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, MqttEvent::Connected));
    }

    #[test]
    fn test_message_event_broadcast() {
        let transport = make_transport_fields("drone-42", "prod");
        let mut rx = transport.subscribe_events();

        let _ = transport.event_tx.send(MqttEvent::Message {
            topic: "test/topic".to_string(),
            payload: b"hello".to_vec(),
        });

        let event = rx.try_recv().unwrap();
        match event {
            MqttEvent::Message { topic, payload } => {
                assert_eq!(topic, "test/topic");
                assert_eq!(payload, b"hello");
            }
            _ => panic!("expected Message event"),
        }
    }

    #[test]
    fn test_validate_topic_segment_valid() {
        assert!(validate_topic_segment("drone-42", "test").is_ok());
        assert!(validate_topic_segment("prod", "test").is_ok());
        assert!(validate_topic_segment("node.abc", "test").is_ok());
    }

    #[test]
    fn test_validate_topic_segment_forbidden_chars() {
        assert!(validate_topic_segment("drone+42", "test").is_err());
        assert!(validate_topic_segment("prod#env", "test").is_err());
        assert!(validate_topic_segment("a/b", "test").is_err());
        assert!(validate_topic_segment("null\0byte", "test").is_err());
    }

    #[test]
    fn test_validate_topic_segment_empty() {
        assert!(validate_topic_segment("", "test").is_err());
    }

    #[test]
    fn test_new_with_invalid_cert_paths() {
        let config = MqttConfig {
            enabled: true,
            host: "localhost".to_string(),
            port: 8883,
            ca_cert_path: "/nonexistent/ca.pem".to_string(),
            client_cert_path: "/nonexistent/cert.pem".to_string(),
            client_key_path: "/nonexistent/key.pem".to_string(),
            env_prefix: "test".to_string(),
            telemetry_interval_s: 1.0,
        };

        let result = MqttTransport::new(&config, CancellationToken::new());
        assert!(matches!(result, Err(TransportError::Tls(_))));
    }
}
