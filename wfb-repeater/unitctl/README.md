# unitctl

MAVLink onboard controller for drone link management, written in Rust. Replaces the MAVLink subsystem of `connection_balancer` (Python) with an async Tokio-based implementation.

## Building

```bash
cargo build --release
```

The binary is produced at `target/release/unitctl`.

### Dependencies

- Rust 2021 edition
- tokio (async runtime)
- mavlink (ArduPilot MAVLink v2 protocol, TCP transport)
- clap (CLI argument parsing)
- serde + toml (configuration)
- tracing (structured logging)
- tokio-util (cancellation tokens)

## Prerequisites

- `mavlink-routerd` running and accessible at the configured host:port (default: 127.0.0.1:5760)
- A flight controller connected to mavlink-routerd (unitctl blocks on startup until an FC heartbeat is detected)

## Usage

```bash
# Run with default config (config.toml in current directory)
unitctl

# Run with a specific config file
unitctl --config /etc/unitctl/config.toml

# Run with debug logging
unitctl --debug

# Combine options
unitctl --config /path/to/config.toml --debug
```

### CLI Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--config` | `-c` | `config.toml` | Path to TOML configuration file |
| `--debug` | `-d` | off | Enable debug-level logging |

## Configuration

Configuration uses TOML format. Copy `config.toml.example` as a starting point:

```bash
cp config.toml.example config.toml
```

### Config Format

```toml
[general]
debug = false                # Enable debug logging via config (default: false; also enabled by --debug CLI flag)

[mavlink]
protocol = "tcpout"          # Connection protocol (default: "tcpout")
host = "127.0.0.1"           # mavlink-routerd host (default: "127.0.0.1")
port = 5760                  # mavlink-routerd port (default: 5760)
self_sysid = 1               # MAVLink system ID for this unit (default: 1)
self_compid = 10             # MAVLink component ID for this unit (default: 10)
gcs_sysid = 255              # Ground control station system ID (default: 255)
gcs_compid = 190             # Ground control station component ID (default: 190)
sniffer_sysid = 199          # Sniffer system ID for passive listening (default: 199)
bs_sysid = 200               # Base station system ID (default: 200)
iteration_period_ms = 10     # Message drain interval in ms (default: 10)

[mavlink.fc]
tty = "/dev/ttyFC"           # Flight controller serial device (default: "/dev/ttyFC")
baudrate = 57600             # Serial baud rate (default: 57600)
```

The `[mavlink]` section is required. All fields within it have defaults, so a minimal config is:

```toml
[mavlink]
```

## Architecture

unitctl runs four async tasks managed by Tokio:

```
mavlink-routerd (TCP:5760)
    |
    +-- MavlinkDroneComponent (tcpout) -- sends outgoing messages + heartbeats
    |       ^
    |       |-- mpsc channel (capacity 500) -- outgoing message queue
    |
    +-- MavlinkSniffer (tcpout) -- receives messages, discovers flight controllers
            |
            +-- broadcast channel (capacity 256) -- routes messages to subscribers
            +-- Context.available_systems -- tracks discovered system IDs
```

### Components

- **Drone Component** (`mavlink/drone.rs`) - Connects as MAVLink client (tcpout). Drains the outgoing message queue at the configured interval and sends messages over TCP. Sends heartbeats every 1s (MAV_TYPE_ONBOARD_CONTROLLER).

- **Sniffer** (`mavlink/sniffer.rs`) - Connects as MAVLink TCP client (tcpout). Receives all messages and broadcasts them on a channel. Discovers flight controller system IDs from heartbeats (system ID < 200), filtering out internal component IDs.

- **Context** (`context.rs`) - Shared state holding config, broadcast/mpsc channels, and discovered system IDs. Thread-safe via Arc and RwLock.

- **Commands** (`mavlink/commands.rs`) - Defines 23 custom MAV_CMD_USER_1 subcommands (IDs 31011-31049) for link switching, telemetry, camera, and GPS control.

### Startup Sequence

1. Parse CLI arguments and load TOML config
2. Create shared Context with channels
3. Spawn drone component and sniffer tasks
4. Wait for flight controller discovery (heartbeat with system ID < 200)
5. Run until SIGINT/SIGTERM triggers graceful shutdown

## Testing

```bash
cargo test
```

Tests cover config parsing, custom command encoding/decoding, channel behavior, heartbeat construction, message queue drain, system discovery, and integration with a mock TCP server.

## Linting

```bash
cargo clippy
cargo fmt --check
```
