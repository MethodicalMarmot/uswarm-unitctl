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

# Generate JSON schemas (required after fresh clone; automatic on subsequent builds)
cargo run --bin generate-schema
# or
make schema
```

## Configuration

- `config.toml` (from `config.toml.example`) — TOML config with sections: general, mavlink, mavlink.fc, camera, sensors, mqtt
- All fields are required — there are no serde defaults. The config file must explicitly specify every value.
- Config is loaded via `config::load_config()` and parsed with serde
- `[general]` section includes `debug` (bool), `interface` (String, required — the network interface name used for ping sensor binding and IP resolution in MQTT online status), and `env_dir` (String, required — directory for runtime state files such as `pending-restart-uuid` written by the restart command's self-restart path)
- Debug logging is enabled by either `--debug` CLI flag or `general.debug = true` in config
- `[mavlink]` section includes `local_mavlink_port` (u16, used for Rust code TCP connection), `remote_mavlink_port` (u16, written to env file), `gcs_ip` (String), and `env_path` (String) fields. `self_sysid = 0` is the autodiscovery sentinel — the effective sysid is resolved at runtime via `Context::self_sysid()` as the minimum FC sysid in `Context.available_systems` (`self_sysid` uniqueness checks in `Config::validate()` are skipped in this mode; `gcs_sysid`/`sniffer_sysid`/`bs_sysid` uniqueness still applies).
- `[camera]` section configures camera env file generation: `gcs_ip`, `env_path`, `remote_video_port`, `width`, `height`, `framerate`, `bitrate`, `flip`, `camera_type` (`rpi`, `usb`, `usb_yuy2`, `siyi`, `openipc`, or `fake` for simulation), `device`
- `[sensors]` section configures three sensors (ping, lte, system) — each can be enabled/disabled independently with optional per-sensor `interval_s` override (falls back to `default_interval_s`, which defaults to 5s). Ping sensor uses `general.interface` for binding. `[sensors.lte]` also requires `modem_type` (`"dbus"` for real ModemManager or `"fake"` for deterministic simulation) and `neighbor_expiry_s` (f64). `[sensors.system]` only takes `enabled` and optional `interval_s`.
- `[general]` also carries optional shared mutual-TLS material: `ca_cert_path`, `client_cert_path`, `client_key_path` (`Option<String>`), consumed by both `[mqtt]` and `[fluentbit]`. MQTT requires them via `MqttTransport::new(&Config)` when `mqtt.enabled`; Fluent Bit validation rejects `tls = true` unless all three are set.
- `[mqtt]` section configures MQTT communication with a central server: `enabled` (bool), `host`, `port` (8883 for TLS), `env_prefix` (topic namespace), `telemetry_interval_s`. TLS cert paths now live in `[general]` (see above); `MqttTransport::new` returns `MissingTlsConfig { field }` if a required path is unset.
- `[fluentbit]` section configures the Fluent Bit log forwarder env writer: `enabled` (bool), `host`, `port` (u16), `tls` (bool), `tls_verify` (bool), `config_path` (output path of generated YAML; the bundled `fluentbit.service` reads `/etc/fluent-bit.yaml`), and optional `systemd_filter: Option<Vec<String>>` (journald `KEY=VALUE` filters; key must match `[A-Z_][A-Z0-9_]*`). Filter values are emitted as double-quoted YAML scalars to avoid YAML metachar interpretation.

## Architecture

### Crate Structure

`lib.rs` defines a `Task` trait (`run() -> Vec<JoinHandle>`) and re-exports all modules. `main.rs` is the application entry point that wires components together. The `generate-schema` binary (`src/bin/generate_schema.rs`) generates JSON Schema files from message types into `assets/schema/`. **Net utilities** (`net.rs`) provides `resolve_ipv4(interface) -> Result<Ipv4Addr, ResolveIpError>` which resolves the first IPv4 address on a named network interface using `nix::ifaddrs::getifaddrs()`. Used at startup for fail-fast validation and by `StatusPublisher` on each MQTT connect/reconnect.

### Async Task System

The `Task` trait is implemented by all major components. `main.rs` creates a shared `Context`, spawns tasks (env writers, drone component, sniffer, sensor manager, telemetry reporter, status publisher), waits for flight controller discovery, then runs until SIGINT/SIGTERM. Before spawning tasks, `main.rs` validates that `general.interface` has a resolvable IPv4 address; on failure it logs an error and exits with code 1.

### Context (`context.rs`)

Shared state hub (Arc-wrapped). Holds config, a broadcast channel (capacity 256) for incoming message routing, an mpsc channel (capacity 500) for outgoing messages, and a RwLock-protected HashSet of discovered system IDs. References `SensorValues` from the sensors module. `Context::self_sysid() -> Option<u8>` resolves the effective MAVLink self sysid: returns the configured value when non-zero, or the minimum FC sysid from `available_systems` (autodiscovery), or `None` if no FC has been observed yet.

### Services (`services/`)

Shared services that run as background tasks and are accessed through `Context`.

- **ModemAccessService** (`services/modem_access/`) — Directory module. Queue-based modem access proxy. Owns an mpsc channel; callers submit AT command requests, a single worker task processes them sequentially (enforcing single-threaded constraint). `ModemAccessService::start(cfg, cancel)` branches on `cfg.modem_type`: `"dbus"` performs ModemManager D-Bus discovery with retry (`dbus.rs`), `"fake"` wires the deterministic `FakeModemAccess` simulator (`fake.rs`). Also defines `ModemAccess` trait, `ModemError`, `ModemType`, `NetworkRegistration`. Stored in `Context` as `Arc<dyn ModemAccess>`.

### MQTT Service (`services/mqtt/`)

Bidirectional MQTT communication with a central server. Split into transport (connection/TLS/reconnect/pub/sub) and command processing (lifecycle/routing/status). Enabled via `mqtt.enabled` config flag. Node ID is extracted from the client certificate CN field.

- **MqttTransport** (`services/mqtt/transport.rs`) — Wraps rumqttc `AsyncClient` + `EventLoop` with mutual TLS. Handles connection, reconnection, publish/subscribe, and exposes a `broadcast::Sender<MqttEvent>` channel for incoming messages. Provides topic builder methods for telemetry, command, and status topics. Generates a 6-char hex session ID and configures an MQTT Last Will and Testament (LWT) with an offline `NodeStatusEnvelope` payload (retained, QoS 1) so the broker automatically publishes offline status when the connection drops.
- **TelemetryPublisher** (`services/mqtt/telemetry.rs`) — Implements `Task` trait. Periodically reads sensor values from `Context.sensors`, wraps them in `TelemetryEnvelope` (non-generic, with `TelemetryData` enum) from the messages module, and publishes JSON to `{env_prefix}/nodes/{nodeId}/telemetry/{lte|ping|system}`. Skips sensors with no reading.
- **CommandProcessor** (`services/mqtt/commands.rs`) — Subscribes to `{prefix}/nodes/{nodeId}/cmnd/+/in`, deserializes incoming commands into typed `CommandEnvelope` (non-generic, with `CommandPayload` enum) from the messages module, routes to registered `CommandHandler` implementations. Manages command lifecycle: TTL check → accepted → in_progress → completed/failed. Publishes status and result using `CommandResultMsg` (non-generic, with `CommandResultData` enum) to corresponding topics.
- **TLS helpers** (`services/mqtt/tls.rs`) — `load_tls_config()` loads CA and client certificates for mutual TLS. `extract_node_id()` parses the client certificate and extracts the CN from the X.509 subject.
- **StatusPublisher** (`services/mqtt/status.rs`) — Implements `Task` trait. Subscribes to `MqttEvent::Connected` broadcast and publishes a retained online `NodeStatusEnvelope` (with session ID, version, and resolved IPv4 from `general.interface`) to `{env_prefix}/nodes/{nodeId}/status` on each connect/reconnect. Resolves IP per-connect so published address reflects current state; on resolver failure logs a warning and publishes with `ip: None`. Works with the LWT set in `MqttTransport` for automatic offline detection.
- **Command handlers** (`services/mqtt/handlers/`) — `GetConfigHandler` returns `GetConfigResult` with `SafeConfig`. `ConfigUpdateHandler` uses `ConfigUpdatePayload`/`ConfigUpdateResult` (placeholder). `UpdateRequestHandler` uses `UpdateRequestPayload`/`UpdateRequestResult` (placeholder). `ModemCommandsHandler` uses `ModemCommandPayload`/`ModemCommandResult` to route AT commands through `ModemAccess`. `RestartHandler` (`handlers/restart.rs`) restarts components: synchronous targets (`camera`, `mavlink`, `modem`) shell out to `systemctl restart <unit>` and verify liveness via `systemctl is-active`; the `unitctl` target writes the command UUID to `<env_dir>/pending-restart-uuid` then execs `systemctl restart unitctl` and parks the handler with `pending().await` until systemd terminates the process; the `reboot` target returns `Ok` immediately and spawns a delayed `reboot` invocation. Built on a `CommandRunner` trait (production: `TokioCommandRunner`) for testability.
- **RestartCompletionPublisher** (`services/mqtt/handlers/restart.rs`) — Implements `Task` trait. On startup reads `<env_dir>/pending-restart-uuid` (written by the previous unitctl process before its self-restart), waits for the next `MqttEvent::Connected`, publishes the deferred `Completed` status and `Restart{target=Unitctl}` result, then deletes the file. Must be constructed before `transport.run()` so it does not miss the first ConnAck. The file is retained until after the publish completes so a publish failure or early cancel does not silently drop the queued ack.
- **Messages module** (`messages/`) — Typed message structs for all MQTT messages. Contains `telemetry.rs` (telemetry data types and `TelemetryEnvelope` with `TelemetryData` enum), `commands.rs` (command payload/result types, `CommandEnvelope` with `CommandPayload` enum, `CommandResultMsg` with `CommandResultData` enum, `CommandStatus`, `SafeConfig`), `status.rs` (node status types: `NodeStatusEnvelope` with `StatusData` enum for Online/Offline presence), and `schema.rs` (JSON Schema generation via `schemars`). Schemas are generated into `assets/schema/` by the `generate-schema` binary. On subsequent builds, `build.rs` automatically runs the binary if it was previously built; for fresh clones, run `cargo run --bin generate-schema` or `make schema`.

### Sensor Subsystem (`sensors/`)

Trait-based sensor framework. Each sensor implements `Sensor` trait (`name()` + `async fn run()`), runs as its own tokio task at a configurable interval, and stores results in Context's `SensorValues`.

- **SensorManager** (`sensors/mod.rs`) — builds list of enabled sensors from config, spawns each as tokio task with CancellationToken. Also defines `SensorValues` struct.
- **PingSensor** (`sensors/ping.rs`) — spawns `ping` subprocess with `-I <interface>` (from `general.interface`), sends SIGQUIT for stats, parses latency/loss. Uses `PingTelemetry` from messages module (aliased as `PingReading`).
- **LteSensor** (`sensors/lte.rs`) — reads modem from Context (via `ModemAccessService`), AT command signal quality parsing, neighbor cell tracking. Uses `LteNeighborCell` from messages module. Defines `LteReading` (sensor-internal, HashMap-based) and `LteSignalQuality`.
- **SystemSensor** (`sensors/system.rs`) — gathers host-wide telemetry once per interval: CPU temperature (sysfs thermal zone), aggregate CPU usage and memory (via `sysinfo`), disks, load average, uptime, per-interface bandwidth (delta vs previous tick) joined with IPv4 addresses from `nix::ifaddrs`, and connected cameras enumerated/probed via the `v4l` crate. Stores the result in `Context.sensors.system`. Uses `SystemTelemetry` from the messages module.

### Env File Writers (`env/`)

Write-on-start module that generates environment files for external services (mavlink-routerd, camera streamer) at startup. Each writer implements the `Task` trait, spawns a single tokio task that writes the file (creating parent directories if needed) and exits.

- **MavlinkEnvWriter** (`env/mavlink_env.rs`) — writes mavlink.env with GCS_IP, REMOTE_MAVLINK_PORT, SNIFFER_SYS_ID, LOCAL_MAVLINK_PORT, FC_TTY, FC_BAUDRATE. Path configured via `mavlink.env_path`.
- **CameraEnvWriter** (`env/camera_env.rs`) — writes camera.env with GCS_IP, REMOTE_VIDEO_PORT, CAMERA_WIDTH, CAMERA_HEIGHT, CAMERA_FRAMERATE, CAMERA_BITRATE, CAMERA_FLIP, CAMERA_TYPE, CAMERA_DEVICE. Path configured via `camera.env_path`.
- **FluentbitEnvWriter** (`env/fluentbit_env.rs`) — when `fluentbit.enabled`, generates a Fluent Bit YAML config (systemd input → forward output, with optional mutual-TLS) at `fluentbit.config_path` using an atomic tmp+rename. Skips entirely when disabled. `generate_fluentbit_config()` returns `FluentbitGenError::MissingCert` if TLS is on but a `general.*_cert_path` is unset (also caught by `Config::validate()` as a fail-fast). The bundled `services/fluentbit-watcher.path` watches the config file and triggers `services/fluentbit-watcher.service` to `systemctl restart fluentbit` on change.
- Env file format: plain text, one KEY=VALUE per line, no quotes, no trailing newline.

### MAVLink Components (`mavlink/`)

- **drone_component.rs** — DroneComponent: TCP client (tcpout) that drains the outgoing message queue at a configurable interval and sends MAVLink v2 messages. Sends heartbeats (MAV_TYPE_ONBOARD_CONTROLLER) every 1s. The heartbeat task waits for FC discovery, then resolves the sender sysid via `Context::self_sysid()` (supporting `mavlink.self_sysid = 0` autodiscovery) before invoking `heartbeat_loop`.
- **sniffer_component.rs** — MavlinkSniffer: TCP client (tcpout) that receives MAVLink messages, broadcasts them on the broadcast channel, and discovers flight controller system IDs from heartbeats (ID < 200). Filters out internal component IDs (self, sniffer, base station) to prevent self-discovery.
- **commands.rs** — 23 custom MAV_CMD_USER_1 subcommands (IDs 31011-31049) for link switching, telemetry reporting, camera control, and GPS management.
- **telemetry_reporter.rs** — TelemetryReporter: reads sensor values from Context at 1Hz and broadcasts COMMAND_LONG (MAV_CMD_USER_1) messages to GCS and base station. Reports LTE radio (subcmd 31014), LTE IP/ping (subcmd 31015), and neighbor cells (subcmds 31040-31049). Resolves the sender sysid each tick via `Context::self_sysid()` and skips ticks until FC discovery completes (autodiscovery support).
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
- `FluentbitEnvWriter` — writes Fluent Bit YAML config at startup when `fluentbit.enabled` (defined in `env/fluentbit_env.rs`)
- `FluentbitConfig` — fluentbit env file settings: enabled, host, port, tls, tls_verify, config_path, optional systemd_filter (defined in `config.rs`)
- `FluentbitGenError::MissingCert` — emitted when `fluentbit.tls` is set but a `general.*_cert_path` is unset (defined in `env/fluentbit_env.rs`)
- `ModemAccessService` — queue-based modem access proxy, serializes AT commands through a worker task (defined in `services/modem_access/mod.rs`)
- `ModemAccess` trait — async modem interface (model, command, imsi, registration_status) defined in `services/modem_access/mod.rs`
- `FakeModemAccess` — deterministic ModemAccess implementation for simulation; monotonic counter drives drifting LTE signal values (defined in `services/modem_access/fake.rs`)
- `NetworkRegistration` — network registration status enum (defined in `services/modem_access/mod.rs`)
- `Context` — shared state with channels, system discovery, sensor values, and modem access. `self_sysid()` async method resolves the effective MAVLink self sysid (configured value if non-zero, else min of `available_systems`).
- `SensorValues` — RwLock-wrapped optional readings for ping, LTE, and system telemetry (defined in `sensors/mod.rs`)
- `PingTelemetry` — reachable, latency_ms, loss_percent (defined in `messages/telemetry.rs`; aliased as `PingReading` in `sensors/ping.rs`)
- `LteTelemetry` — signal quality fields and neighbor cells (defined in `messages/telemetry.rs`)
- `LteNeighborCell` — neighbor cell data: pcid, rsrp, rsrq, rssi, rssnr, earfcn, last_seen (defined in `messages/telemetry.rs`)
- `SystemTelemetry` — host-wide snapshot: `cpu_temperature_c: Option<f64>`, `cpu_usage_percent: f32`, `ram: RamUsage`, `disks: Vec<DiskUsage>`, `load_avg: LoadAverage`, `uptime_s: u64`, `network_interfaces: Vec<NetworkInterfaceTelemetry>`, `cameras: Vec<CameraInfo>` (defined in `messages/telemetry.rs`)
- `SystemSensorConfig` — `enabled` + optional `interval_s` (defined in `config.rs`)
- `TelemetryEnvelope` — non-generic envelope: `ts` timestamp + `data: TelemetryData` enum (defined in `messages/telemetry.rs`)
- `TelemetryData` — `#[serde(tag = "type")]` enum: Ping, Lte, System variants (defined in `messages/telemetry.rs`)
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
- `CommandPayload` — `#[serde(tag = "type")]` enum: GetConfig, ConfigUpdate, ModemCommands, UpdateRequest, Restart variants (defined in `messages/commands.rs`)
- `CommandState` — enum: Accepted, InProgress, Completed, Failed, Rejected, Expired, Superseded (defined in `messages/commands.rs`)
- `CommandResultMsg` — non-generic command result: uuid, ok, ts, error, `data: Option<CommandResultData>` (defined in `messages/commands.rs`)
- `CommandResultData` — `#[serde(tag = "type")]` enum: GetConfig, ConfigUpdate, ModemCommands, UpdateRequest, Restart variants (defined in `messages/commands.rs`)
- `RestartPayload`/`RestartResult` — restart command payload and result with `target: RestartTarget` (defined in `messages/commands.rs`)
- `RestartTarget` — enum: Camera, Mavlink, Modem, Unitctl, Reboot (defined in `messages/commands.rs`)
- `RestartHandler` — restart command handler; uses `CommandRunner` trait to invoke `systemctl`/`reboot` (defined in `services/mqtt/handlers/restart.rs`)
- `RestartCompletionPublisher` — Task that publishes deferred `Completed` for self-restart on next-boot MQTT connect (defined in `services/mqtt/handlers/restart.rs`)
- `CommandRunner` trait — async wrapper around `tokio::process::Command::output` for testability (defined in `services/mqtt/handlers/restart.rs`)
- `CommandStatus` — command status update: uuid, state, ts (defined in `messages/commands.rs`)
- `SafeConfig` — Config with sensitive fields (cert paths, keys) redacted for MQTT exposure (defined in `messages/commands.rs`)
- `GetConfigPayload`/`GetConfigResult` — get_config command types (defined in `messages/commands.rs`)
- `ConfigUpdatePayload`/`ConfigUpdateResult` — config_update command types (defined in `messages/commands.rs`)
- `UpdateRequestPayload`/`UpdateRequestResult` — update_request command types (defined in `messages/commands.rs`)
- `ModemCommandPayload`/`ModemCommandResult` — modem_commands command types (defined in `messages/commands.rs`)
- `NodeStatusEnvelope` — non-generic envelope: `ts` timestamp + `data: StatusData` enum (defined in `messages/status.rs`)
- `StatusData` — `#[serde(tag = "type")]` enum: Online, Offline variants for node presence (defined in `messages/status.rs`)
- `OnlineStatusData` — session ID, version, and optional `ip` (IPv4 of `general.interface`, resolved per-connect), published on connect (defined in `messages/status.rs`)
- `OfflineStatusData` — last_session and last_online timestamp, used in LWT (defined in `messages/status.rs`)
- `StatusPublisher` — publishes retained online status (including resolved IPv4 from `general.interface`) on each MQTT connect/reconnect, works with LWT for offline detection (defined in `services/mqtt/status.rs`)
- `ResolveIpError` — enum: InterfaceNotFound, NoIpv4, Getifaddrs; returned by `resolve_ipv4()` (defined in `net.rs`)

## Dependencies

tokio, mavlink (ardupilotmega + tcp), tokio-util, serde, toml, tracing, tracing-subscriber, clap, async-trait, regex, modemmanager, zbus, nix (signal, process, net), rumqttc (use-rustls), serde_json, chrono (serde), x509-parser, schemars (chrono feature, also in build-dependencies), rand, thiserror

## Conventions

- Backend lint gate is `cargo clippy -- -D warnings` — warnings fail CI.
- Design specs and implementation plans live in `docs/plans/` (completed plans move to `docs/plans/completed/`). Do not use `docs/superpowers/`.
