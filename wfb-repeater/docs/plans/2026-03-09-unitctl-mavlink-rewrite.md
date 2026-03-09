# Unitctl MAVLink System Rewrite

## Overview
- Rewrite the MAVLink subsystem of connection_balancer from Python to Rust (unitctl)
- Components: MavlinkDroneComponent, MavlinkSniffer, custom command protocol (31011-31049)
- Clean rewrite — can redesign config format and internal interfaces while maintaining MAVLink protocol compatibility
- Uses Tokio async runtime + `mavlink` crate for protocol handling

## Context (from discovery)
- **Source code:** `connection_balancer/app/tasks/mavlink_*.py`, `app/include/context.py`, `app/include/mav_cmd_user_1_subcmd.py`
- **Rust target:** `unitctl/` (currently a hello-world stub)
- **Architecture pattern:** Async tasks communicating via shared Context + blinker pub/sub → Tokio tasks + broadcast/mpsc channels
- **MAVLink connection:** TCP to mavlink-routerd (127.0.0.1:5760), heartbeat exchange, message queue drain
- **Custom commands:** MAV_CMD_USER_1 with subcommand IDs 31011-31049 for link switching, telemetry, camera, GPS
- **Shell integration:** Calls `drone-link-switch.sh` for link failover, updates env files
- **Config:** INI format (`config.ini`) — free to redesign in Rust (e.g., TOML)

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
- Maintain MAVLink v2 protocol compatibility

## Testing Strategy
- **Unit tests**: required for every task — use `#[cfg(test)]` modules
- **Integration tests**: test MAVLink message round-trip with mock TCP server
- Test custom command encoding/decoding thoroughly
- Test connection state machine transitions
- Test telemetry report generation

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this codebase - code changes, tests, documentation updates
- **Post-Completion** (no checkboxes): items requiring external action - manual testing on hardware, deployment verification

## Implementation Steps

### Task 1: Project scaffolding and dependencies
- [ ] Fix Cargo.toml edition (2024 → 2021) and add dependencies: `tokio`, `mavlink`, `serde`, `toml`, `tracing`, `tracing-subscriber`, `clap`
- [ ] Create module structure: `src/{main.rs, config.rs, context.rs, mavlink/mod.rs}`
- [ ] Implement CLI argument parsing with clap (--config path, --debug flag)
- [ ] Implement config file loading (TOML format) with all MAVLink-relevant settings: protocol, host, port, sysids, compids, iteration_period_ms, fc_tty, fc_baudrate
- [ ] Set up tracing/logging with configurable level (debug/info)
- [ ] Write tests for config parsing (valid config, missing fields, defaults)
- [ ] Run `cargo test` — must pass before next task

### Task 2: Shared context and message types
- [ ] Define custom command enum matching Python's `MavCmdUser1SubCmd` (31011-31049 IDs)
- [ ] Add `tokio::sync::broadcast` channel for received MAVLink message routing (replaces blinker)
- [ ] Add `tokio::sync::mpsc` channel for outgoing message queue (capacity 500)
- [ ] Write tests for custom command enum conversions (ID ↔ enum)
- [ ] Write tests for Context creation and state access
- [ ] Run `cargo test` — must pass before next task

### Task 3: MAVLink connection core (MavlinkDroneComponent)
- [ ] Implement async TCP connection to mavlink-routerd (configurable host:port)
- [ ] Implement MAVLink v2 heartbeat sending (MAV_TYPE_ONBOARD_CONTROLLER, system_id from config)
- [ ] Implement outgoing message queue drain loop (configurable interval, default 10ms)
- [ ] Implement reconnection logic with 1s backoff on connection failure
- [ ] Implement graceful shutdown on task cancellation
- [ ] Write tests for heartbeat message construction
- [ ] Write tests for message queue drain behavior (mock channel)
- [ ] Run `cargo test` — must pass before next task

### Task 4: MAVLink sniffer (MavlinkSniffer)
- [ ] Implement separate TCP connection with sniffer_sysid (199)
- [ ] Implement continuous message receive loop with `recv_match` equivalent
- [ ] Implement HEARTBEAT message handling — auto-discover flight controller system IDs, update Context.available_systems
- [ ] Implement message routing — broadcast received messages on the broadcast channel
- [ ] Implement periodic heartbeat sending from sniffer
- [ ] Write tests for system ID discovery from heartbeat messages
- [ ] Write tests for message type filtering and routing
- [ ] Run `cargo test` — must pass before next task

### Task 6: Integration and main loop
- [ ] Wire up all components in `main.rs`: config → context → spawn tasks with `tokio::select!` / `tokio::spawn`
- [ ] Implement `wait_for_fc()` — block until flight controller heartbeat detected (matching Python's `get_fc_system_id`)
- [ ] Implement graceful shutdown (SIGTERM/SIGINT handling via `tokio::signal`)
- [ ] Add logging at key lifecycle points (connection established, link switch, FC detected, errors)
- [ ] Write integration test: mock TCP server, verify heartbeat exchange and message routing
- [ ] Run `cargo test` — must pass before next task

### Task 7: Verify acceptance criteria
- [ ] Verify all MAVLink custom commands (31011-31049) are implemented
- [ ] Verify heartbeat protocol matches Python behavior (MAV_TYPE_ONBOARD_CONTROLLER)
- [ ] Verify connection switcher guards match Python logic
- [ ] Verify telemetry reporter sends all message types at 1Hz
- [ ] Run full test suite (`cargo test`)
- [ ] Run `cargo clippy` — all warnings must be fixed
- [ ] Run `cargo fmt --check` — formatting must pass
- [ ] Verify `cargo build --release` succeeds

### Task 9: [Final] Update documentation
- [ ] Add README.md to unitctl/ with build instructions, config format, and usage
- [ ] Update project CLAUDE.md with unitctl architecture notes
- [ ] Create example TOML config file (`unitctl/config.toml.example`)

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Config Format (TOML)
```toml
[general]
debug = false

[mavlink]
protocol = "tcp"
host = "127.0.0.1"
port = 5760
self_sysid = 1
self_compid = 10
gcs_sysid = 255
gcs_compid = 190
sniffer_sysid = 199
bs_sysid = 200
iteration_period_ms = 10

[mavlink.fc]
tty = "/dev/ttyFC"
baudrate = 57600
```

### Rust Crate Dependencies
```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
mavlink = { version = "0.14", features = ["ardupilotmega"] }
serde = { version = "1", features = ["derive"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = "0.3"
clap = { version = "4", features = ["derive"] }
```

### Architecture
```
main.rs
├── config.rs          — TOML config loading, CLI args
├── context.rs         — Shared state (Arc<RwLock>), channels
└── mavlink/
    ├── mod.rs         — Re-exports
    ├── commands.rs    — Custom command enum (31011-31049)
    ├── drone.rs       — MavlinkDroneComponent (outgoing connection)
    ├── sniffer.rs     — MavlinkSniffer (passive listener)
    ├── switcher.rs    — MavlinkConnectionSwitcher (link failover)
    └── telemetry.rs   — MavlinkTelemetryReporter (1Hz broadcasts)
```

### Message Flow
```
mavlink-routerd (TCP:5760)
    ↕ MAVLink v2
MavlinkDroneComponent ←── outgoing mpsc queue
    ↕
MavlinkSniffer ──→ broadcast channel ──→ Context (HEARTBEAT → system discovery)
```

**Future migration phases:**
- LTE monitoring rewrite
- WFB monitoring rewrite
- Camera manager rewrite
- Base station connector rewrite
- Web server rewrite
