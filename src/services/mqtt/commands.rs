use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::transport::{MqttEvent, MqttTransport};
use crate::context::Context;
use crate::messages::commands::{
    CommandEnvelope, CommandResultData, CommandResultMsg, CommandState, CommandStatus,
};
use crate::services::mqtt::handlers::config_update::ConfigUpdateHandler;
use crate::services::mqtt::handlers::get_config::GetConfigHandler;
use crate::services::mqtt::handlers::modem_commands::ModemCommandsHandler;
use crate::services::mqtt::handlers::restart::{RestartHandler, TokioCommandRunner};
use crate::services::mqtt::handlers::update_request::UpdateRequestHandler;
use crate::Task;
use async_trait::async_trait;
use chrono::Utc;
use rumqttc::QoS;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Maximum number of recently-processed UUIDs to remember for deduplication.
/// QoS 1 redeliveries arrive quickly, so a modest window suffices.
const DEDUP_CAPACITY: usize = 256;

/// Capacity of the bounded bridge channel between the broadcast receiver and the
/// command processing loop.  This limits memory growth when slow handlers (e.g.
/// modem AT commands that block for up to 30 s) cause a backlog.  When the
/// channel is full, new events are dropped and logged rather than accumulated
/// without bound.
const BRIDGE_CHANNEL_CAPACITY: usize = 64;

/// Error returned by command handlers.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    pub message: String,
}

impl CommandError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Result returned by command handlers.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub data: CommandResultData,
}

/// Trait for handling a specific command type.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError>;
}

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
    dedup: Mutex<DedupQueue>,
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
            dedup: Mutex::new(DedupQueue::with_capacity(DEDUP_CAPACITY)),
        }
    }

    pub fn register_commands(&mut self, ctx: &Arc<Context>) {
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
        self.register(
            RestartHandler::NAME,
            RestartHandler::new(
                Arc::new(TokioCommandRunner),
                std::path::PathBuf::from(&ctx.config.general.env_dir),
            ),
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
                            debug!(topic = %topic, "CommandProcessor received message");
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
            warn!(topic = %topic, "Received message on topic that does not match command pattern");
            return None;
        }
        let remainder = &topic[prefix.len()..];
        let name = remainder.strip_suffix("/in")?;
        if name.is_empty() || name.contains('/') || name.contains('+') || name.contains('#') {
            warn!("Invalid command name in topic {topic}");
            return None;
        }
        Some(name.to_string())
    }

    /// Record a UUID as processed. Returns `false` if already seen (duplicate).
    async fn record_uuid(&self, uuid: &str) -> bool {
        self.dedup.lock().await.record(uuid)
    }

    /// Process a single command message.
    async fn process_command(&self, cmd_name: &str, payload: &[u8]) {
        // Parse envelope using the typed CommandEnvelope from messages module
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

        // Execute handler. The handler future is awaited inside a select against
        // the shutdown token so that long-parking handlers (e.g. restart{unitctl}
        // which awaits pending() until systemd terminates the process) do not
        // wedge the CommandProcessor task and block process exit on SIGTERM.
        tokio::select! {
            _ = self.cancel.cancelled() => {
                info!(command = cmd_name, uuid = %uuid, "CommandProcessor cancelled during handler execution");
            }
            res = handler.handle(&envelope) => {
                match res {
                    Ok(result) => {
                        self.publish_status(cmd_name, uuid, CommandState::Completed)
                            .await;
                        self.publish_result(cmd_name, uuid, true, None, Some(result.data))
                            .await;
                    }
                    Err(e) => {
                        warn!(command = cmd_name, uuid = %uuid, error = %e, "Command failed");
                        self.publish_status(cmd_name, uuid, CommandState::Failed)
                            .await;
                        self.publish_result(cmd_name, uuid, false, Some(e.message), None)
                            .await;
                    }
                }
            }
        }
    }

    /// Publish a status message to `.../cmnd/{name}/status` using `CommandStatus`
    /// from the messages module.
    async fn publish_status(&self, cmd_name: &str, uuid: &str, state: CommandState) {
        let topic = self.transport.command_topic(cmd_name, "status");
        let msg = CommandStatus {
            uuid: uuid.to_string(),
            state,
            ts: Utc::now(),
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

    /// Publish a result message to `.../cmnd/{name}/result` using `CommandResultMsg`
    /// from the messages module.
    async fn publish_result(
        &self,
        cmd_name: &str,
        uuid: &str,
        ok: bool,
        error: Option<String>,
        data: Option<CommandResultData>,
    ) {
        let topic = self.transport.command_topic(cmd_name, "result");
        let msg = CommandResultMsg {
            uuid: uuid.to_string(),
            ok,
            ts: Utc::now(),
            error,
            data,
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

/// Bounded deduplication queue that tracks recently-processed UUIDs.
struct DedupQueue {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl DedupQueue {
    fn with_capacity(cap: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(cap),
            order: VecDeque::with_capacity(cap),
        }
    }

    /// Record a UUID. Returns `false` if already seen (duplicate).
    fn record(&mut self, uuid: &str) -> bool {
        if self.seen.contains(uuid) {
            return false;
        }
        if self.seen.len() >= DEDUP_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        self.seen.insert(uuid.to_string());
        self.order.push_back(uuid.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::commands::{
        CommandPayload, GetConfigPayload, ModemCommandPayload, ModemCommandResult,
    };
    use chrono::TimeZone;

    // Helper: a valid GetConfig payload for deserialization tests
    fn get_config_payload_json() -> &'static str {
        r#"{"type": "GetConfig"}"#
    }

    // --- CommandEnvelope deserialization tests ---

    #[test]
    fn test_envelope_deserialize_valid() {
        let json = format!(
            r#"{{
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 300,
            "payload": {}
        }}"#,
            get_config_payload_json()
        );
        let envelope: CommandEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope.uuid, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(envelope.ttl_sec, 300);
    }

    #[test]
    fn test_envelope_deserialize_missing_uuid() {
        let json = format!(
            r#"{{
            "issued_at": "2026-03-23T10:00:00Z",
            "ttl_sec": 60,
            "payload": {}
        }}"#,
            get_config_payload_json()
        );
        let result: Result<CommandEnvelope, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn test_envelope_deserialize_missing_ttl() {
        let json = format!(
            r#"{{
            "uuid": "test-uuid",
            "issued_at": "2026-03-23T10:00:00Z",
            "payload": {}
        }}"#,
            get_config_payload_json()
        );
        let result: Result<CommandEnvelope, _> = serde_json::from_str(&json);
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

    fn test_envelope(ttl_sec: u64, issued_at: chrono::DateTime<Utc>) -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test".to_string(),
            issued_at,
            ttl_sec,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        }
    }

    #[test]
    fn test_ttl_not_expired() {
        let envelope = test_envelope(300, Utc::now());
        assert!(!envelope.is_expired());
    }

    #[test]
    fn test_ttl_expired() {
        let issued = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let envelope = test_envelope(60, issued);
        assert!(envelope.is_expired());
    }

    #[test]
    fn test_ttl_expired_at_specific_time() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let envelope = test_envelope(60, issued);

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
        let envelope = test_envelope(0, issued);
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
        assert_eq!(
            serde_json::to_string(&CommandState::Superseded).unwrap(),
            "\"superseded\""
        );
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

    // --- CommandStatus serialization tests (using messages module type) ---

    #[test]
    fn test_status_message_serialization() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let msg = CommandStatus {
            uuid: "test-uuid".to_string(),
            state: CommandState::Accepted,
            ts,
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["uuid"], "test-uuid");
        assert_eq!(json["state"], "accepted");
        assert!(json["ts"].as_str().unwrap().starts_with("2026-03-23"));
    }

    // --- CommandResultMsg serialization tests (using messages module type) ---

    #[test]
    fn test_result_message_serialization_with_error() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let msg = CommandResultMsg {
            uuid: "test-uuid".to_string(),
            ok: false,
            ts,
            error: Some("something failed".to_string()),
            data: None,
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "something failed");
        assert!(json.get("data").is_none());
    }

    #[test]
    fn test_result_message_serialization_without_error() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let msg = CommandResultMsg {
            uuid: "test-uuid".to_string(),
            ok: true,
            ts,
            error: None,
            data: Some(CommandResultData::ModemCommands(ModemCommandResult {
                command: "ATI".to_string(),
                response: "OK".to_string(),
            })),
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["ok"], true);
        assert!(json.get("error").is_none());
        assert!(json.get("data").is_some());
    }

    // --- Command lifecycle integration test (with mock handler) ---

    struct EchoHandler;

    #[async_trait]
    impl CommandHandler for EchoHandler {
        async fn handle(&self, _envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
            Ok(CommandResult {
                data: CommandResultData::ModemCommands(ModemCommandResult {
                    command: "echo".to_string(),
                    response: "ok".to_string(),
                }),
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

    fn make_test_envelope_json(uuid: &str) -> Vec<u8> {
        let envelope = serde_json::json!({
            "uuid": uuid,
            "issued_at": Utc::now().to_rfc3339(),
            "ttl_sec": 300,
            "payload": {"type": "GetConfig"}
        });
        serde_json::to_vec(&envelope).unwrap()
    }

    fn make_test_transport() -> (Arc<MqttTransport>, rumqttc::EventLoop) {
        let opts = rumqttc::MqttOptions::new("test", "localhost", 1883);
        let (client, eventloop) = rumqttc::AsyncClient::new(opts, 10);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let transport = Arc::new(MqttTransport::new_for_test(
            client,
            "drone-42".to_string(),
            "prod".to_string(),
            event_tx,
        ));
        (transport, eventloop)
    }

    #[tokio::test]
    async fn test_process_command_lifecycle_success() {
        let (transport, _eventloop) = make_test_transport();
        let cancel = CancellationToken::new();
        let mut processor = CommandProcessor::new(transport, cancel);
        processor.register("echo", EchoHandler);

        let payload = make_test_envelope_json("test-uuid-123");
        processor.process_command("echo", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_expired() {
        let (transport, _eventloop) = make_test_transport();
        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        let envelope = serde_json::json!({
            "uuid": "expired-uuid",
            "issued_at": "2020-01-01T00:00:00Z",
            "ttl_sec": 60,
            "payload": {"type": "GetConfig"}
        });
        let payload = serde_json::to_vec(&envelope).unwrap();
        processor.process_command("anything", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_rejected_no_handler() {
        let (transport, _eventloop) = make_test_transport();
        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        let payload = make_test_envelope_json("reject-uuid");
        processor.process_command("unknown_cmd", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_handler_failure() {
        let (transport, _eventloop) = make_test_transport();
        let cancel = CancellationToken::new();
        let mut processor = CommandProcessor::new(transport, cancel);
        processor.register("fail_cmd", FailHandler);

        let payload = make_test_envelope_json("fail-uuid");
        processor.process_command("fail_cmd", &payload).await;
    }

    #[tokio::test]
    async fn test_process_command_invalid_json() {
        let (transport, _eventloop) = make_test_transport();
        let cancel = CancellationToken::new();
        let processor = CommandProcessor::new(transport, cancel);

        // Invalid JSON payload — should log warning and return without panicking
        processor.process_command("test", b"not json").await;
    }

    #[test]
    fn test_command_error_display() {
        let err = CommandError::new("test error");
        assert_eq!(err.to_string(), "test error");
    }

    // --- UUID deduplication tests ---

    #[tokio::test]
    async fn test_record_uuid_deduplicates() {
        let (transport, _eventloop) = make_test_transport();
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
        let (transport, _eventloop) = make_test_transport();
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

    // --- Messages module integration tests ---

    #[test]
    fn test_command_envelope_uses_typed_payload() {
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::ModemCommands(ModemCommandPayload {
                command: "AT+CSQ".to_string(),
                timeout_ms: None,
            }),
        };
        assert!(!env.is_expired());

        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("AT+CSQ"));
    }

    #[test]
    fn test_publish_uses_messages_command_status() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
        let status = CommandStatus {
            uuid: "status-test".to_string(),
            state: CommandState::Completed,
            ts,
        };
        let json: serde_json::Value = serde_json::to_value(&status).unwrap();
        assert_eq!(json["uuid"], "status-test");
        assert_eq!(json["state"], "completed");
        assert!(json["ts"].as_str().unwrap().contains("2026"));
    }

    #[test]
    fn test_publish_uses_messages_command_result_msg() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
        let result = CommandResultMsg {
            uuid: "result-test".to_string(),
            ok: true,
            ts,
            error: None,
            data: Some(CommandResultData::ModemCommands(ModemCommandResult {
                command: "ATI".to_string(),
                response: "OK".to_string(),
            })),
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["uuid"], "result-test");
        assert_eq!(json["ok"], true);
        assert!(json.get("error").is_none());
        assert!(json.get("data").is_some());
    }
}
