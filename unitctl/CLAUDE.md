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

- `config.toml` (from `config.toml.example`) — TOML config with sections: general, mavlink, mavlink.fc, sensors
- All fields are required — there are no serde defaults. The config file must explicitly specify every value.
- Config is loaded via `config::load_config()` and parsed with serde
- Debug logging is enabled by either `--debug` CLI flag or `general.debug = true` in config
- `[sensors]` section configures three sensors (ping, lte, cpu_temp) — each can be enabled/disabled independently with optional per-sensor `interval_s` override (falls back to `default_interval_s`)

## Architecture

### Async Task System

`main.rs` defines a `Task` trait (`run() -> Vec<JoinHandle>`) implemented by all major components. It creates a shared `Context`, spawns tasks (drone component, sniffer, sensor manager, telemetry reporter), waits for flight controller discovery, then runs until SIGINT/SIGTERM.

### Context (`context.rs`)

Shared state hub (Arc-wrapped). Holds config, a broadcast channel (capacity 256) for incoming message routing, an mpsc channel (capacity 500) for outgoing messages, and a RwLock-protected HashSet of discovered system IDs. References `SensorValues` from the sensors module.

### Sensor Subsystem (`sensors/`)

Trait-based sensor framework. Each sensor implements `Sensor` trait (`name()` + `async fn run()`), runs as its own tokio task at a configurable interval, and stores results in Context's `SensorValues`.

- **SensorManager** (`sensors/mod.rs`) — builds list of enabled sensors from config, spawns each as tokio task with CancellationToken. Also defines `SensorValues` struct.
- **PingSensor** (`sensors/ping.rs`) — spawns `ping` subprocess, sends SIGQUIT for stats, parses latency/loss. Defines `PingReading`.
- **LteSensor** (`sensors/lte.rs`) — ModemManager D-Bus integration, modem detection (SIMCOM 7600, Quectel EM12/EM06E/EM06GL), AT command signal quality parsing, neighbor cell tracking. Defines `LteReading`, `LteSignalQuality`, `LteNeighborCell`.
- **CpuTempSensor** (`sensors/cpu_temp.rs`) — reads sysfs thermal zone, converts millidegrees to degrees. Defines `CpuTempReading`.

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
- `Config` — top-level config struct with `general`, `mavlink`, and `sensors` sections (all fields required)
- `Context` — shared state with channels, system discovery, and sensor values
- `SensorValues` — RwLock-wrapped optional readings for ping, LTE, and CPU temperature (defined in `sensors/mod.rs`)
- `PingReading` — reachable, latency_ms, loss_percent (defined in `sensors/ping.rs`)
- `LteReading` — signal quality, neighbor cells (defined in `sensors/lte.rs`)
- `CpuTempReading` — temperature_c (defined in `sensors/cpu_temp.rs`)
- `Task` trait — component interface (`run() -> Vec<JoinHandle>`) defined in `main.rs`
- `Sensor` trait — async sensor interface (name + run)
- `SensorManager` — spawns enabled sensors as tokio tasks
- `TelemetryReporter` — 1Hz MAVLink broadcast of sensor values

## Dependencies

tokio, mavlink (ardupilotmega + tcp), tokio-util, serde, toml, tracing, tracing-subscriber, clap, async-trait, regex, modemmanager, zbus, nix
