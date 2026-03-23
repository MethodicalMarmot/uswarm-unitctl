use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use super::transport::{MqttEvent, MqttTransport};
use crate::context::Context;
use crate::services::mqtt::handlers::config_update::ConfigUpdateHandler;
use crate::services::mqtt::handlers::get_config::GetConfigHandler;
use crate::services::mqtt::handlers::modem_commands::ModemCommandsHandler;
use crate::services::mqtt::handlers::update_request::UpdateRequestHandler;
use crate::Task;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// State of a command in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Accepted,
    InProgress,
    Completed,
    Failed,
    Rejected,
    Expired,
    Superseded,
}

/// Incoming command envelope — the JSON payload on `.../cmnd/{name}/in`.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandEnvelope {
    pub uuid: String,
    pub issued_at: DateTime<Utc>,
    pub ttl_sec: u64,
    pub payload: serde_json::Value,
}

impl CommandEnvelope {
    /// Check if this command has expired.
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    /// Check if this command has expired relative to a given timestamp.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        // Cap TTL to avoid chrono::Duration::seconds() panic on overflow
        // (chrono stores nanoseconds internally, so i64::MAX seconds overflows).
        // 10 years is a generous upper bound for any command TTL.
        const MAX_TTL_SEC: i64 = 315_360_000;
        let ttl = i64::try_from(self.ttl_sec)
            .unwrap_or(MAX_TTL_SEC)
            .min(MAX_TTL_SEC);
        let expiry = self.issued_at + chrono::Duration::seconds(ttl);
        now > expiry
    }
}

/// Result of a command handler execution.
///
/// The `ok` field in the published result message is determined by the `Ok`/`Err`
/// return type of the handler, not by a field here.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Error returned by command handlers.
#[derive(Debug)]
pub struct CommandError {
    pub message: String,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CommandError {}

impl CommandError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Trait for handling a specific command type.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError>;
}

/// Status message published to `.../cmnd/{name}/status`.
#[derive(Debug, Serialize)]
struct StatusMessage {
    uuid: String,
    state: CommandState,
    ts: String,
}

/// Reserved keys that cannot be overridden by handler extra data.
const RESERVED_RESULT_KEYS: &[&str] = &["uuid", "ok", "ts", "error"];

/// Result message published to `.../cmnd/{name}/result`.
#[derive(Debug, Serialize)]
struct ResultMessage {
    uuid: String,
    ok: bool,
    ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Value,
}

/// Maximum number of recently-processed UUIDs to remember for deduplication.
/// QoS 1 redeliveries arrive quickly, so a modest window suffices.
const DEDUP_CAPACITY: usize = 256;

/// Capacity of the bounded bridge channel between the broadcast receiver and the
/// command processing loop.  This limits memory growth when slow handlers (e.g.
/// modem AT commands that block for up to 30 s) cause a backlog.  When the
/// channel is full, new events are dropped and logged rather than accumulated
/// without bound.
const BRIDGE_CHANNEL_CAPACITY: usize = 64;

/// Processes incoming MQTT commands, manages lifecycle, and routes to handlers.
pub struct CommandProcessor {
    transport: Arc<MqttTransport>,
    handlers: HashMap<String, Box<dyn CommandHandler>>,
    cancel: CancellationToken,
    /// Pre-created event receiver to avoid race with the event loop.
    /// Subscribed eagerly in `new()` so no messages are missed between
    /// event loop start and `run()`.
    event_rx: Mutex<tokio::sync::broadcast::Receiver<MqttEvent>>,
    /// Track recently-processed command UUIDs to prevent duplicate execution
    /// from QoS 1 redeliveries.
    seen_uuids: Mutex<HashSet<String>>,
    /// Insertion order for evicting the oldest UUID when at capacity.
    seen_order: Mutex<VecDeque<String>>,
}

impl Task for CommandProcessor {
    /// Run the command processor — process incoming messages and re-subscribe on reconnect.
    ///
    /// Internally spawns a bridge task that drains the broadcast channel into a
    /// bounded mpsc channel.  This decouples *receiving* events (which must keep
    /// up with the broadcast) from *processing* commands (which may block on slow
    /// handlers such as modem AT commands).  Without this bridge, a handler that
    /// takes longer than the time needed to fill the 256-slot broadcast buffer
    /// would cause `RecvError::Lagged`, silently dropping commands with no
    /// status/result feedback to the server.  The bridge channel is bounded
    /// (`BRIDGE_CHANNEL_CAPACITY`) so that a burst of commands during a slow
    /// handler does not grow memory without limit.  The bridge task uses an
    /// awaiting send (not `try_send`) so that backpressure propagates: the
    /// broadcast buffer (256 slots) acts as additional overflow before any
    /// messages are lost via the `Lagged` error path.
    fn run(self: Arc<Self>) -> Vec<JoinHandle<()>> {
        let (bridge_tx, bridge_rx) = tokio::sync::mpsc::channel(BRIDGE_CHANNEL_CAPACITY);
        let commands = Arc::clone(&self);
        let mut handles = vec![tokio::spawn(async move {
            commands.fill_bridge_queue(bridge_tx).await;
        })];

        let commands = Arc::clone(&self);
        handles.push(tokio::spawn(async move {
            commands.drain_commands(bridge_rx).await;
        }));

        handles
    }
}

impl CommandProcessor {
    pub fn new(transport: Arc<MqttTransport>, cancel: CancellationToken) -> Self {
        let event_rx = transport.subscribe_events();
        Self {
            transport,
            handlers: HashMap::new(),
            cancel,
            event_rx: Mutex::new(event_rx),
            seen_uuids: Mutex::new(HashSet::with_capacity(DEDUP_CAPACITY)),
            seen_order: Mutex::new(VecDeque::with_capacity(DEDUP_CAPACITY)),
        }
    }

    pub(crate) fn register_commands(&mut self, ctx: &Arc<Context>) {
        self.register(ConfigUpdateHandler::NAME, ConfigUpdateHandler::new());
        self.register(
            GetConfigHandler::NAME,
            GetConfigHandler::new(Arc::clone(ctx)),
        );
        self.register(UpdateRequestHandler::NAME, UpdateRequestHandler::new());
        self.register(
            ModemCommandsHandler::NAME,
            ModemCommandsHandler::new(Arc::clone(ctx)),
        );
    }

    /// Register a handler for a command name.
    pub fn register(&mut self, name: &str, handler: impl CommandHandler + 'static) {
        self.handlers.insert(name.to_string(), Box::new(handler));
    }

    /// Queue the initial wildcard SUBSCRIBE before the event loop starts.
    ///
    /// `AsyncClient::subscribe()` enqueues into rumqttc's internal request
    /// channel without requiring the event loop to be running. The SUBSCRIBE
    /// will be sent on the first `poll()` after ConnAck — not as part of the
    /// CONNECT handshake itself (rumqttc returns ConnAck before processing
    /// queued requests). With `clean_session=false` the broker retains
    /// subscriptions across reconnects, so this gap only matters for the
    /// very first connection with a brand-new session, where the window is
    /// a single event loop iteration.
    pub async fn subscribe_commands(&self) {
        let subscribe_topic = self.transport.command_topic("+", "in");

        if let Err(e) = self
            .transport
            .subscribe(&subscribe_topic, QoS::AtLeastOnce)
            .await
        {
            warn!("Failed initial subscribe to command topic {subscribe_topic}: {e}, will retry on connect");
        } else {
            info!(topic = %subscribe_topic, "CommandProcessor subscribed to commands");
        }
    }

    // Bridge: drain the bounded broadcast into a bounded mpsc so that
    // slow command handlers never cause the broadcast receiver to lag.
    // The bridge uses an awaiting send so that when the mpsc is full,
    // backpressure stalls the bridge task and the broadcast buffer
    // (256 slots) absorbs the overflow.  Only when both are exhausted
    // does the broadcast Lagged error fire, which is logged at error
    // level below.
    async fn fill_bridge_queue(&self, bridge_tx: Sender<MqttEvent>) {
        let mut event_rx = self.event_rx.lock().await;
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                event = (*event_rx).recv() => {
                    match event {
                        Ok(evt) => {
                            if bridge_tx.send(evt).await.is_err() {
                                break; // main loop dropped its receiver
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Even with the bridge this could theoretically
                            // happen if the bridge task itself is starved.
                            // Log at error level since it means command loss.
                            tracing::error!(
                                "CommandProcessor event bridge lagged, missed {n} broadcast events"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    }

    async fn drain_commands(&self, mut bridge_rx: Receiver<MqttEvent>) {
        let command_suffix = self.transport.command_topic("", "");

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!("CommandProcessor cancelled");
                    break;
                }
                event = bridge_rx.recv() => {
                    match event {
                        Some(MqttEvent::Message { topic, payload }) => {
                            if let Some(cmd_name) = Self::extract_command_name(&topic, &command_suffix) {
                                self.process_command(&cmd_name, &payload).await;
                            }
                        }
                        Some(MqttEvent::Connected) => {
                            // Re-subscribe after reconnection
                            self.subscribe_commands().await;
                        }
                        Some(MqttEvent::Disconnected) => {
                            debug!("CommandProcessor: transport disconnected");
                        }
                        None => {
                            info!("CommandProcessor: bridge channel closed");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Extract command name from topic like `{prefix}/nodes/{id}/cmnd/{name}/in`.
    fn extract_command_name(topic: &str, prefix: &str) -> Option<String> {
        if !topic.starts_with(prefix) || !topic.ends_with("/in") {
            return None;
        }
        let remainder = &topic[prefix.len()..];
        let name = remainder.strip_suffix("/in")?;
        if name.is_empty() || name.contains('/') || name.contains('+') || name.contains('#') {
            return None;
        }
        Some(name.to_string())
    }

    /// Record a UUID as processed. Returns `false` if already seen (duplicate).
    async fn record_uuid(&self, uuid: &str) -> bool {
        let mut seen_uuids = self.seen_uuids.lock().await;
        let mut seen_order = self.seen_order.lock().await;
        if seen_uuids.contains(uuid) {
            return false;
        }
        // Evict oldest if at capacity
        if seen_uuids.len() >= DEDUP_CAPACITY {
            if let Some(old) = (*seen_order).pop_front() {
                (*seen_uuids).remove(&old);
            }
        }
        (*seen_uuids).insert(uuid.to_string());
        (*seen_order).push_back(uuid.to_string());
        true
    }

    /// Process a single command message.
    async fn process_command(&self, cmd_name: &str, payload: &[u8]) {
        // Parse envelope
        let envelope: CommandEnvelope = match serde_json::from_slice(payload) {
            Ok(e) => e,
            Err(e) => {
                warn!(command = cmd_name, "Failed to parse command envelope: {e}");
                return;
            }
        };

        let uuid = &envelope.uuid;
        debug!(command = cmd_name, uuid = %uuid, "Processing command");

        // Deduplicate: QoS 1 may redeliver the same message
        if !self.record_uuid(uuid).await {
            debug!(command = cmd_name, uuid = %uuid, "Duplicate command, skipping");
            return;
        }

        // Check TTL
        if envelope.is_expired() {
            info!(command = cmd_name, uuid = %uuid, "Command expired");
            self.publish_status(cmd_name, uuid, CommandState::Expired)
                .await;
            return;
        }

        // Publish accepted (acknowledges receipt before handler lookup per spec)
        self.publish_status(cmd_name, uuid, CommandState::Accepted)
            .await;

        // Look up handler
        let handler = match self.handlers.get(cmd_name) {
            Some(h) => h,
            None => {
                warn!(command = cmd_name, uuid = %uuid, "No handler registered");
                self.publish_status(cmd_name, uuid, CommandState::Rejected)
                    .await;
                return;
            }
        };

        // Publish in_progress
        self.publish_status(cmd_name, uuid, CommandState::InProgress)
            .await;

        // Execute handler
        match handler.handle(&envelope).await {
            Ok(result) => {
                self.publish_status(cmd_name, uuid, CommandState::Completed)
                    .await;
                self.publish_result(cmd_name, uuid, true, None, result.extra)
                    .await;
            }
            Err(e) => {
                warn!(command = cmd_name, uuid = %uuid, error = %e, "Command failed");
                self.publish_status(cmd_name, uuid, CommandState::Failed)
                    .await;
                self.publish_result(
                    cmd_name,
                    uuid,
                    false,
                    Some(e.message),
                    serde_json::Value::Object(Default::default()),
                )
                .await;
            }
        }
    }

    /// Publish a status message to `.../cmnd/{name}/status`.
    async fn publish_status(&self, cmd_name: &str, uuid: &str, state: CommandState) {
        let topic = self.transport.command_topic(cmd_name, "status");
        let msg = StatusMessage {
            uuid: uuid.to_string(),
            state,
            ts: Utc::now().to_rfc3339(),
        };

        match serde_json::to_string(&msg) {
            Ok(json) => {
                if let Err(e) = self
                    .transport
                    .publish(&topic, json.as_bytes(), QoS::AtLeastOnce, false)
                    .await
                {
                    warn!(topic = %topic, "Failed to publish status: {e}");
                }
            }
            Err(e) => {
                warn!(topic = %topic, "Failed to serialize status message: {e}");
            }
        }
    }

    /// Strip reserved keys from extra data to prevent overwriting fixed fields.
    fn sanitize_extra(extra: serde_json::Value) -> serde_json::Value {
        if let serde_json::Value::Object(mut map) = extra {
            for key in RESERVED_RESULT_KEYS {
                map.remove(*key);
            }
            serde_json::Value::Object(map)
        } else if extra.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            // Wrap non-object values so #[serde(flatten)] can serialize them
            let mut map = serde_json::Map::new();
            map.insert("data".to_string(), extra);
            serde_json::Value::Object(map)
        }
    }

    /// Publish a result message to `.../cmnd/{name}/result`.
    async fn publish_result(
        &self,
        cmd_name: &str,
        uuid: &str,
        ok: bool,
        error: Option<String>,
        extra: serde_json::Value,
    ) {
        let topic = self.transport.command_topic(cmd_name, "result");
        let msg = ResultMessage {
            uuid: uuid.to_string(),
            ok,
            ts: Utc::now().to_rfc3339(),
            error,
            extra: Self::sanitize_extra(extra),
        };

        match serde_json::to_string(&msg) {
            Ok(json) => {
                if let Err(e) = self
                    .transport
                    .publish(&topic, json.as_bytes(), QoS::AtLeastOnce, false)
                    .await
                {
                    warn!(topic = %topic, "Failed to publish result: {e}");
                }
            }
            Err(e) => {
                warn!(topic = %topic, "Failed to serialize result message: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // --- CommandEnvelope deserialization tests ---

    #[test]
    fn test_envelope_deserialize_valid() {
        let json = r#"{
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 300,
            "payload": {"key": "value"}
        }"#;
        let envelope: CommandEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.uuid, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(envelope.ttl_sec, 300);
        assert_eq!(envelope.payload["key"], "value");
    }

    #[test]
    fn test_envelope_deserialize_empty_payload() {
        let json = r#"{
            "uuid": "test-uuid",
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 60,
            "payload": {}
        }"#;
        let envelope: CommandEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.uuid, "test-uuid");
        assert!(envelope.payload.is_object());
    }

    #[test]
    fn test_envelope_deserialize_missing_uuid() {
        let json = r#"{
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 60,
            "payload": {}
        }"#;
        let result: Result<CommandEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_envelope_deserialize_missing_ttl() {
        let json = r#"{
            "uuid": "test-uuid",
            "issued_at": "2026-03-23T10:00:00Z",
            "payload": {}
        }"#;
        let result: Result<CommandEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_envelope_deserialize_missing_payload() {
        let json = r#"{
            "uuid": "test-uuid",
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 60
        }"#;
        let result: Result<CommandEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- TTL expiry tests ---

    #[test]
    fn test_ttl_not_expired() {
        let now = Utc::now();
        let envelope = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: now,
            ttl_sec: 300,
            payload: serde_json::Value::Null,
        };
        assert!(!envelope.is_expired());
    }

    #[test]
    fn test_ttl_expired() {
        let issued = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let envelope = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: issued,
            ttl_sec: 60,
            payload: serde_json::Value::Null,
        };
        assert!(envelope.is_expired());
    }

    #[test]
    fn test_ttl_expired_at_specific_time() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let envelope = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: issued,
            ttl_sec: 60,
            payload: serde_json::Value::Null,
        };

        // 30 seconds later — not expired
        let t1 = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 30).unwrap();
        assert!(!envelope.is_expired_at(t1));

        // 61 seconds later — expired
        let t2 = Utc.with_ymd_and_hms(2026, 3, 23, 10, 1, 1).unwrap();
        assert!(envelope.is_expired_at(t2));
    }

    #[test]
    fn test_ttl_zero_expires_immediately() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let envelope = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: issued,
            ttl_sec: 0,
            payload: serde_json::Value::Null,
        };
        // Any time after issued_at should be expired
        let later = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 1).unwrap();
        assert!(envelope.is_expired_at(later));
    }

    // --- CommandState serialization tests ---

    #[test]
    fn test_command_state_serialization() {
        assert_eq!(
            serde_json::to_string(&CommandState::Accepted).unwrap(),
            "\"accepted\""
        );
        assert_eq!(
            serde_json::to_string(&CommandState::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&CommandState::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&CommandState::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&CommandState::Rejected).unwrap(),
            "\"rejected\""
        );
        assert_eq!(
            serde_json::to_string(&CommandState::Expired).unwrap(),
            "\"expired\""
        );
    }

    // --- CommandResult serialization tests ---

    #[test]
    fn test_command_result_serialization() {
        let result = CommandResult {
            extra: serde_json::json!({"data": "test"}),
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["data"], "test");
    }

    #[test]
    fn test_command_result_empty_extra() {
        let result = CommandResult {
            extra: serde_json::json!({}),
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert!(json.is_object());
    }

    // --- extract_command_name tests ---

    #[test]
    fn test_extract_command_name_valid() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "prod/nodes/drone-42/cmnd/config_update/in";
        assert_eq!(
            CommandProcessor::extract_command_name(topic, prefix),
            Some("config_update".to_string())
        );
    }

    #[test]
    fn test_extract_command_name_different_command() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "prod/nodes/drone-42/cmnd/get_config/in";
        assert_eq!(
            CommandProcessor::extract_command_name(topic, prefix),
            Some("get_config".to_string())
        );
    }

    #[test]
    fn test_extract_command_name_wrong_suffix() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "prod/nodes/drone-42/cmnd/config_update/status";
        assert_eq!(CommandProcessor::extract_command_name(topic, prefix), None);
    }

    #[test]
    fn test_extract_command_name_wrong_prefix() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "staging/nodes/drone-42/cmnd/config_update/in";
        assert_eq!(CommandProcessor::extract_command_name(topic, prefix), None);
    }

    #[test]
    fn test_extract_command_name_empty_name() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "prod/nodes/drone-42/cmnd//in";
        assert_eq!(CommandProcessor::extract_command_name(topic, prefix), None);
    }

    #[test]
    fn test_extract_command_name_nested_slash() {
        let prefix = "prod/nodes/drone-42/cmnd/";
        let topic = "prod/nodes/drone-42/cmnd/nested/cmd/in";
        assert_eq!(CommandProcessor::extract_command_name(topic, prefix), None);
    }

    // --- StatusMessage serialization tests ---

    #[test]
    fn test_status_message_serialization() {
        let msg = StatusMessage {
            uuid: "test-uuid".to_string(),
            state: CommandState::Accepted,
            ts: "2026-03-23T10:00:00Z".to_string(),
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["uuid"], "test-uuid");
        assert_eq!(json["state"], "accepted");
        assert_eq!(json["ts"], "2026-03-23T10:00:00Z");
    }

    // --- Command lifecycle integration test (with mock handler) ---

    struct EchoHandler;

    #[async_trait]
    impl CommandHandler for EchoHandler {
        async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
            Ok(CommandResult {
                extra: envelope.payload.clone(),
            })
        }
    }

    struct FailHandler;

    #[async_trait]
    impl CommandHandler for FailHandler {
        async fn handle(&self, _envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
            Err(CommandError::new("something went wrong"))
        }
    }

    #[tokio::test]
    async fn test_process_command_lifecycle_success() {
        // Set up transport for test
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);

        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let mut processor = CommandProcessor::new(transport, cancel);
        processor.register("echo", EchoHandler);

        // Build a valid command envelope
        let envelope = serde_json::json!({
            "uuid": "test-uuid-123",
            "issued_at": Utc::now().to_rfc3339(),
            "ttl_sec": 300,
            "payload": {"message": "hello"}
        });
        let payload = serde_json::to_vec(&envelope).unwrap();

        // process_command won't panic — publishes will fail silently (no broker)
        // but the lifecycle logic runs correctly
        processor.process_command("echo", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_expired() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);

        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        // Build an expired command
        let envelope = serde_json::json!({
            "uuid": "expired-uuid",
            "issued_at": "2020-01-01T00:00:00Z",
            "ttl_sec": 60,
            "payload": {}
        });
        let payload = serde_json::to_vec(&envelope).unwrap();

        // Should detect expired and publish expired status (publish fails silently, no broker)
        processor.process_command("anything", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_rejected_no_handler() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);

        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);
        // No handlers registered

        let envelope = serde_json::json!({
            "uuid": "reject-uuid",
            "issued_at": Utc::now().to_rfc3339(),
            "ttl_sec": 300,
            "payload": {}
        });
        let payload = serde_json::to_vec(&envelope).unwrap();

        // Should publish accepted then rejected (no handler found)
        processor.process_command("unknown_cmd", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_handler_failure() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);

        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let mut processor = CommandProcessor::new(transport, cancel);
        processor.register("fail_cmd", FailHandler);

        let envelope = serde_json::json!({
            "uuid": "fail-uuid",
            "issued_at": Utc::now().to_rfc3339(),
            "ttl_sec": 300,
            "payload": {}
        });
        let payload = serde_json::to_vec(&envelope).unwrap();

        // Should go: accepted -> in_progress -> failed
        processor.process_command("fail_cmd", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_invalid_json() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);

        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));

        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        // Invalid JSON payload — should log warning and return without panicking
        processor.process_command("test", b"not json").await;
    }

    // --- sanitize_extra tests ---

    #[test]
    fn test_sanitize_extra_strips_reserved_keys() {
        let extra = serde_json::json!({
            "uuid": "evil",
            "ok": false,
            "ts": "spoofed",
            "error": "injected",
            "data": "real"
        });
        let sanitized = CommandProcessor::sanitize_extra(extra);
        let obj = sanitized.as_object().unwrap();
        assert!(!obj.contains_key("uuid"));
        assert!(!obj.contains_key("ok"));
        assert!(!obj.contains_key("ts"));
        assert!(!obj.contains_key("error"));
        assert_eq!(obj["data"], "real");
    }

    #[test]
    fn test_sanitize_extra_null_returns_empty_object() {
        let sanitized = CommandProcessor::sanitize_extra(serde_json::Value::Null);
        assert!(sanitized.is_object());
        assert!(sanitized.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_sanitize_extra_non_object_wrapped() {
        let sanitized =
            CommandProcessor::sanitize_extra(serde_json::Value::String("hello".to_string()));
        let obj = sanitized.as_object().unwrap();
        assert_eq!(obj["data"], "hello");
    }

    #[test]
    fn test_command_error_display() {
        let err = CommandError::new("test error");
        assert_eq!(err.to_string(), "test error");
    }

    #[test]
    fn test_result_message_serialization_with_error() {
        let msg = ResultMessage {
            uuid: "test-uuid".to_string(),
            ok: false,
            ts: "2026-03-23T10:00:00Z".to_string(),
            error: Some("something failed".to_string()),
            extra: serde_json::json!({}),
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "something failed");
    }

    #[test]
    fn test_result_message_serialization_without_error() {
        let msg = ResultMessage {
            uuid: "test-uuid".to_string(),
            ok: true,
            ts: "2026-03-23T10:00:00Z".to_string(),
            error: None,
            extra: serde_json::json!({"data": 42}),
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["ok"], true);
        assert!(json.get("error").is_none());
        assert_eq!(json["data"], 42);
    }

    // --- UUID deduplication tests ---

    #[tokio::test]
    async fn test_record_uuid_deduplicates() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));
        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        assert!(processor.record_uuid("uuid-1").await);
        assert!(processor.record_uuid("uuid-2").await);
        // Duplicate should return false
        assert!(!processor.record_uuid("uuid-1").await);
        assert!(!processor.record_uuid("uuid-2").await);
    }

    #[tokio::test]
    async fn test_record_uuid_evicts_oldest_at_capacity() {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, _eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));
        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        // Fill to capacity
        for i in 0..DEDUP_CAPACITY {
            assert!(processor.record_uuid(&format!("uuid-{i}")).await);
        }
        // All should be seen
        assert!(!processor.record_uuid("uuid-0").await);

        // Adding one more should evict the oldest (uuid-0)
        assert!(processor.record_uuid("new-uuid").await);
        // uuid-0 was evicted, so it should be accepted again
        assert!(processor.record_uuid("uuid-0").await);
    }
}
