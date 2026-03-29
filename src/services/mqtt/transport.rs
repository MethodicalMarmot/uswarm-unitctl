use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rumqttc::{
    AsyncClient, Event, EventLoop, LastWill, MqttOptions, Outgoing, Packet, QoS, Transport,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::tls;
use crate::config::MqttConfig;
use crate::messages::status::{NodeStatusEnvelope, OfflineStatusData, StatusData};
use crate::Task;

/// Events emitted by the MQTT transport layer.
#[derive(Debug, Clone)]
pub enum MqttEvent {
    Connected,
    Disconnected,
    Message { topic: String, payload: Vec<u8> },
}

/// Errors from the MQTT transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("TLS error: {0}")]
    Tls(#[from] tls::TlsError),
    #[error("MQTT client error: {0}")]
    Client(#[from] rumqttc::ClientError),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
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

/// Generate a 6-character hex session ID using the `rand` crate.
fn generate_session_id() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: [u8; 3] = rng.random();
    format!("{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2])
}

/// MQTT transport layer handling connection, TLS, reconnection, publish/subscribe.
pub struct MqttTransport {
    client: AsyncClient,
    node_id: String,
    env_prefix: String,
    session_id: String,
    event_tx: broadcast::Sender<MqttEvent>,
    eventloop: Mutex<Option<EventLoop>>,
    cancel: CancellationToken,
    /// Timestamp of the most recent successful MQTT connection (ConnAck).
    /// `None` until the first ConnAck is received.
    last_connected_at: Mutex<Option<DateTime<Utc>>>,
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
        let mut eventloop = transport
            .eventloop
            .lock()
            .expect("eventloop mutex poisoned")
            .take()
            .expect("eventloop already taken — run() must only be called once");
        vec![tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = transport.cancel.cancelled() => {
                        info!("MQTT event loop cancelled, shutting down gracefully");
                        // Only publish an explicit offline status if we were ever connected.
                        // This prevents phantom retained Offline messages for sessions
                        // that never successfully came online.
                        let last_online = *transport.last_connected_at.lock().expect("last_connected_at mutex poisoned");
                        if let Some(last_online) = last_online {
                            let offline = NodeStatusEnvelope {
                                ts: Utc::now(),
                                data: StatusData::Offline(OfflineStatusData {
                                    last_session: transport.session_id.clone(),
                                    last_online,
                                }),
                            };
                            if let Ok(payload) = serde_json::to_string(&offline) {
                                let _ = transport.client.try_publish(
                                    transport.status_topic(),
                                    QoS::AtLeastOnce,
                                    true,
                                    payload.into_bytes(),
                                );
                            }
                        }
                        let _ = transport.client.try_disconnect();
                        // Flush queued messages until DISCONNECT is sent or timeout.
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                        loop {
                            match tokio::time::timeout_at(deadline, eventloop.poll()).await {
                                Ok(Ok(Event::Outgoing(Outgoing::Disconnect))) => break,
                                Ok(Ok(_)) => continue,
                                _ => break, // timeout or error
                            }
                        }
                        break;
                    }
                    event = eventloop.poll() => {
                        match event {
                            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                                info!(node_id = %transport.node_id, "MQTT connected");
                                *transport.last_connected_at.lock().expect("last_connected_at mutex poisoned") = Some(Utc::now());
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

        let session_id = generate_session_id();

        // Build the status topic and LWT (offline) payload
        let status_topic = Self::build_status_topic(&config.env_prefix, &node_id);
        let now = Utc::now();
        let lwt_envelope = NodeStatusEnvelope {
            ts: now,
            data: StatusData::Offline(OfflineStatusData {
                last_session: session_id.clone(),
                last_online: now,
            }),
        };
        let lwt_payload = serde_json::to_string(&lwt_envelope).map_err(|e| {
            TransportError::InvalidConfig(format!("failed to serialize LWT payload: {e}"))
        })?;

        let mut mqtt_options = MqttOptions::new(&node_id, &config.host, config.port);
        mqtt_options.set_keep_alive(Duration::from_secs(30));
        mqtt_options.set_transport(Transport::tls_with_config(tls_config));
        mqtt_options.set_clean_session(false);
        mqtt_options.set_last_will(LastWill::new(
            status_topic,
            lwt_payload,
            QoS::AtLeastOnce,
            true,
        ));

        let (client, eventloop) = AsyncClient::new(mqtt_options, 50);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        Ok(Self {
            client,
            node_id,
            env_prefix: config.env_prefix.clone(),
            session_id,
            event_tx,
            eventloop: Mutex::new(Some(eventloop)),
            cancel,
            last_connected_at: Mutex::new(None),
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
        if cmd.is_empty() && suffix.is_empty() {
            format!("{}/nodes/{}/cmnd", self.env_prefix, self.node_id)
        } else if suffix.is_empty() {
            format!("{}/nodes/{}/cmnd/{}", self.env_prefix, self.node_id, cmd)
        } else {
            format!(
                "{}/nodes/{}/cmnd/{}/{}",
                self.env_prefix, self.node_id, cmd, suffix
            )
        }
    }

    /// Build a status topic path from stored fields.
    ///
    /// Returns `{env_prefix}/nodes/{node_id}/status`
    pub fn status_topic(&self) -> String {
        Self::build_status_topic(&self.env_prefix, &self.node_id)
    }

    /// Build a status topic path from raw parts (usable before `self` exists).
    fn build_status_topic(env_prefix: &str, node_id: &str) -> String {
        format!("{env_prefix}/nodes/{node_id}/status")
    }

    /// Get the node ID extracted from the client certificate.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Get the session ID generated for this transport instance.
    pub fn session_id(&self) -> &str {
        &self.session_id
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
            session_id: "test01".to_string(),
            event_tx,
            eventloop: Mutex::new(Some(eventloop)),
            cancel: CancellationToken::new(),
            last_connected_at: Mutex::new(None),
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
            session_id: "abcdef".to_string(),
            event_tx,
            eventloop: Mutex::new(Some(eventloop)),
            cancel: CancellationToken::new(),
            last_connected_at: Mutex::new(None),
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

    #[test]
    fn test_status_topic() {
        let transport = make_transport_fields("drone-42", "prod");
        assert_eq!(transport.status_topic(), "prod/nodes/drone-42/status");
    }

    #[test]
    fn test_status_topic_different_env() {
        let transport = make_transport_fields("node-abc", "staging");
        assert_eq!(transport.status_topic(), "staging/nodes/node-abc/status");
    }

    #[test]
    fn test_session_id_accessor() {
        let transport = make_transport_fields("drone-42", "prod");
        assert_eq!(transport.session_id(), "abcdef");
    }

    #[test]
    fn test_generate_session_id_format() {
        let id = generate_session_id();
        assert_eq!(id.len(), 6, "session ID should be 6 characters");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "session ID should be hex: {id}"
        );
    }
}
