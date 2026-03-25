# MQTT message structs and JSON Schema generation

## Overview
- Define typed Rust structs for all MQTT messages (telemetry + commands) in a dedicated `messages/` module
- Replace existing generic `serde_json::Value` payloads in `CommandEnvelope` and `CommandResult` with internally-tagged enums (`CommandPayload`, `CommandResultData`) that carry per-command typed variants
- Replace existing sensor reading structs (`PingReading`, `LteReading`, `LteSignalQuality`, `LteNeighborCell`, `CpuTempReading`) with message structs using a non-generic `TelemetryEnvelope` with an internally-tagged `TelemetryData` enum
- Add `schemars` derive to all message structs and generate unified JSON Schema files via `build.rs` into `assets/schema/`

## Context (from discovery)
- **Telemetry messages**: 3 types (lte, ping, cpu_temp), currently using sensor structs + runtime `ts` injection via `build_telemetry_json()`
- **Command messages**: 4 commands (get_config, config_update, update_request, modem_commands), originally with generic `CommandEnvelope { payload: serde_json::Value }` and `CommandResult { extra: serde_json::Value }`, migrated to `CommandEnvelope { payload: CommandPayload }` and `CommandResult { data: CommandResultData }`
- **Command lifecycle**: envelope in → status updates (accepted/in_progress/completed/failed/rejected/expired) → result out
- **Topic patterns**: `{env_prefix}/nodes/{node_id}/telemetry/{name}` and `{env_prefix}/nodes/{node_id}/cmnd/{name}/{in|status|result}`
- **Placeholder commands**: `config_update` and `update_request` handlers are placeholders but need properly named structs

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility during migration

## Testing Strategy
- **Unit tests**: required for every task
- Serialization round-trip tests for all message structs
- JSON Schema generation verification in tests
- Existing telemetry and command tests must be updated to use new types
- **Integration tests**: MQTT broker tests run via `testcontainers` (no external scripts or docker-compose needed) — `cargo test --test mqtt_integration`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: Create `messages` module with telemetry structs
- [x] create `unitctl/src/messages/mod.rs` with submodule declarations
- [x] create `unitctl/src/messages/telemetry.rs` with telemetry message structs:
  - `TelemetryEnvelope { ts: DateTime<Utc>, data: TelemetryData }` — non-generic envelope carrying timestamp and tagged telemetry data
  - `TelemetryData` — `#[serde(tag = "type")]` enum with variants: `Ping(PingTelemetry)`, `Lte(LteTelemetry)`, `CpuTemp(CpuTempTelemetry)`
  - `PingTelemetry { reachable: bool, latency_ms: f64, loss_percent: u8 }`
  - `LteTelemetry { signal: LteSignalQuality, neighbors: Vec<LteNeighborCell> }` — signal fields grouped into `LteSignalQuality` substruct, with `From<LteReading>` conversion
  - `LteSignalQuality { rsrq, rsrp, rssi, rssnr, earfcn, tx_power, pcid: i32 }`
  - `LteNeighborCell { pcid: i32, rsrp: i32, rsrq: i32, rssi: i32, rssnr: i32, earfcn: i32, last_seen: u64 }`
  - `CpuTempTelemetry { temperature_c: f64 }`
- [x] all structs derive `Debug, Clone, Serialize, Deserialize, JsonSchema`
- [x] add `schemars` dependency to `Cargo.toml`
- [x] register `mod messages` in `main.rs`
- [x] write serialization round-trip tests for each telemetry struct
- [x] run tests — must pass before next task

### Task 2: Create command message structs
- [x] create `SafeConfig` struct (in messages module) — mirrors `Config` but omits/redacts sensitive fields (TLS cert paths, keys). Include a `From<&Config>` impl.
- [x] create `unitctl/src/messages/commands.rs` with per-command types:
  - **get_config**: `GetConfigPayload {}` (empty), `GetConfigResult { config: SafeConfig }` — where `SafeConfig` is derived from `Config` with sensitive fields (cert paths, keys) redacted
  - **config_update**: `ConfigUpdatePayload { payload: serde_json::Value }` (placeholder), `ConfigUpdateResult { message: String, fields_received: Vec<String> }`
  - **update_request**: `UpdateRequestPayload { version: String, url: String }` (placeholder), `UpdateRequestResult { message: String, version: String }`
  - **modem_commands**: `ModemCommandPayload { command: String, timeout_ms: Option<u32> }`, `ModemCommandResult { command: String, response: String }`
- [x] create shared command envelope and result wrappers:
  - `CommandEnvelope { uuid: String, issued_at: DateTime<Utc>, ttl_sec: u64, payload: CommandPayload }` — non-generic, uses tagged enum for payload
  - `CommandPayload` — `#[serde(tag = "type")]` enum with variants: `GetConfig`, `ConfigUpdate`, `ModemCommands`, `UpdateRequest`
  - `CommandStatus { uuid: String, state: CommandState, ts: DateTime<Utc> }`
  - `CommandResultMsg { uuid: String, ok: bool, ts: DateTime<Utc>, error: Option<String>, data: Option<CommandResultData> }` — non-generic, data is optional (None on error)
  - `CommandResultData` — `#[serde(tag = "type")]` enum with variants: `GetConfig`, `ConfigUpdate`, `ModemCommands`, `UpdateRequest`
- [x] all structs derive `Debug, Clone, Serialize, Deserialize, JsonSchema`
- [x] write serialization round-trip tests for each command struct
- [x] run tests — must pass before next task

### Task 3: Add JSON Schema generation in build.rs
- [x] add `schemars` to `[build-dependencies]` in `Cargo.toml`
- [x] create `unitctl/build.rs` that:
  - creates directory structure for schema output (`telemetry/`, `command/`)
  - Note: build.rs creates directory structure only; actual schema generation is in `messages::schema::generate_all_schemas()` (called from tests) since build.rs runs before crate compilation and cannot import crate types
  - generates unified JSON Schema files (one per top-level type, containing all variants via `oneOf`/definitions):
    - `assets/schema/telemetry/envelope.json` — `TelemetryEnvelope` with `TelemetryData` variants
    - `assets/schema/command/envelope.json` — `CommandEnvelope` with `CommandPayload` variants
    - `assets/schema/command/status.json` — `CommandStatus`
    - `assets/schema/command/result.json` — `CommandResultMsg` with `CommandResultData` variants
  - Per-type schemas (`telemetry/ping.json`, `command/get_config/payload.json`, etc.) removed — all variants are embedded in the unified schemas
- [x] create `unitctl/assets/schema/` directory
- [x] verify build produces correct schema files
- [x] write test that validates generated schemas are valid JSON Schema
- [x] run tests — must pass before next task

### Task 4: Migrate sensor structs to use message types
- [x] update `sensors/ping.rs` — replace `PingReading` with `messages::telemetry::PingTelemetry` (type alias `PingReading = PingTelemetry` for backward compat)
- [x] update `sensors/lte.rs` — replace `LteNeighborCell` with `messages::telemetry::LteNeighborCell` (re-exported). `LteSignalQuality` and `LteReading` kept as sensor-internal types (LteReading uses HashMap for neighbor tracking; removal deferred to Task 8)
- [x] update `sensors/cpu_temp.rs` — replace `CpuTempReading` with `messages::telemetry::CpuTempTelemetry` (type alias `CpuTempReading = CpuTempTelemetry` for backward compat)
- [x] update `sensors/mod.rs` `SensorValues` to use `PingTelemetry` and `CpuTempTelemetry`
- [x] update `context.rs` sensor value references and tests to use new type names
- [x] update all sensor tests (existing tests pass with type aliases; context tests updated to use PingTelemetry/CpuTempTelemetry)
- [x] run tests — must pass before next task

### Task 5: Migrate TelemetryPublisher to use message types directly
- [x] update `services/mqtt/telemetry.rs` — remove `build_telemetry_json()` function, wrap sensor data in `TelemetryEnvelope` with `TelemetryData` enum variants at publish time
- [x] update `publish_one` to serialize non-generic `TelemetryEnvelope` directly (no manual ts injection needed)
- [x] LTE telemetry uses `LteTelemetry::from(reading)` to convert sensor-internal `LteReading` to message type
- [x] update telemetry tests to use `TelemetryData` enum variants
- [x] run tests — must pass before next task

### Task 6: Migrate CommandProcessor to typed payloads
- [x] update `CommandEnvelope` in `commands.rs` to use non-generic `CommandEnvelope` from messages module (with `CommandPayload` enum)
- [x] update `CommandHandler` trait — handlers receive `&CommandEnvelope` and pattern-match on `CommandPayload` to extract their specific payload variant
- [x] update `CommandResult` — changed `extra: serde_json::Value` to `data: CommandResultData` (typed enum)
- [x] update `CommandProcessor::publish_result` — accepts `Option<CommandResultData>` (None for error responses)
- [x] remove `sanitize_extra` — no longer needed with typed data (reserved-key stripping was a `serde_json::Value` concern)
- [x] update command processor tests
- [x] run tests — must pass before next task

### Task 7: Migrate command handlers to typed structs
- [x] update `handlers/get_config.rs` — pattern-match `CommandPayload::GetConfig`, return `CommandResultData::GetConfig(result)`
- [x] update `handlers/config_update.rs` — pattern-match `CommandPayload::ConfigUpdate`, return `CommandResultData::ConfigUpdate(result)`
- [x] update `handlers/update_request.rs` — pattern-match `CommandPayload::UpdateRequest`, return `CommandResultData::UpdateRequest(result)`
- [x] update `handlers/modem_commands.rs` — pattern-match `CommandPayload::ModemCommands`, return `CommandResultData::ModemCommands(result)`
- [x] all handlers return `CommandError` on wrong payload variant (defensive, shouldn't happen with correct routing)
- [x] update handler tests — construct `CommandEnvelope` with `CommandPayload` enum variants, assert on typed `CommandResultData` variants
- [x] run tests — must pass before next task

### Task 8: Clean up removed types and dead code
- [x] remove old `PingReading`, `LteReading`, `LteSignalQuality`, `LteNeighborCell`, `CpuTempReading` from sensor files (now in messages module) — PingReading/CpuTempReading already removed in Task 4; LteSignalQuality/LteNeighborCell re-exported from messages module; LteReading kept as sensor-internal type (HashMap-based neighbor tracking, no messages module equivalent)
- [x] remove old `CommandEnvelope` type alias and `CommandResult { extra: serde_json::Value }` — `CommandEnvelope` imported directly from messages module; `CommandResult` changed to `{ data: CommandResultData }` (typed enum, distinct from `CommandResultMsg` which is the MQTT wire message)
- [x] remove `sanitize_extra` method and `RESERVED_RESULT_KEYS` constant — no longer needed with typed `CommandResultData`
- [x] remove `build_telemetry_json()` if not already removed — already removed in Task 5
- [x] remove per-command schema files (`command/{name}/payload.json`, `command/{name}/result.json`) and per-type telemetry schemas (`telemetry/ping.json`, etc.) — replaced by unified schemas
- [x] remove unused `schema_pair` helper function from `messages/schema.rs`
- [x] verify no dead imports or unused code
- [x] run `cargo clippy` — all issues must be fixed
- [x] run tests — must pass before next task

### Task 9: Verify acceptance criteria
- [x] verify all telemetry is published via `TelemetryEnvelope` with `TelemetryData` enum and `ts` field
- [x] verify all command handlers use typed payloads via `CommandPayload` enum (no `serde_json::Value` for known fields)
- [x] verify `GetConfigResult` uses `SafeConfig` (not raw `serde_json::Value`), with sensitive fields redacted
- [x] verify JSON Schema files generated in `assets/schema/`
- [x] verify placeholder commands have correct struct names
- [x] run full test suite (unit tests)
- [x] run `cargo clippy` — all issues must be fixed

### Task 10: Update documentation
- [x] update CLAUDE.md Key Types section with new message types
- [x] update CLAUDE.md MQTT Service section to reference messages module

### Task 11: [Final] Replace MQTT integration test infrastructure with testcontainers
- [x] add `testcontainers = "0.27"` to `[dev-dependencies]` in `Cargo.toml`
- [x] rewrite `tests/mqtt_integration.rs` to be fully self-contained:
  - generate TLS certs at runtime using `rcgen` + `tempfile` (no static fixture dependency)
  - write `mosquitto.conf` to temp dir
  - start `eclipse-mosquitto:2` container via `testcontainers::GenericImage` with bind mounts
  - use dynamic port mapping via `container.get_host_port_ipv4()`
  - wait for broker readiness via `WaitFor::message_on_stderr("Opening ipv4 listen socket on port 1883")`
- [x] remove `#[ignore]` from all tests — tests now run with plain `cargo test --test mqtt_integration`
- [x] add `wait_for_suback` helper — properly drains event loop until `SubAck`, replacing unreliable `timeout(poll())` pattern for subscription readiness
- [x] add `wait_for_puback` helper — ensures publish is acknowledged by broker before asserting on subscriber, fixing flaky sequential delivery test
- [x] delete `tests/run_mqtt_tests.sh` (no longer needed)
- [x] delete `tests/docker-compose.mqtt.yml` (no longer needed)
- [x] run tests 3× for stability — all 7 tests pass consistently

## Technical Details

### Message struct naming convention
- Telemetry data: `{Sensor}Telemetry` (e.g., `PingTelemetry`, `LteTelemetry`, `CpuTempTelemetry`)
- Telemetry envelope: `TelemetryEnvelope { ts, data: TelemetryData }` — non-generic, tagged enum dispatch
- Telemetry dispatch: `TelemetryData` — `#[serde(tag = "type")]` enum wrapping all telemetry variants
- Command payload: `{Command}Payload` (e.g., `GetConfigPayload`, `ModemCommandPayload`)
- Command result: `{Command}Result` (e.g., `GetConfigResult`, `ModemCommandResult`)
- Command dispatch enums: `CommandPayload` and `CommandResultData` — `#[serde(tag = "type")]` enums wrapping all command variants
- Non-generic wrappers: `CommandEnvelope { payload: CommandPayload }`, `CommandResultMsg { data: Option<CommandResultData> }`, `CommandStatus`
- Config types: `SafeConfig` — `Config` with sensitive fields (cert paths) redacted for MQTT exposure

### Schema file layout
Unified schemas — each top-level type generates one schema file containing all variants via `oneOf`/definitions:
```
assets/schema/
├── telemetry/
│   └── envelope.json          (TelemetryEnvelope with all TelemetryData variants)
└── command/
    ├── envelope.json          (CommandEnvelope with all CommandPayload variants)
    ├── status.json            (CommandStatus)
    └── result.json            (CommandResultMsg with all CommandResultData variants)
```

### Dependencies added
- `schemars = { version = "0.8", features = ["chrono"] }` in `[dependencies]`
- `schemars = "0.8"` in `[build-dependencies]` (for build.rs directory setup; actual schema generation is in `messages::schema::generate_all_schemas()` called from tests)
- `testcontainers = "0.27"` in `[dev-dependencies]` (for MQTT integration tests)

### Design decisions (captured during implementation)

**Generics → Enums**: Originally designed with generic types (`TelemetryEnvelope<T>`, `CommandEnvelope<T>`, `CommandResultMsg<T>`) using `#[serde(flatten)]`. Migrated to non-generic types with internally-tagged enums (`#[serde(tag = "type")]`) because:
- Eliminates type-erasure issues with `serde_json::from_value` in handlers
- Single unified schema per top-level type (no per-variant schema files needed)
- `"type"` discriminator field in JSON makes messages self-describing
- Pattern matching in handlers provides compile-time exhaustiveness checking

**`CommandResultMsg.data` is `Option`**: Error responses carry `data: None` (omitted from JSON via `skip_serializing_if`). Success responses carry `Some(CommandResultData::Variant(...))`.

**`LteTelemetry` restructured**: Signal quality fields grouped into `LteSignalQuality` substruct (matching the sensor-internal type), with `From<LteReading>` conversion to handle HashMap→Vec neighbor collection.

**Integration tests → testcontainers**: Replaced the external `run_mqtt_tests.sh` + `docker-compose.mqtt.yml` workflow with `testcontainers` crate. Each test now spins up its own isolated Mosquitto container with runtime-generated TLS certs. Key improvements:
- No `#[ignore]` — tests run with plain `cargo test`, no manual docker-compose or scripts needed
- Proper event loop synchronization via `wait_for_suback`/`wait_for_puback` helpers instead of unreliable `timeout(poll())` pattern
- Each test is fully isolated (own container, own certs, own ports)

## Post-Completion

**Manual verification**:
- Inspect generated JSON Schema files for correctness
- Validate schemas against sample JSON payloads from existing MQTT broker logs if available
