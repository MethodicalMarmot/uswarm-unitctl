# MQTT Node Status LWT

## Overview
- Implement an MQTT status topic for node presence detection at `{env_prefix}/nodes/{node_id}/status`
- Uses MQTT Last Will and Testament (LWT) to automatically publish an offline status (retained) when the broker detects a dropped connection
- Publishes an online status (retained) on each successful MQTT connection/reconnection
- Enables other systems (central server, monitoring) to know whether a node is alive in real-time
- Integrates naturally alongside existing telemetry and command topics

### Wire Format

**Online (published on connect, retained):**
```json
{
  "ts": "2026-03-23T10:00:00Z",
  "data": {
    "type": "Online",
    "session": "a8f2c1",
    "version": "0.1.0"
  }
}
```

**Offline (LWT, retained, set before connect):**
```json
{
  "ts": "2026-03-23T12:00:00Z",
  "data": {
    "type": "Offline",
    "last_session": "a8f2c1",
    "last_online": "2026-03-23T10:00:00Z"
  }
}
```

## Context (from discovery)
- **Files/components involved:**
  - `src/messages/telemetry.rs` — envelope/data pattern to follow
  - `src/messages/commands.rs` — serde patterns, JsonSchema derives
  - `src/messages/mod.rs` — module re-exports
  - `src/services/mqtt/transport.rs` — MqttTransport, MqttOptions setup, topic builders
  - `src/services/mqtt/telemetry.rs` — TelemetryPublisher pattern (Task trait, publish loop)
  - `src/services/mqtt/mod.rs` — module exports
  - `src/main.rs` — MQTT initialization and task wiring
  - `src/config.rs` — MqttConfig struct (no new fields needed)
  - `src/bin/generate_schema.rs` — JSON schema generation
- **Related patterns found:**
  - `TelemetryEnvelope { ts, data: TelemetryData }` with `#[serde(tag = "type")]` enum
  - All message types derive `Debug, Clone, Serialize, Deserialize, JsonSchema`
  - `TelemetryPublisher` implements `Task` trait, spawns tokio task with cancel select
  - Topic builders: `telemetry_topic(name)`, `command_topic(cmd, suffix)`
  - `MqttEvent::Connected` broadcast on each `ConnAck` — used to trigger actions on connect
  - `CommandProcessor` is created before `transport.run()` to avoid missing first `ConnAck`
- **Dependencies identified:**
  - `rumqttc` — `MqttOptions::set_last_will()`, `LastWill` struct
  - `chrono` — `DateTime<Utc>` for timestamps
  - `schemars` — `JsonSchema` derive for schema generation
  - `serde`/`serde_json` — serialization
  - No new crate dependencies needed

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: required for every task (see Development Approach above)
- Message types: round-trip serde tests, JSON field verification, schema generation
- Transport: topic builder test for `status_topic()`, LWT construction test
- StatusPublisher: verify online payload structure, verify publish-on-connect behavior

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this codebase - code changes, tests, documentation updates
- **Post-Completion** (no checkboxes): items requiring external action - manual testing, changes in consuming projects, deployment configs, third-party verifications

## Implementation Steps

### Task 1: Define status message types in `src/messages/status.rs`
- [x] Create `src/messages/status.rs` with `NodeStatusEnvelope` struct: `ts: DateTime<Utc>`, `data: StatusData`
- [x] Define `StatusData` as `#[serde(tag = "type")]` enum with two variants: `Online(OnlineStatusData)` and `Offline(OfflineStatusData)` — following `TelemetryData` pattern
- [x] Define `OnlineStatusData` struct with fields: `session: String`, `version: String`
- [x] Define `OfflineStatusData` struct with fields: `last_session: String`, `last_online: DateTime<Utc>`
- [x] Derive `Debug, Clone, Serialize, Deserialize, JsonSchema` on all types
- [x] Add `pub mod status;` to `src/messages/mod.rs`
- [x] Write tests: round-trip serde for online payload, verify JSON has `"type": "Online"` tag and correct fields
- [x] Write tests: round-trip serde for offline payload, verify JSON has `"type": "Offline"` tag and correct fields
- [x] Write tests: verify online payload has no `last_session`/`last_online` fields, offline has no `session`/`version` fields
- [x] Write test: JSON schema generation (`schema_for!(NodeStatusEnvelope)`)
- [x] Run `cargo test` — must pass before next task

### Task 2: Add status topic builder and session ID to `MqttTransport`
- [x] Add `session_id: String` field to `MqttTransport` struct
- [x] Add `generate_session_id() -> String` helper function using `rand` crate (3 random bytes → 6-char hex)
- [x] Generate session ID in `MqttTransport::new()` and store it
- [x] Add `status_topic(&self) -> String` method returning `{env_prefix}/nodes/{node_id}/status`
- [x] Build LWT in `MqttTransport::new()`: create `StatusData::Offline(OfflineStatusData { last_session, last_online })` wrapped in `NodeStatusEnvelope`, serialize to JSON, set via `mqtt_options.set_last_will(LastWill { topic, message, qos: AtLeastOnce, retain: true })`
- [x] Add `session_id(&self) -> &str` accessor method
- [x] Update `new_for_test()` to include `session_id` field
- [x] Write test: `status_topic()` returns correct format `{env_prefix}/nodes/{node_id}/status`
- [x] Write test: `session_id()` accessor returns stored value
- [x] Write test: `generate_session_id()` returns 6-char hex string
- [x] Run `cargo test` — must pass before next task

### Task 3: Create `StatusPublisher` in `src/services/mqtt/status.rs`
- [x] Create `src/services/mqtt/status.rs` with `StatusPublisher` struct holding `Arc<MqttTransport>` and `CancellationToken`
- [x] Implement `StatusPublisher::new(transport, cancel)` constructor
- [x] Implement `publish_online(&self)` method: build `NodeStatusEnvelope` with `StatusData::Online(OnlineStatusData { session, version })`, serialize to JSON, publish to `status_topic()` with `QoS::AtLeastOnce` and `retain: true`
- [x] Implement `Task` trait for `StatusPublisher`: subscribe to `MqttEvent` broadcast, on each `MqttEvent::Connected` call `publish_online()`, select with cancellation token
- [x] Add `pub mod status;` to `src/services/mqtt/mod.rs`
- [x] Write test: verify `publish_online()` produces correct JSON payload structure
- [x] Write test: verify the published topic matches `status_topic()` format
- [x] Run `cargo test` — must pass before next task

### Task 4: Wire `StatusPublisher` into `main.rs`
- [x] Import `StatusPublisher` in `main.rs`
- [x] Create `StatusPublisher` after `MqttTransport` creation but before `transport.run()` (same pattern as `CommandProcessor` — must subscribe to broadcast before event loop starts)
- [x] Spawn `StatusPublisher` via `Arc::new(publisher).run()` and extend `handles`
- [x] Write test or verify: existing integration tests still pass with the new wiring
- [x] Run `cargo test` — must pass before next task

### Task 5: Update schema generation
- [x] Import `NodeStatusEnvelope` in `src/bin/generate_schema.rs`
- [x] Add `status/` directory creation in `generate_all_schemas()`
- [x] Write `NodeStatusEnvelope` schema to `assets/schema/status/envelope.json`
- [x] Update test `generate_and_write_schemas` to verify `status/envelope.json` exists
- [x] Update test `schemas_are_valid_json_schema` to include `status/envelope.json` in the schema files list
- [x] Run `cargo test` — must pass before next task

### Task 6: Verify acceptance criteria
- [x] Verify online payload JSON matches specified wire format exactly
- [x] Verify offline (LWT) payload JSON matches specified wire format exactly
- [x] Verify `skip_serializing_if` produces clean payloads (no null optional fields)
- [x] Verify LWT is set with `retain: true` and `QoS::AtLeastOnce`
- [x] Verify online status is published as retained on each `ConnAck`
- [x] Verify status topic format is `{env_prefix}/nodes/{node_id}/status`
- [x] Verify session ID is 6-char hex string
- [x] Run full test suite (`cargo test`)
- [x] Run linter (`cargo clippy`) — all issues must be fixed
- [x] Run format check (`cargo fmt --check`)

### Task 7: [Final] Update documentation
- [x] Update CLAUDE.md with new types: `NodeStatusEnvelope`, `StatusData`, `StatusPublisher`
- [x] Update CLAUDE.md MQTT Service section to mention status/LWT functionality
- [x] Update CLAUDE.md Key Types section with new types
- [x] Clean up temporary files (`context.md`, `architecture.md`)

## Technical Details

### Architecture Decisions
- **`StatusData` as `#[serde(tag = "type")]` enum** with `Online(OnlineStatusData)` and `Offline(OfflineStatusData)` variants — follows the `TelemetryData` pattern, each variant has only its own fields (no `Option` wrappers needed)
- **Session ID in `MqttTransport`** — must exist before `AsyncClient::new()` to build the LWT payload; generated from `/dev/urandom` (3 bytes → 6-char hex)
- **`StatusPublisher` as separate `Task`** — follows the `TelemetryPublisher` pattern; listens for `MqttEvent::Connected` to publish online status on each connect/reconnect
- **LWT frozen at connect time** — rumqttc doesn't support changing LWT after `MqttOptions` creation; `last_session` and `last_online` in the offline LWT reflect the initial connection, not subsequent reconnects. This is acceptable per user decision.

### Key Data Structures
```rust
// messages/status.rs
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeStatusEnvelope {
    pub ts: DateTime<Utc>,
    pub data: StatusData,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum StatusData {
    Online(OnlineStatusData),
    Offline(OfflineStatusData),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OnlineStatusData {
    pub session: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OfflineStatusData {
    pub last_session: String,
    pub last_online: DateTime<Utc>,
}
```

### Processing Flow
```
MqttTransport::new()
  ├── generate session_id (6-char hex)
  ├── build offline StatusData::offline(session_id, now)
  ├── serialize to JSON → set as LWT (retained, QoS 1)
  └── store session_id in MqttTransport

StatusPublisher (Task)
  └── loop: wait for MqttEvent::Connected
        └── build StatusData::online(session_id, version)
        └── publish to status_topic() (retained, QoS 1)

On broker disconnect:
  └── broker publishes LWT → offline status appears on status topic
```

### Module Layout
```
src/messages/status.rs     (NEW) — NodeStatusEnvelope, StatusData
src/messages/mod.rs        (MOD) — add `pub mod status;`
src/services/mqtt/status.rs (NEW) — StatusPublisher
src/services/mqtt/mod.rs   (MOD) — add `pub mod status;`
src/services/mqtt/transport.rs (MOD) — session_id, LWT setup, status_topic()
src/main.rs                (MOD) — wire StatusPublisher
src/bin/generate_schema.rs (MOD) — add status schema generation
```

### Error Handling
- `generate_session_id()`: falls back to timestamp-based ID if `/dev/urandom` read fails
- LWT serialization failure in `MqttTransport::new()`: return `TransportError::InvalidConfig`
- Online status publish failure: log warning, do not crash (same pattern as telemetry publishing)

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification:**
- Test with a real MQTT broker: connect, verify online status appears retained on status topic
- Kill the unitctl process abruptly, verify LWT offline status appears on status topic
- Reconnect, verify online status replaces offline status
- Verify retained messages persist across broker restarts

**External system updates:**
- Consuming projects (central server, monitoring) should subscribe to `{env_prefix}/nodes/+/status` to track node presence
- Update any API documentation or integration guides with the new status topic format
