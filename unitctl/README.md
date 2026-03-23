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
- async-trait (async trait support for sensor interface)
- regex (ping output parsing)
- modemmanager (ModemManager D-Bus client for LTE modem communication)
- zbus (D-Bus transport)
- nix (POSIX signals for ping subprocess control)

## Prerequisites

- `mavlink-routerd` running and accessible at the configured host:port (default: 127.0.0.1:5760)
- A flight controller connected to mavlink-routerd (unitctl blocks on startup until an FC heartbeat is detected)
- `ping` command available in PATH (used by the ping sensor for connectivity monitoring)
- ModemManager D-Bus service running (required for LTE sensor)

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

Configuration uses TOML format. All fields are required — copy `config.toml.example` as a starting point:

```bash
cp config.toml.example config.toml
```

### Config Format

```toml
[general]
debug = false                # Enable debug logging via config (also enabled by --debug CLI flag)

[mavlink]
protocol = "tcpout"          # Connection protocol (only "tcpout" supported)
host = "127.0.0.1"           # mavlink-routerd host
local_mavlink_port = 5760    # Local port for Rust code to connect to mavlink-routerd
remote_mavlink_port = 14550  # Remote mavlink port written to env file
self_sysid = 1               # MAVLink system ID for this unit
self_compid = 10             # MAVLink component ID for this unit
gcs_sysid = 255              # Ground control station system ID
gcs_compid = 190             # Ground control station component ID
sniffer_sysid = 199          # Sniffer system ID for passive listening
bs_sysid = 200               # Base station system ID
iteration_period_ms = 10     # Message drain interval in ms
gcs_ip = "10.101.0.1"       # GCS IP address written to mavlink env file
env_path = "/etc/mavlink.env" # Path where mavlink env file is written at startup

[mavlink.fc]
tty = "/dev/ttyFC"           # Flight controller serial device
baudrate = 57600             # Serial baud rate

[sensors]
default_interval_s = 1.0     # Default polling interval for all sensors

[sensors.ping]
enabled = true               # Enable ping sensor
# interval_s = 1.0           # Override default interval (optional)
host = "10.45.0.2"           # Target host to ping
interface = ""               # Bind to specific interface ("" = any)

[sensors.lte]
enabled = true               # Enable LTE telemetry sensor
# interval_s = 1.0           # Override default interval (optional)
neighbor_expiry_s = 30.0     # Remove neighbors not seen for this many seconds

[sensors.cpu_temp]
enabled = true               # Enable CPU temperature sensor
# interval_s = 5.0           # Override default interval (optional)

[camera]
gcs_ip = "10.101.0.1"       # GCS IP address for camera env file
env_path = "/etc/camera.env" # Path where camera env file is written at startup
remote_video_port = 5600     # Remote video port
width = 640                  # Camera resolution width
height = 360                 # Camera resolution height
framerate = 60               # Camera framerate
bitrate = 1664000            # Camera bitrate
flip = 0                     # Camera flip: 0=none, 1=horizontal, 2=vertical, 3=both
camera_type = "rpi"          # Camera type identifier
device = "/dev/video1"       # Camera device path
```

All sections and fields must be present in the config file. The only optional field is `interval_s` on each sensor, which overrides `default_interval_s` when set.

## Architecture

unitctl runs async tasks managed by Tokio:

```
mavlink-routerd (TCP:5760)
    |
    +-- DroneComponent (tcpout) -- sends outgoing messages + heartbeats
    |       ^
    |       |-- mpsc channel (capacity 500) -- outgoing message queue
    |       |
    |       +-- TelemetryReporter (1Hz) -- drains sensor values to MAVLink
    |
    +-- MavlinkSniffer (tcpout) -- receives messages, discovers flight controllers
    |       |
    |       +-- broadcast channel (capacity 256) -- routes messages to subscribers
    |       +-- Context.available_systems -- tracks discovered system IDs
    |
    +-- ModemAccessService -- serializes modem D-Bus access via request queue
    |       |
    |       +-- Discovered at startup, stored in Context
    |       +-- LteSensor and future consumers access modem through this service
    |
    +-- SensorManager -- spawns and manages sensor tasks
    |       |
    |       +-- PingSensor -- pings target host, tracks latency and loss
    |       +-- LteSensor -- reads LTE signal quality via modem access service
    |       +-- CpuTempSensor -- reads CPU temperature from sysfs
    |
    +-- MavlinkEnvWriter -- writes mavlink.env at startup, then exits
    +-- CameraEnvWriter -- writes camera.env at startup, then exits
```

### Components

- **Drone Component** (`mavlink/drone_component.rs`) - Connects as MAVLink client (tcpout). Drains the outgoing message queue at the configured interval and sends messages over TCP. Sends heartbeats every 1s (MAV_TYPE_ONBOARD_CONTROLLER).

- **Sniffer** (`mavlink/sniffer_component.rs`) - Connects as MAVLink TCP client (tcpout). Receives all messages and broadcasts them on a channel. Discovers flight controller system IDs from heartbeats (system ID < 200), filtering out internal component IDs.

- **Context** (`context.rs`) - Shared state holding config, broadcast/mpsc channels, discovered system IDs, sensor values, and modem access service. Thread-safe via Arc and RwLock.

- **ModemAccessService** (`services/modem_access.rs`) - Queue-based modem access proxy. Discovers modem via D-Bus at startup with auto-retry, then serializes AT command requests through an internal worker task. Implements `ModemAccess` trait. Stored in Context as `Arc<dyn ModemAccess>`.

- **Commands** (`mavlink/commands.rs`) - Defines 23 custom MAV_CMD_USER_1 subcommands (IDs 31011-31049) for link switching, telemetry, camera, and GPS control.

- **Telemetry Reporter** (`mavlink/telemetry_reporter.rs`) - Reads sensor values from Context at 1Hz and broadcasts them as COMMAND_LONG (MAV_CMD_USER_1) messages to both GCS and base station. Reports LTE radio telemetry (subcmd 31014), LTE IP telemetry with ping data (subcmd 31015), and up to 10 neighbor cells (subcmds 31040-31049).

### Sensor Subsystem

The sensor subsystem (`sensors/`) provides a trait-based framework for gathering telemetry data. Each sensor runs as its own tokio task at a configurable interval, storing results in shared Context.

- **Sensor trait** - Common interface: `name()` and `async fn run()` with Context and CancellationToken.
- **SensorManager** - Reads config, builds list of enabled sensors, spawns each as a tokio task.
- **PingSensor** (`sensors/ping.rs`) - Spawns a `ping` subprocess, sends periodic SIGQUIT to get stats, parses latency and packet loss. Stores `PingReading { reachable, latency_ms, loss_percent }`.
- **LteSensor** (`sensors/lte.rs`) - Reads LTE signal quality via modem access service from Context. Waits for modem availability at startup, detects modem type (SIMCOM 7600, Quectel EM12/EM06E/EM06GL), then sends modem-specific AT commands to read signal quality. Stores `LteReading { signal, neighbors }`.
- **CpuTempSensor** (`sensors/cpu_temp.rs`) - Reads `/sys/class/thermal/thermal_zone0/temp`, converts millidegrees to degrees. Stores `CpuTempReading { temperature_c }`.

Each sensor can be independently enabled/disabled and has a configurable polling interval (with a global default fallback).

### Env File Writers

The env module (`env/`) generates environment files for external services at startup. Each writer implements the `Task` trait, writes its file once (creating parent directories if needed), then exits.

- **MavlinkEnvWriter** (`env/mavlink_env.rs`) - Writes mavlink.env with GCS_IP, REMOTE_MAVLINK_PORT, SNIFFER_SYS_ID, LOCAL_MAVLINK_PORT, FC_TTY, FC_BAUDRATE. Path configured via `mavlink.env_path`.
- **CameraEnvWriter** (`env/camera_env.rs`) - Writes camera.env with GCS_IP, REMOTE_VIDEO_PORT, CAMERA_WIDTH, CAMERA_HEIGHT, CAMERA_FRAMERATE, CAMERA_BITRATE, CAMERA_FLIP, CAMERA_TYPE, CAMERA_DEVICE. Path configured via `camera.env_path`.

### Startup Sequence

1. Parse CLI arguments and load TOML config
2. Create shared Context with channels and sensor value storage
3. Spawn modem discovery as background task (ModemAccessService::start(), stores in Context when ready)
4. Spawn env file writers (MavlinkEnvWriter, CameraEnvWriter) — write config-derived env files and exit
5. Spawn SensorManager (starts enabled sensor tasks; LteSensor waits for modem in Context)
6. Spawn drone component and sniffer tasks (with heartbeat loops)
7. Spawn TelemetryReporter (1Hz sensor value broadcasts)
8. Wait for flight controller discovery (heartbeat with system ID < 200)
9. Run until SIGINT/SIGTERM triggers graceful shutdown

## Testing

```bash
cargo test
```

Tests cover config parsing, custom command encoding/decoding, channel behavior, heartbeat construction, message queue drain, system discovery, integration with a mock TCP server, sensor value parsing (ping output, AT command responses, sysfs temperature), sensor manager construction, telemetry message construction, concurrent sensor value access, and env file content generation and file write verification.

## Linting

```bash
cargo clippy
cargo fmt --check
```
