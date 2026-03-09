# Unitctl Sensor Subsystem

## Overview
- Add a telemetry sensor gathering subsystem to unitctl
- Trait-based sensor framework: each sensor runs as its own tokio task, stores results in shared Context
- Configurable per-sensor: enable/disable, custom interval (with global default)
- Three initial sensors: ping, LTE telemetry (AT commands), CPU temperature
- Sensors are independent — each reads its own config section from config file, runs at its own interval, updates Context

## Context (from discovery)
- **Rust target:** `unitctl/src/` — existing Tokio-based async architecture with `Arc<Context>` shared state
- **Python references:**
  - `connection_balancer/app/include/ping.py` — spawns `ping` subprocess, parses SIGQUIT stats output (latency, loss%)
  - `connection_balancer/app/tasks/lte_monitoring.py` — AT command modem detection (SIMCOM_7600, QUECTEL_EM12, EM06E, EM06GL), signal quality parsing
  - `connection_balancer/app/include/lte_signal_quality.py` — LteSignalQuality struct (rsrq, rsrp, rssi, rssnr, earfcn, tx_power, pcid, neighbor_cells)
- **D-Bus / ModemManager:** LTE modem communication via `org.freedesktop.ModemManager1` D-Bus interface instead of direct serial. Use [`modemmanager`](https://github.com/omnect/modemmanager/) crate for D-Bus communication. Modem detection via D-Bus properties, AT commands via `org.freedesktop.ModemManager1.Modem.Command` method
- **Modem control interface:** Define an interface for modem status changes (enable/disable, set bands) — not wired to anything initially, but available for future use
- **Existing unitctl patterns:** `CancellationToken` for shutdown, `tokio::spawn` for tasks, TOML config with serde defaults
- **Dependencies to add:** `modemmanager` (ModemManager D-Bus client), `nix` (signals for ping subprocess)

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change

## Testing Strategy
- **Unit tests**: required for every task — use `#[cfg(test)]` modules
- Test sensor value parsing (AT response parsing, ping output parsing, sysfs reading)
- Test config deserialization (defaults, per-sensor overrides, enable/disable)
- Test sensor trait implementations with mock data
- Test neighbor cell tracking (add, update, expiry)

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this codebase
- **Post-Completion** (no checkboxes): manual testing on hardware, deployment

## Implementation Steps

### Task 1: Sensor framework and config structure
- [x] Add dependencies to `Cargo.toml`: `modemmanager` (ModemManager D-Bus client, from https://github.com/omnect/modemmanager/), `nix` (features: signal, process)
- [x] Create `unitctl/src/sensors/mod.rs` with `Sensor` trait:
  ```rust
  #[async_trait]
  pub trait Sensor: Send + Sync {
      fn name(&self) -> &str;
      async fn run(&self, ctx: Arc<Context>, cancel: CancellationToken);
  }
  ```
- [x] Create `SensorManager` in `sensors/mod.rs`: takes config, builds list of enabled sensors, spawns each as `tokio::spawn` with cancel token
- [x] Extend `Config` in `config.rs` with `[sensors]` section:
  ```toml
  [sensors]
  default_interval_s = 1.0

  [sensors.ping]
  enabled = true
  interval_s = 1.0  # optional, overrides default
  host = "10.45.0.2"
  interface = ""  # optional, bind to interface

  [sensors.lte]
  enabled = true
  interval_s = 1.0
  neighbor_expiry_s = 30.0

  [sensors.cpu_temp]
  enabled = true
  interval_s = 5.0
  ```
- [x] Add `SensorValues` struct to Context for storing current sensor readings (behind `Arc<RwLock<>>`)
- [x] Register `sensors` module in `main.rs`
- [x] Write tests for config parsing: sensor defaults, per-sensor interval override, disabled sensor
- [x] Write tests for SensorManager: builds correct sensor list based on enabled config
- [x] Run `cargo test` — must pass before next task

### Task 2: Ping sensor
- [x] Create `unitctl/src/sensors/ping.rs`
- [x] Implement `PingSensor` struct implementing `Sensor` trait
- [x] Spawn `ping -q -i0.2 [-I<iface>] <host>` as async subprocess using `tokio::process::Command`
- [x] Implement periodic SIGQUIT (signal 3) sending at configured interval using `nix::sys::signal::kill`
- [x] Parse ping stderr output with regex: extract packets_sent, packets_rcvd, latency_ms (ewma)
- [x] Calculate loss_percent from delta sent/rcvd
- [x] Store result in Context: `PingReading { reachable: bool, latency_ms: f64, loss_percent: u8 }`
- [x] Handle subprocess restart on exit/error
- [x] Write tests for ping output parsing (connected case, disconnected case, partial loss)
- [x] Write tests for loss percentage calculation edge cases
- [x] Run `cargo test` — must pass before next task

### Task 3: LTE telemetry sensor — ModemManager integration and modem detection
- [x] Create `unitctl/src/sensors/lte.rs`
- [x] Define `ModemType` enum: `Simcom7600`, `QuectelEm12`, `QuectelEm06E`, `QuectelEm06GL`
- [x] Define modem identifier map: `"SIMCOM_SIM7600G-H"` → Simcom7600, `"EM12"` → QuectelEm12, `"EM06"` → QuectelEm06E, `"EM060K-GL"` → QuectelEm06GL
- [x] Use `modemmanager` crate to connect to ModemManager D-Bus service and enumerate modems
- [x] Detect modem type: read modem `Model` property via `modemmanager` API, match to modem identifier map
- [x] Implement AT command execution via `modemmanager` crate's `Command` method (wraps `org.freedesktop.ModemManager1.Modem.Command`)
- [x] Read current bands from modem via `modemmanager` API (`CurrentBands` property), store in telemetry
- [x] Write tests for modem type detection from Model property values
- [x] Write tests for AT command response handling (success, timeout, D-Bus error)
- [x] Run `cargo test` — must pass before next task

### Task 4: LTE telemetry sensor — signal quality parsing
- [x] Define `LteSignalQuality` struct: `rsrq, rsrp, rssi, rssnr, earfcn, tx_power, pcid` (all i32)
- [x] Define `LteNeighborCell` struct: `pcid, rsrp, rsrq, rssi, rssnr, earfcn` (all i32) + `last_seen: u64`
- [x] Implement SIMCOM_7600 response parser: parse `AT+CPSI?` response, extract signal fields
- [x] Implement QUECTEL_EM12 serving cell parser: parse `AT+QENG="servingcell"` response
- [x] Implement QUECTEL_EM12 neighbor cell parser: parse `AT+QENG="neighbourcell"` response, track in HashMap by pcid
- [x] Implement QUECTEL_EM06E parser: same as EM12 serving cell but without tx_power field
- [x] Implement QUECTEL_EM06GL parser: same as EM12 serving cell format
- [x] Implement neighbor cell expiry: remove cells not seen for `neighbor_expiry_s` seconds
- [x] Store result in Context: `LteReading { signal: LteSignalQuality, neighbors: HashMap<i32, LteNeighborCell>, current_bands: Vec<String> }`
- [x] Write `LteSensor` struct implementing `Sensor` trait: run loop with ModemManager modem discovery → AT command polling + band reading at interval
- [x] Write tests for SIMCOM_7600 AT+CPSI? response parsing (valid data, partial data, invalid)
- [x] Write tests for QUECTEL_EM12 serving cell parsing
- [x] Write tests for QUECTEL_EM12 neighbor cell parsing and expiry
- [x] Write tests for QUECTEL_EM06E and EM06GL parsing
- [x] Run `cargo test` — must pass before next task

### Task 5: LTE modem control interface
- [x] Create `unitctl/src/sensors/lte_control.rs` with `ModemControl` struct
- [x] Define `ModemControl` trait/interface with methods (not wired to anything yet, for future use):
  - `async fn enable(&self) -> Result<()>` — enable modem via ModemManager
  - `async fn disable(&self) -> Result<()>` — disable modem via ModemManager
  - `async fn set_bands(&self, bands: &[String]) -> Result<()>` — set allowed bands via ModemManager `SetCurrentBands` method
  - `async fn get_bands(&self) -> Result<Vec<String>>` — read current bands
- [x] Implement `ModemControl` using `modemmanager` crate, wrapping the D-Bus modem object
- [x] Store `ModemControl` instance in Context (behind `Arc<RwLock<Option<...>>>`) so it can be used by future components
- [x] Write tests for ModemControl construction and method signatures
- [x] Run `cargo test` — must pass before next task

### Task 6: CPU temperature sensor
- [x] Create `unitctl/src/sensors/cpu_temp.rs`
- [x] Implement `CpuTempSensor` struct implementing `Sensor` trait
- [x] Read temperature from `/sys/class/thermal/thermal_zone0/temp` (millidegrees Celsius)
- [x] Convert to degrees: value / 1000.0
- [x] Store result in Context: `CpuTempReading { temperature_c: f64 }`
- [x] Run loop at configured interval, handle file read errors gracefully
- [x] Write tests for temperature parsing (valid, invalid, missing file)
- [x] Run `cargo test` — must pass before next task

### Task 7: Integration and wiring
- [x] Wire SensorManager into `main.rs`: create after Context, spawn as tokio task
- [x] Ensure SensorManager respects CancellationToken for graceful shutdown
- [x] Add tracing logs at sensor lifecycle points (started, reading, error, stopped)
- [x] Update example config file with `[sensors]` section
- [x] Write integration test: create SensorManager with mock config, verify task spawning
- [x] Run `cargo test` — must pass before next task

### Task 8: MAVLink telemetry drain for LTE and ping sensors
- [x] Create `unitctl/src/mavlink/telemetry.rs`
- [x] Implement `TelemetryReporter` that reads sensor values from Context and sends MAVLink messages via outgoing mpsc queue
- [x] Implement LTE radio telemetry message (subcmd `LteRadioTelemetry` / 31014):
  - param1: subcmd ID, param2: rssi, param3: rsrq, param4: rsrp, param5: rssnr, param6: earfcn, param7: tx_power
- [x] Implement LTE IP telemetry message (subcmd `LteIpTelemetry` / 31015):
  - param1: subcmd ID, param2: is_connected (from ping sensor reachable), param3: latency_ms, param4: loss_percent, param5: pcid, param6: neighbor_count, param7: 0
- [x] Implement neighbor cell telemetry (subcmds `LteIpTelemetryNeighbors0..9` / 31040-31049):
  - For each neighbor (up to 10): param1: subcmd ID, param2: pcid, param3: rsrp, param4: rssi, param5: rsrq, param6: rssnr, param7: earfcn
- [x] Send all messages to both GCS (gcs_sysid) and base station (bs_sysid) targets using `COMMAND_LONG` with `MAV_CMD_USER_1`
- [x] Run telemetry loop at 1Hz, reading latest sensor values from Context
- [x] Wire TelemetryReporter into `main.rs` as a tokio task with CancellationToken
- [x] Write tests for LTE radio telemetry message construction with known values
- [x] Write tests for LTE IP telemetry message construction (connected/disconnected cases)
- [x] Write tests for neighbor cell telemetry message construction (0, 5, 10+ neighbors)
- [x] Run `cargo test` — must pass before next task

### Task 9: Verify acceptance criteria
- [x] Verify all 3 sensors can be enabled/disabled independently via config
- [x] Verify default interval works and per-sensor override works
- [x] Verify sensor values are stored in Context correctly
- [x] Verify LTE and ping telemetry drains to MAVLink at 1Hz with correct subcmd IDs
- [x] Run full test suite (`cargo test`)
- [x] Run `cargo clippy` — all warnings must be fixed
- [x] Run `cargo fmt --check` — formatting must pass
- [x] Verify `cargo build --release` succeeds

### Task 10: [Final] Update documentation
- [x] Update unitctl README.md with sensor subsystem docs
- [x] Document config format for sensors section
- [x] Update CLAUDE.md if new patterns discovered

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Config Format (TOML addition)
```toml
[sensors]
default_interval_s = 1.0

[sensors.ping]
enabled = true
interval_s = 1.0
host = "10.45.0.2"
interface = ""

[sensors.lte]
enabled = true
interval_s = 1.0
neighbor_expiry_s = 30.0

[sensors.cpu_temp]
enabled = true
interval_s = 5.0
```

### Sensor Value Types in Context
```rust
pub struct SensorValues {
    pub ping: RwLock<Option<PingReading>>,
    pub lte: RwLock<Option<LteReading>>,
    pub cpu_temp: RwLock<Option<CpuTempReading>>,
}

pub struct PingReading {
    pub reachable: bool,
    pub latency_ms: f64,
    pub loss_percent: u8,
}

pub struct LteReading {
    pub signal: LteSignalQuality,
    pub neighbors: HashMap<i32, LteNeighborCell>,
    pub current_bands: Vec<String>,
}

pub struct CpuTempReading {
    pub temperature_c: f64,
}
```

### ModemManager Crate
Use [`modemmanager`](https://github.com/omnect/modemmanager/) crate for all D-Bus communication.
This crate wraps `org.freedesktop.ModemManager1` D-Bus interface and provides typed Rust API.

Key operations used:
- **Enumerate modems:** list available modem objects
- **Model property:** detect modem type for AT command selection
- **Command method:** send AT commands, receive responses (wraps `org.freedesktop.ModemManager1.Modem.Command`)
- **CurrentBands property:** read active LTE bands (telemetry)
- **SetCurrentBands method:** set allowed bands (modem control interface, for future use)
- **Enable/Disable:** modem power control (modem control interface, for future use)

### MAVLink Telemetry Drain (1Hz)
Sensor values are drained to MAVLink as `COMMAND_LONG` messages with `MAV_CMD_USER_1`, sent to both GCS and base station:

| Message | Subcmd ID | param2 | param3 | param4 | param5 | param6 | param7 |
|---------|-----------|--------|--------|--------|--------|--------|--------|
| LTE Radio | 31014 | rssi | rsrq | rsrp | rssnr | earfcn | tx_power |
| LTE IP | 31015 | is_connected | latency_ms | loss_% | pcid | neighbor_count | 0 |
| Neighbor N | 31040+N | pcid | rsrp | rssi | rsrq | rssnr | earfcn |

- `is_connected` and `latency_ms`/`loss_%` come from the **ping sensor** reading
- Signal quality fields come from the **LTE sensor** reading
- Up to 10 neighbor cells reported (subcmds 31040-31049)

### AT Commands by Modem Type (sent via D-Bus Command method)
| Modem | Serving Cell Command | Neighbor Command | Response Prefix |
|-------|---------------------|-----------------|-----------------|
| SIMCOM_7600 | `AT+CPSI?` | — | `+CPSI:` |
| QUECTEL_EM12 | `AT+QENG="servingcell"` | `AT+QENG="neighbourcell"` | `+QENG:` |
| QUECTEL_EM06E | `AT+QENG="servingcell"` | `AT+QENG="neighbourcell"` | `+QENG:` |
| QUECTEL_EM06GL | `AT+QENG="servingcell"` | `AT+QENG="neighbourcell"` | `+QENG:` |

### Module Structure
```
unitctl/src/
├── main.rs
├── config.rs          — extended with [sensors] section
├── context.rs         — extended with SensorValues
├── mavlink/           — existing (unchanged)
└── sensors/
    ├── mod.rs         — Sensor trait, SensorManager
    ├── ping.rs        — PingSensor
    ├── lte.rs         — LteSensor, ModemManager integration, AT response parsing, band reading
    ├── lte_control.rs — ModemControl interface (enable/disable, set bands) — for future use
    └── cpu_temp.rs    — CpuTempSensor
├── mavlink/
│   ├── ...            — existing modules (unchanged)
│   └── telemetry.rs   — TelemetryReporter (drains LTE + ping sensor values to MAVLink)
```

## Post-Completion

**Manual verification (on hardware):**
- Test ping sensor with real network interface and target host
- Test LTE sensor with each supported modem type via ModemManager D-Bus (SIMCOM, Quectel variants)
- Verify CPU temperature reads correctly on Raspberry Pi / Armbian
- Verify graceful shutdown stops all sensor tasks
- Test with sensors disabled in config — verify no tasks spawned
- Test interval overrides — verify correct polling frequency

**Future enhancements:**
- Sensor health monitoring (detect stuck/failing sensors)
