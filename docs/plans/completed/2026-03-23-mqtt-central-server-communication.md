# MQTT Central Server Communication

## Overview
- Add bidirectional MQTT communication between drone units and a central server
- Drone publishes sensor telemetry (LTE, ping, CPU temp) and receives commands (config update, get config, update request, modem commands)
- Authentication via mutual TLS with client certificates issued by a private CA
- Node ID extracted from client certificate CN field
- Architecture: split `MqttTransport` (connection/TLS/reconnect/pub/sub) from `CommandProcessor` (command lifecycle/routing/status)
- Integration tests using dev containers with a Mosquitto broker

## Context (from discovery)
- **Existing telemetry flow**: Sensors → Context.sensors (RwLock) → TelemetryReporter → MAVLink COMMAND_LONG (1Hz)
- **Existing services pattern**: `services/modem_access.rs` — queue-based service with mpsc + worker task
- **Config pattern**: TOML with serde, all fields required, validated in `Config::validate()`
- **Task pattern**: `Task` trait with `run() -> Vec<JoinHandle>`, spawned in `main.rs`
- **Dependencies**: tokio (full), serde, toml, tracing, tokio-util (CancellationToken)
- **No MQTT code exists** — this is entirely new

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: required for every task (in `#[cfg(test)]` modules)
- **Integration tests**: dev container-based end-to-end tests in `unitctl/tests/` using Mosquitto broker
- Test TLS connection, telemetry publish/subscribe, command lifecycle, reconnection

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: Add dependencies and MQTT config types
- [x] Add `rumqttc` dependency to `unitctl/Cargo.toml`
- [x] Add `serde_json` dependency (for MQTT JSON payloads)
- [x] Add `uuid` dependency with `v4` feature (for command UUID generation)
- [x] Add `chrono` dependency with `serde` feature (for ISO 8601 timestamps)
- [x] Add `MqttConfig` struct to `config.rs` with fields: `enabled` (bool), `host` (String), `port` (u16), `ca_cert_path` (String), `client_cert_path` (String), `client_key_path`
(String), `env_prefix` (String, e.g. "prod"), `telemetry_interval_s` (f64)
- [x] Add `mqtt: MqttConfig` field to `Config` struct
- [x] Add `MqttConfig::default()` impl
- [x] Add MQTT validation in `Config::validate()` — validate cert paths are non-empty when enabled, port > 0
- [x] Update `test_config()` helper to include MQTT config
- [x] Add `[mqtt]` section to `config.toml.example`
- [x] Write tests for MQTT config parsing (valid config, missing fields, validation errors)
- [x] Run tests — must pass before next task

### Task 2: TLS certificate loading and node ID extraction
- [x] Create `unitctl/src/services/mqtt/mod.rs` with `pub mod transport;` and `pub mod commands;`
- [x] Add `pub mod mqtt;` to `unitctl/src/services/mod.rs`
- [x] Create `unitctl/src/services/mqtt/tls.rs` — module for TLS helpers
- [x] Implement `load_tls_config(ca_path, cert_path, key_path) -> Result<TlsConfiguration>` using rumqttc's `TlsConfiguration::Simple` (avoids rustls version mismatch)
- [x] Implement `extract_node_id(cert_path) -> Result<String>` — read PEM certificate, parse X.509, extract CN from subject
- [x] Add `x509-parser` dependency for certificate handling (rustls/rustls-pemfile not needed — rumqttc handles TLS internally)
- [x] Write tests for `extract_node_id` with a self-signed test certificate
- [x] Write tests for `load_tls_config` with valid/invalid cert paths
- [x] Run tests — must pass before next task

### Task 3: MqttTransport — connection, reconnect, pub/sub
- [x] Create `unitctl/src/services/mqtt/transport.rs`
- [x] Define `MqttTransport` struct holding `rumqttc::AsyncClient`, node ID, and env prefix
- [x] Implement `MqttTransport::new(config: &MqttConfig, cancel: CancellationToken) -> Result<Self>` — load TLS, extract node ID, create rumqttc `AsyncClient` + `EventLoop` with TLS
- [x] Implement `async fn run_event_loop(&self, ...)` — drive the rumqttc event loop in a tokio task, handle reconnection, log connection events
- [x] Implement `async fn publish(&self, topic: &str, payload: &[u8], qos: QoS, retain: bool) -> Result<()>`
- [x] Implement `async fn subscribe(&self, topic: &str, qos: QoS) -> Result<()>`
- [x] Define `MqttEvent` enum — `Connected`, `Disconnected`, `Message { topic: String, payload: Vec<u8> }` — and expose an event channel (`broadcast::Sender<MqttEvent>`)
- [x] Event loop forwards incoming publishes as `MqttEvent::Message` on the broadcast channel
- [x] Implement topic builder methods: `fn telemetry_topic(&self, name: &str) -> String` returns `{env_prefix}/nodes/{node_id}/telemetry/{name}`, `fn command_topic(&self, cmd: &str, suffix: &str) -> String` returns `{env_prefix}/nodes/{node_id}/cmnd/{cmd}/{suffix}`
- [x] Write tests for topic builder methods
- [x] Write tests for MqttTransport creation with mock/test config (unit test, no real broker)
- [x] Run tests — must pass before next task

### Task 4: Telemetry publisher — read sensors, publish JSON
- [x] Create `unitctl/src/services/mqtt/telemetry.rs`
- [x] Define `TelemetryPublisher` struct holding `Arc<MqttTransport>`, `Arc<Context>`, interval, cancel token
- [x] Implement `Task` trait for `TelemetryPublisher`
- [x] In `run()`, spawn a tokio task that periodically (configurable interval from `mqtt.telemetry_interval_s`):
  - Reads `ctx.sensors.lte`, serializes to JSON with `ts` field, publishes to `telemetry/lte`
  - Reads `ctx.sensors.ping`, serializes to JSON with `ts` field, publishes to `telemetry/ping`
  - Reads `ctx.sensors.cpu_temp`, serializes to JSON with `ts` field, publishes to `telemetry/cpu_temp`
- [x] Add `Serialize` derive to `PingReading`, `LteSignalQuality`, `LteNeighborCell`, `LteReading`, `CpuTempReading`
- [x] Skip publishing for sensors that have no reading yet (Option is None)
- [x] Write tests for telemetry JSON serialization (verify field names, ts format)
- [x] Write tests for TelemetryPublisher skipping None readings
- [x] Run tests — must pass before next task

### Task 5: Command framework — dispatcher, lifecycle, status
- [x] Create `unitctl/src/services/mqtt/commands.rs`
- [x] Define `CommandState` enum: `Accepted`, `InProgress`, `Completed`, `Failed`, `Rejected`, `Expired`, `Superseded`
- [x] Define `CommandEnvelope` struct: `uuid` (String), `issued_at` (DateTime), `ttl_sec` (u64), `payload` (serde_json::Value)
- [x] Define `CommandResult` struct: `ok` (bool), `ts` (DateTime), extra fields (serde_json::Value)
- [x] Define `CommandHandler` trait: `async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError>`
- [x] Define `CommandProcessor` struct holding `Arc<MqttTransport>`, registered handlers (HashMap<String, Box<dyn CommandHandler>>)
- [x] Implement `CommandProcessor::register(&mut self, name: &str, handler: impl CommandHandler)`
- [x] Implement `CommandProcessor::run()` — subscribe to `{prefix}/nodes/{node_id}/cmnd/+/in`, listen on MqttTransport's event channel for incoming messages
- [x] On incoming command message:
  1. Parse `CommandEnvelope` from JSON payload
  2. Check TTL — if expired (`issued_at + ttl_sec < now`), publish `expired` status, skip
  3. Publish `accepted` status to `.../status`
  4. Look up handler by command name (from topic)
  5. If no handler found, publish `rejected` status, skip
  6. Publish `in_progress` status
  7. Call handler
  8. On success: publish `completed` status + result to `.../result`
  9. On error: publish `failed` status + error to `.../result`
- [x] Write tests for CommandEnvelope deserialization (valid, expired, missing fields)
- [x] Write tests for command lifecycle state transitions (accepted → in_progress → completed)
- [x] Write tests for TTL expiry check
- [x] Write tests for rejected state when handler not found
- [x] Run tests — must pass before next task

### Task 6: Implement command handlers
- [x] Create `unitctl/src/services/mqtt/handlers/mod.rs` with submodules
- [x] Create `config_update` handler — receives config payload, applies changes (scope TBD, placeholder impl for now)
- [x] Create `get_config` handler — reads current config, returns as JSON result
- [x] Create `update_request` handler — receives update request payload (placeholder impl, logs and acknowledges)
- [x] Create `modem_commands` handler — receives AT command, routes through `ModemAccess` from Context, returns AT response
- [x] Write tests for each handler with mock Context
- [x] Write tests for modem_commands handler with mock ModemAccess
- [x] Run tests — must pass before next task

### Task 7: Wire MQTT service into main.rs and Context
- [x] Add `mqtt_transport: RwLock<Option<Arc<MqttTransport>>>` to Context (or pass directly)
- [x] In `main.rs`, if `config.mqtt.enabled`:
  1. Create `MqttTransport` (loads TLS, extracts node ID)
  2. Spawn transport event loop task
  3. Create `TelemetryPublisher`, spawn via `Task::run()`
  4. Create `CommandProcessor`, register all handlers, spawn
- [x] Add MQTT startup log line with broker host, port, node ID
- [x] Graceful shutdown: CancellationToken stops all MQTT tasks
- [x] Write test verifying MQTT tasks are not spawned when `mqtt.enabled = false`
- [x] Run tests — must pass before next task

### Task 8: Integration tests with dev containers
- [x] Create `unitctl/tests/mqtt_integration.rs`
- [x] Create `unitctl/tests/docker-compose.mqtt.yml` with Mosquitto broker container configured for mTLS
- [x] Generate test certificates (CA, server cert, client cert) as test fixtures in `unitctl/tests/fixtures/certs/`
- [x] Create Mosquitto config for mTLS (`unitctl/tests/fixtures/mosquitto.conf`)
- [x] Write integration test: connect to broker with TLS, verify connection established
- [x] Write integration test: publish telemetry message, subscribe and verify received
- [x] Write integration test: send command on `/in` topic, verify status transitions (accepted → in_progress → completed) and result published on `/result`
- [x] Write integration test: send expired command, verify `expired` status published
- [x] Write integration test: send command for unknown handler, verify `rejected` status
- [x] Write integration test: broker disconnect → reconnect → resume publishing
- [x] Add test runner script or cargo test configuration for dev container lifecycle
- [x] Run integration tests — must pass before next task

### Task 9: Verify acceptance criteria
- [x] Verify TLS mutual authentication works with private CA certificates
- [x] Verify node ID is correctly extracted from certificate CN
- [x] Verify telemetry messages are published with correct topic structure and JSON format
- [x] Verify command lifecycle: in → status(accepted) → status(in_progress) → status(completed/failed) → result
- [x] Verify expired commands are properly rejected
- [x] Verify unsupported commands get `rejected` status
- [x] Verify reconnection behavior on broker disconnect
- [x] Verify `mqtt.enabled = false` disables all MQTT functionality
- [x] Run full test suite (unit + integration)
- [x] Run linter (`cargo clippy`) — all issues must be fixed
- [x] Run `cargo fmt --check` — must pass

### Task 10: [Final] Update documentation
- [x] Update `config.toml.example` with MQTT section documentation/comments
- [x] Update CLAUDE.md architecture section to include MQTT service
- [x] Update CLAUDE.md key types with MQTT types
- [x] Update CLAUDE.md dependencies list

## Technical Details

### Architecture: Split Transport + Command Processor

```
                        ┌──────────────────────────────────────┐
                        │          MqttTransport               │
                        │  (rumqttc AsyncClient + EventLoop)   │
                        │  - TLS/mTLS connection               │
                        │  - Reconnect with backoff            │
                        │  - Publish / Subscribe               │
                        │  - Event broadcast channel           │
                        └──────────┬───────────────────────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    │              │              │
           ┌────────▼──────┐  ┌───▼──────────────▼───────┐
           │  Telemetry    │  │   CommandProcessor        │
           │  Publisher    │  │                            │
           │               │  │  Subscribe: .../cmnd/+/in │
           │  ctx.sensors  │  │  Route to handlers        │
           │  → JSON       │  │  Manage lifecycle:        │
           │  → publish    │  │  accepted → in_progress   │
           │               │  │  → completed/failed       │
           └───────────────┘  │                            │
                              │  ┌─────────────────────┐  │
                              │  │ Handlers:            │  │
                              │  │  - config_update     │  │
                              │  │  - get_config        │  │
                              │  │  - update_request    │  │
                              │  │  - modem_commands    │  │
                              │  └─────────────────────┘  │
                              └────────────────────────────┘
```

### MQTT Topic Structure

```
{env_prefix}/nodes/{nodeId}/telemetry/lte
{env_prefix}/nodes/{nodeId}/telemetry/ping
{env_prefix}/nodes/{nodeId}/telemetry/cpu_temp
{env_prefix}/nodes/{nodeId}/cmnd/{commandName}/in       ← server publishes command
{env_prefix}/nodes/{nodeId}/cmnd/{commandName}/status   ← drone publishes state
{env_prefix}/nodes/{nodeId}/cmnd/{commandName}/result   ← drone publishes result
```

### Command Lifecycle

```
Server publishes to .../cmnd/config_update/in
    │
    ▼
Drone receives, parses CommandEnvelope
    │
    ├─ TTL expired? ──▶ publish status: { state: "expired" } ──▶ STOP
    │
    ├─ publish status: { state: "accepted", uuid }
    │
    ├─ handler exists? ──NO──▶ publish status: { state: "rejected" } ──▶ STOP
    │
    ├─ publish status: { state: "in_progress", uuid }
    │
    ├─ call handler.handle(envelope)
    │
    ├─ OK ──▶ publish status: { state: "completed" }
    │         publish result: { ok: true, ... }
    │
    └─ Err ─▶ publish status: { state: "failed" }
              publish result: { ok: false, error: "..." }
```

### Telemetry JSON Payloads

**telemetry/lte:**
```json
{
  "ts": "2026-03-23T10:04:00Z",
  "rsrq": -10,
  "rsrp": -85,
  "rssi": -60,
  "rssnr": 15,
  "earfcn": 1300,
  "tx_power": 23,
  "pcid": 42
}
```

**telemetry/ping:**
```json
{
  "ts": "2026-03-23T10:04:00Z",
  "reachable": true,
  "latency_ms": 25.5,
  "loss_percent": 3
}
```

**telemetry/cpu_temp:**
```json
{
  "ts": "2026-03-23T10:04:00Z",
  "temperature_c": 42.5
}
```

### Configuration

```toml
[mqtt]
enabled = true
host = "mqtt.example.com"
port = 8883
ca_cert_path = "/etc/unitctl/certs/ca.pem"
client_cert_path = "/etc/unitctl/certs/client.pem"
client_key_path = "/etc/unitctl/certs/client.key"
env_prefix = "prod"
telemetry_interval_s = 1.0
```

### Dependencies to Add

```toml
rumqttc = { version = "0.24", features = ["use-rustls"] }
rustls = "0.23"
rustls-pemfile = "2"
x509-parser = "0.16"
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
```

## Post-Completion

**Manual verification:**
- Test with real Mosquitto broker using private CA certificates
- Verify mTLS handshake succeeds with valid cert, fails with invalid/expired cert
- Monitor telemetry messages with `mosquitto_sub` to verify JSON format and timing
- Test command round-trip: publish command via `mosquitto_pub`, observe status transitions
- Verify behavior on broker restart (reconnection)
- Load test: verify telemetry publishing doesn't degrade MAVLink performance

**Future extensions:**
- Drone → Server commands (unit sending commands to central server)
- Additional telemetry topics (GPS, battery)
- Command queuing during offline periods (store-and-forward)
- Rate limiting / backpressure on telemetry publishing
