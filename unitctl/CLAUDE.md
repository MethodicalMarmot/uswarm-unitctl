# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

unitctl is a Rust async MAVLink onboard controller that manages drone link communication. It replaces the MAVLink subsystem of `connection_balancer` (Python) with a Tokio-based implementation. Connects to mavlink-routerd via TCP, exchanges heartbeats, discovers flight controllers, and routes MAVLink messages between components.

## Building and Testing

```bash
# Build
cargo build --release

# Run tests
cargo test

# Lint
cargo clippy
cargo fmt --check

# Run
cargo run -- --config config.toml --debug
```

## Configuration

- `config.toml` (from `config.toml.example`) — TOML config with sections: general, mavlink, mavlink.fc, camera, sensors, mqtt
- All fields are required — there are no serde defaults. The config file must explicitly specify every value.
- Config is loaded via `config::load_config()` and parsed with serde
- Debug logging is enabled by either `--debug` CLI flag or `general.debug = true` in config
- `[mavlink]` section includes `local_mavlink_port` (u16, used for Rust code TCP connection), `remote_mavlink_port` (u16, written to env file), `gcs_ip` (String), and `env_path` (String) fields
- `[camera]` section configures camera env file generation: `gcs_ip`, `env_path`, `remote_video_port`, `width`, `height`, `framerate`, `bitrate`, `flip`, `camera_type`, `device`
- `[sensors]` section configures three sensors (ping, lte, cpu_temp) — each can be enabled/disabled independently with optional per-sensor `interval_s` override (falls back to `default_interval_s`)
- `[mqtt]` section configures MQTT communication with a central server: `enabled` (bool), `host`, `port` (8883 for TLS), `ca_cert_path`, `client_cert_path`, `client_key_path` (mutual TLS), `env_prefix` (topic namespace), `telemetry_interval_s`

## Architecture

### Async Task System

`main.rs` defines a `Task` trait (`run() -> Vec<JoinHandle>`) implemented by all major components. It creates a shared `Context`, spawns tasks (env writers, drone component, sniffer, sensor manager, telemetry reporter), waits for flight controller discovery, then runs until SIGINT/SIGTERM.

### Context (`context.rs`)

Shared state hub (Arc-wrapped). Holds config, a broadcast channel (capacity 256) for incoming message routing, an mpsc channel (capacity 500) for outgoing messages, and a RwLock-protected HashSet of discovered system IDs. References `SensorValues` from the sensors module.

### Services (`services/`)

Shared services that run as background tasks and are accessed through `Context`.

- **ModemAccessService** (`services/modem_access.rs`) — Queue-based modem access proxy. Owns an mpsc channel; callers submit AT command requests, a single worker task processes them sequentially against D-Bus (enforcing single-threaded D-Bus constraint). Implements `ModemAccess` trait. Handles modem discovery with auto-retry at startup, stored in `Context` as `Arc<dyn ModemAccess>`. Also defines `ModemAccess` trait, `ModemError`, `ModemType`, `NetworkRegistration`, and D-Bus modem integration (`DbusModemAccess`).

### MQTT Service (`services/mqtt/`)

Bidirectional MQTT communication with a central server. Split into transport (connection/TLS/reconnect/pub/sub) and command processing (lifecycle/routing/status). Enabled via `mqtt.enabled` config flag. Node ID is extracted from the client certificate CN field.

- **MqttTransport** (`services/mqtt/transport.rs`) — Wraps rumqttc `AsyncClient` + `EventLoop` with mutual TLS. Handles connection, reconnection, publish/subscribe, and exposes a `broadcast::Sender<MqttEvent>` channel for incoming messages. Provides topic builder methods for telemetry and command topics.
- **TelemetryPublisher** (`services/mqtt/telemetry.rs`) — Implements `Task` trait. Periodically reads sensor values from `Context.sensors`, wraps them in `TelemetryEnvelope` (non-generic, with `TelemetryData` enum) from the messages module, and publishes JSON to `{env_prefix}/nodes/{nodeId}/telemetry/{lte|ping|cpu_temp}`. Skips sensors with no reading.
- **CommandProcessor** (`services/mqtt/commands.rs`) — Subscribes to `{prefix}/nodes/{nodeId}/cmnd/+/in`, deserializes incoming commands into typed `CommandEnvelope` (non-generic, with `CommandPayload` enum) from the messages module, routes to registered `CommandHandler` implementations. Manages command lifecycle: TTL check → accepted → in_progress → completed/failed. Publishes status and result using `CommandResultMsg` (non-generic, with `CommandResultData` enum) to corresponding topics.
- **TLS helpers** (`services/mqtt/tls.rs`) — `load_tls_config()` loads CA and client certificates for mutual TLS. `extract_node_id()` parses the client certificate and extracts the CN from the X.509 subject.
- **Command handlers** (`services/mqtt/handlers/`) — `GetConfigHandler` returns `GetConfigResult` with `SafeConfig`. `ConfigUpdateHandler` uses `ConfigUpdatePayload`/`ConfigUpdateResult` (placeholder). `UpdateRequestHandler` uses `UpdateRequestPayload`/`UpdateRequestResult` (placeholder). `ModemCommandsHandler` uses `ModemCommandPayload`/`ModemCommandResult` to route AT commands through `ModemAccess`.
- **Messages module** (`messages/`) — Typed message structs for all MQTT messages. Contains `telemetry.rs` (telemetry data types and `TelemetryEnvelope` with `TelemetryData` enum), `commands.rs` (command payload/result types, `CommandEnvelope` with `CommandPayload` enum, `CommandResultMsg` with `CommandResultData` enum, `CommandStatus`, `SafeConfig`), and `schema.rs` (JSON Schema generation via `schemars`). Schemas are pre-generated into `assets/schema/` by running `cargo test` (not at build time; `build.rs` only creates directory structure).

### Sensor Subsystem (`sensors/`)

Trait-based sensor framework. Each sensor implements `Sensor` trait (`name()` + `async fn run()`), runs as its own tokio task at a configurable interval, and stores results in Context's `SensorValues`.

- **SensorManager** (`sensors/mod.rs`) — builds list of enabled sensors from config, spawns each as tokio task with CancellationToken. Also defines `SensorValues` struct.
- **PingSensor** (`sensors/ping.rs`) — spawns `ping` subprocess, sends SIGQUIT for stats, parses latency/loss. Uses `PingTelemetry` from messages module (aliased as `PingReading`).
- **LteSensor** (`sensors/lte.rs`) — reads modem from Context (via `ModemAccessService`), AT command signal quality parsing, neighbor cell tracking. Uses `LteNeighborCell` from messages module. Defines `LteReading` (sensor-internal, HashMap-based) and `LteSignalQuality`.
- **CpuTempSensor** (`sensors/cpu_temp.rs`) — reads sysfs thermal zone, converts millidegrees to degrees. Uses `CpuTempTelemetry` from messages module (aliased as `CpuTempReading`).

### Env File Writers (`env/`)

Write-on-start module that generates environment files for external services (mavlink-routerd, camera streamer) at startup. Each writer implements the `Task` trait, spawns a single tokio task that writes the file (creating parent directories if needed) and exits.

- **MavlinkEnvWriter** (`env/mavlink_env.rs`) — writes mavlink.env with GCS_IP, REMOTE_MAVLINK_PORT, SNIFFER_SYS_ID, LOCAL_MAVLINK_PORT, FC_TTY, FC_BAUDRATE. Path configured via `mavlink.env_path`.
- **CameraEnvWriter** (`env/camera_env.rs`) — writes camera.env with GCS_IP, REMOTE_VIDEO_PORT, CAMERA_WIDTH, CAMERA_HEIGHT, CAMERA_FRAMERATE, CAMERA_BITRATE, CAMERA_FLIP, CAMERA_TYPE, CAMERA_DEVICE. Path configured via `camera.env_path`.
- Env file format: plain text, one KEY=VALUE per line, no quotes, no trailing newline.

### MAVLink Components (`mavlink/`)

- **drone_component.rs** — DroneComponent: TCP client (tcpout) that drains the outgoing message queue at a configurable interval and sends MAVLink v2 messages. Sends heartbeats (MAV_TYPE_ONBOARD_CONTROLLER) every 1s.
- **sniffer_component.rs** — MavlinkSniffer: TCP client (tcpout) that receives MAVLink messages, broadcasts them on the broadcast channel, and discovers flight controller system IDs from heartbeats (ID < 200). Filters out internal component IDs (self, sniffer, base station) to prevent self-discovery.
- **commands.rs** — 23 custom MAV_CMD_USER_1 subcommands (IDs 31011-31049) for link switching, telemetry reporting, camera control, and GPS management.
- **telemetry_reporter.rs** — TelemetryReporter: reads sensor values from Context at 1Hz and broadcasts COMMAND_LONG (MAV_CMD_USER_1) messages to GCS and base station. Reports LTE radio (subcmd 31014), LTE IP/ping (subcmd 31015), and neighbor cells (subcmds 31040-31049).
- **mod.rs** — Defines `MavFrame` type alias, `build_heartbeat()`, `heartbeat_loop()`, `wait_for_fc()`, `is_fc_sysid()`, and shared connection/backoff helpers.
- **Not yet implemented:** `switcher.rs` (MavlinkConnectionSwitcher for link failover) is planned for a future phase.

### Message Flow

```
mavlink-routerd (TCP:5760)
    |-- tcpout --> DroneComponent sends outgoing messages (from mpsc queue)
    |-- tcpout --> Sniffer receives messages (broadcasts on channel)
```

### Reconnection

Both drone and sniffer components reconnect with 1s backoff on TCP connection failure. Graceful shutdown uses tokio-util CancellationToken propagated from the main signal handler.

## Key Types

- `MavFrame = (MavHeader, MavMessage)` — header + message tuple, defined in `mavlink/mod.rs`
- `MavCmdUser1SubCmd` — enum for custom command IDs (31011-31049)
- `Config` — top-level config struct with `general`, `mavlink`, `camera`, and `sensors` sections (all fields required)
- `CameraConfig` — camera env file settings: gcs_ip, env_path, video port, resolution, framerate, bitrate, flip, camera_type, device
- `MavlinkEnvWriter` — writes mavlink env file at startup from mavlink config (defined in `env/mavlink_env.rs`)
- `CameraEnvWriter` — writes camera env file at startup from camera config (defined in `env/camera_env.rs`)
- `ModemAccessService` — queue-based modem access proxy, serializes AT commands through a worker task (defined in `services/modem_access.rs`)
- `ModemAccess` trait — async modem interface (model, command, imsi, registration_status) defined in `services/modem_access.rs`
- `NetworkRegistration` — network registration status enum (defined in `services/modem_access.rs`)
- `Context` — shared state with channels, system discovery, sensor values, and modem access
- `SensorValues` — RwLock-wrapped optional readings for ping, LTE, and CPU temperature (defined in `sensors/mod.rs`)
- `PingTelemetry` — reachable, latency_ms, loss_percent (defined in `messages/telemetry.rs`; aliased as `PingReading` in `sensors/ping.rs`)
- `LteTelemetry` — signal quality fields and neighbor cells (defined in `messages/telemetry.rs`)
- `LteNeighborCell` — neighbor cell data: pcid, rsrp, rsrq, rssi, rssnr, earfcn, last_seen (defined in `messages/telemetry.rs`)
- `CpuTempTelemetry` — temperature_c (defined in `messages/telemetry.rs`; aliased as `CpuTempReading` in `sensors/cpu_temp.rs`)
- `TelemetryEnvelope` — non-generic envelope: `ts` timestamp + `data: TelemetryData` enum (defined in `messages/telemetry.rs`)
- `TelemetryData` — `#[serde(tag = "type")]` enum: Ping, Lte, CpuTemp variants (defined in `messages/telemetry.rs`)
- `Task` trait — component interface (`run() -> Vec<JoinHandle>`) defined in `main.rs`
- `Sensor` trait — async sensor interface (name + run)
- `SensorManager` — spawns enabled sensors as tokio tasks
- `TelemetryReporter` — 1Hz MAVLink broadcast of sensor values
- `MqttConfig` — MQTT config: enabled, host, port, cert paths, env_prefix, telemetry_interval_s (defined in `config.rs`)
- `MqttTransport` — MQTT connection with TLS, reconnect, pub/sub, event broadcast (defined in `services/mqtt/transport.rs`)
- `MqttEvent` — enum: Connected, Disconnected, Message (defined in `services/mqtt/transport.rs`)
- `TelemetryPublisher` — periodic sensor JSON publisher over MQTT (defined in `services/mqtt/telemetry.rs`)
- `CommandProcessor` — command lifecycle manager with handler dispatch (defined in `services/mqtt/commands.rs`)
- `CommandHandler` trait — async command handler interface (defined in `services/mqtt/commands.rs`)
- `CommandEnvelope` — non-generic incoming command: uuid, issued_at, ttl_sec, `payload: CommandPayload` (defined in `messages/commands.rs`)
- `CommandPayload` — `#[serde(tag = "type")]` enum: GetConfig, ConfigUpdate, ModemCommands, UpdateRequest variants (defined in `messages/commands.rs`)
- `CommandState` — enum: Accepted, InProgress, Completed, Failed, Rejected, Expired, Superseded (defined in `messages/commands.rs`)
- `CommandResultMsg` — non-generic command result: uuid, ok, ts, error, `data: Option<CommandResultData>` (defined in `messages/commands.rs`)
- `CommandResultData` — `#[serde(tag = "type")]` enum: GetConfig, ConfigUpdate, ModemCommands, UpdateRequest variants (defined in `messages/commands.rs`)
- `CommandStatus` — command status update: uuid, state, ts (defined in `messages/commands.rs`)
- `SafeConfig` — Config with sensitive fields (cert paths, keys) redacted for MQTT exposure (defined in `messages/commands.rs`)
- `GetConfigPayload`/`GetConfigResult` — get_config command types (defined in `messages/commands.rs`)
- `ConfigUpdatePayload`/`ConfigUpdateResult` — config_update command types (defined in `messages/commands.rs`)
- `UpdateRequestPayload`/`UpdateRequestResult` — update_request command types (defined in `messages/commands.rs`)
- `ModemCommandPayload`/`ModemCommandResult` — modem_commands command types (defined in `messages/commands.rs`)

## Dependencies

tokio, mavlink (ardupilotmega + tcp), tokio-util, serde, toml, tracing, tracing-subscriber, clap, async-trait, regex, modemmanager, zbus, nix, rumqttc (use-rustls), serde_json, chrono (serde), x509-parser, schemars (chrono feature, also in build-dependencies)
