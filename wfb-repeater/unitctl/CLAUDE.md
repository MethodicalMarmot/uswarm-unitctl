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

- `config.toml` (from `config.toml.example`) — TOML config with sections: general, mavlink, mavlink.fc
- The `[mavlink]` section is required; all fields have serde defaults
- Config is loaded via `config::load_config()` and parsed with serde
- Debug logging is enabled by either `--debug` CLI flag or `general.debug = true` in config

## Architecture

### Async Task System

`main.rs` creates a shared `Context`, spawns four Tokio tasks (drone component, drone heartbeat, sniffer, sniffer heartbeat), waits for flight controller discovery, then runs until SIGINT/SIGTERM.

### Context (`context.rs`)

Shared state hub (Arc-wrapped). Holds config, a broadcast channel (capacity 256) for incoming message routing, an mpsc channel (capacity 500) for outgoing messages, and a RwLock-protected HashSet of discovered system IDs.

### MAVLink Components (`mavlink/`)

- **drone.rs** — MavlinkDroneComponent: TCP client (tcpout) that drains the outgoing message queue at a configurable interval and sends MAVLink v2 messages. Sends heartbeats (MAV_TYPE_ONBOARD_CONTROLLER) every 1s.
- **sniffer.rs** — MavlinkSniffer: TCP client (tcpout) that receives MAVLink messages, broadcasts them on the broadcast channel, and discovers flight controller system IDs from heartbeats (ID < 200). Filters out internal component IDs (self, sniffer, base station) to prevent self-discovery.
- **commands.rs** — 23 custom MAV_CMD_USER_1 subcommands (IDs 31011-31049) for link switching, telemetry reporting, camera control, and GPS management.
- **Not yet implemented:** `switcher.rs` (MavlinkConnectionSwitcher for link failover) and `telemetry.rs` (MavlinkTelemetryReporter for 1Hz broadcasts) are planned for a future phase.

### Message Flow

```
mavlink-routerd (TCP:5760)
    |-- tcpout --> DroneComponent sends outgoing messages (from mpsc queue)
    |-- tcpout --> Sniffer receives messages (broadcasts on channel)
```

### Reconnection

Both drone and sniffer components reconnect with 1s backoff on TCP connection failure. Graceful shutdown uses tokio-util CancellationToken propagated from the main signal handler.

## Key Types

- `MavFrame = (MavHeader, MavMessage)` — header + message tuple used in channels
- `MavCmdUser1SubCmd` — enum for custom command IDs (31011-31049)
- `Config` — top-level config struct with `general` and `mavlink` sections
- `Context` — shared state with channels and system discovery

## Dependencies

tokio, mavlink (ardupilotmega + tcp), tokio-util, serde, toml, tracing, tracing-subscriber, clap
