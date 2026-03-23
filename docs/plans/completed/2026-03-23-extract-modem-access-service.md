# Extract Modem Access Service

## Overview
- Separate the modem communication layer (D-Bus discovery, AT commands) from `sensors/lte.rs` into a standalone service at `unitctl/src/services/modem_access.rs`
- The service owns a **request queue** (mpsc channel) — callers submit AT command requests, a single internal worker processes them sequentially against D-Bus (single-threaded D-Bus constraint)
- The service implements `ModemAccess` trait itself (queue-based proxy), so consumers use the same interface
- Handles modem discovery with auto-retry, stored in `Context` as `Arc<dyn ModemAccess>`
- LteSensor becomes simpler — gets modem from Context, sends commands through the queue

## Context (from discovery)
- **ModemAccess trait** defined in `sensors/lte.rs:116-128` — `model()` and `command()` methods
- **DbusModemAccess** in `sensors/lte.rs:163-245` — real D-Bus implementation in `dbus` submodule
- **ModemError, ModemType, detect_modem_type** — supporting types in `sensors/lte.rs:15-55`
- **LteSensor::run()** at line 535 — currently handles discovery + retry loop itself
- **Context** in `context.rs` — shared state hub, no services directory exists yet
- **No `services/` module** — needs to be created and wired into `main.rs`
- **D-Bus is single-threaded** — concurrent AT commands from different tasks would conflict

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: required for every task
- Mock-based tests for modem access service (reuse existing `MockModem` pattern from lte.rs tests)

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: Create services module with modem_access.rs — move types and trait
- [x] Create `unitctl/src/services/mod.rs` with `pub mod modem_access;`
- [x] Create `unitctl/src/services/modem_access.rs` — move `ModemAccess` trait, `ModemError`, `ModemType`, `MODEM_IDENTIFIERS`, `detect_modem_type()`, `discover_modem()`, `send_at_command()`, and `dbus` submodule from `sensors/lte.rs`
- [x] Add `mod services;` to `main.rs`
- [x] Update `sensors/lte.rs` to import from `crate::services::modem_access` instead of defining these types locally
- [x] Update any other files that import from `sensors::lte` for moved types (check `telemetry_reporter.rs`, `context.rs`)
- [x] Verify `cargo build` passes with no functional changes
- [x] Run tests — must pass before next task

### Task 2: Add modem field to Context
- [x] Add `pub modem: RwLock<Option<Arc<dyn ModemAccess>>>` field to `Context` struct
- [x] Update `Context::new()` to initialize modem field as `None`
- [x] Add `pub async fn set_modem(&self, modem: Arc<dyn ModemAccess>)` and `pub async fn get_modem(&self) -> Option<Arc<dyn ModemAccess>>` methods
- [x] Update test helpers (`test_config()` usage in context tests) to accommodate the new field
- [x] Write tests for `set_modem` / `get_modem` (set then get returns Some, initial is None)
- [x] Run tests — must pass before next task

### Task 3: Build queue-based ModemAccessService
- [x] Define `NetworkRegistration` enum — `NotRegistered`, `RegisteredHome`, `Searching`, `Denied`, `Unknown`, `RegisteredRoaming`
- [x] Define `ModemRequest` enum — variants: `Model`, `Command`, `Imsi` (AT+CIMI), `RegistrationStatus` (AT+CREG?/AT+CEREG?) — each with `oneshot::Sender` reply
- [x] Create `ModemAccessService` struct holding `mpsc::Sender<ModemRequest>` — this is the handle callers use
- [x] Implement `ModemAccess` trait for `ModemAccessService` — `model()`, `command()`, `imsi()`, and `registration_status()` create a `ModemRequest`, send it on the mpsc channel, and await the oneshot reply
- [x] Create `ModemAccessWorker` (or internal fn) — owns `mpsc::Receiver<ModemRequest>` and the real `DbusModemAccess`, processes requests sequentially in a loop
- [x] `ModemAccessService::start()` — async constructor that discovers modem (with retry + cancellation), spawns the worker task, returns `Arc<ModemAccessService>`
- [x] Worker handles `Imsi` — sends `AT+CIMI`, parses IMSI string from response
- [x] Worker handles `RegistrationStatus` — sends `AT+CREG?` or `AT+CEREG?`, parses registration status code into `NetworkRegistration` enum
- [x] Worker loop: recv request → match variant → call real `DbusModemAccess` method → send reply on oneshot
- [x] Worker exits when mpsc sender is dropped or cancellation fires
- [x] Write tests: service creation with mock D-Bus backend
- [x] Write tests: concurrent `command()` calls are serialized (verify ordering)
- [x] Write tests: service handles caller drop (oneshot sender dropped before reply)
- [x] Run tests — must pass before next task

### Task 4: Simplify LteSensor to use modem from Context
- [x] Remove modem discovery loop from `LteSensor::run()` — instead get modem from `ctx.get_modem()`
- [x] If modem not yet available in Context, wait/retry with cancellation support
- [x] Remove `dbus::DbusModemAccess::discover()` call from LteSensor
- [x] `poll_loop` already takes `&dyn ModemAccess` — no signature change needed
- [x] Update existing LteSensor tests for the new flow
- [x] Write tests for LteSensor behavior when modem is not yet available in Context
- [x] Run tests — must pass before next task

### Task 5: Wire modem service into startup in main.rs
- [x] In `main.rs`, after Context creation, spawn modem discovery as a background tokio task
- [x] Background task: calls `ModemAccessService::start()`, then `ctx.set_modem()` when ready
- [x] Sensors that need modem wait for it via `ctx.get_modem()` (polling with delay)
- [x] Verify startup sequence: Context → modem discovery (background) → sensor manager → other components
- [x] Write integration test or verify existing integration tests still pass
- [x] Run full test suite — must pass before next task

### Task 6: Verify acceptance criteria
- [x] Verify `ModemAccess` trait, `ModemError`, `ModemType` live in `services/modem_access.rs`
- [x] Verify `DbusModemAccess` and D-Bus logic live in `services/modem_access.rs`
- [x] Verify LteSensor no longer handles modem discovery directly
- [x] Verify modem service is initialized at startup and stored in Context
- [x] Verify AT commands from multiple threads are serialized through the queue
- [x] Run full test suite (unit tests)
- [x] Run linter (`cargo clippy`) — all issues must be fixed
- [x] Verify `cargo fmt --check` passes

### Task 7: [Final] Update documentation
- [x] Update CLAUDE.md architecture section to reflect new `services/` module
- [x] Update CLAUDE.md key types section with `ModemAccessService`

## Technical Details

### Architecture: Queue-based modem access

```
  LteSensor ──┐                        ┌─────────────────────┐
              │  ModemRequest (mpsc)    │  ModemAccessWorker  │
  Future      ├──────────────────────►  │  (single task)      │
  consumer ───┤                         │                     │
              │  ◄── oneshot reply ──── │  DbusModemAccess    │
              └                         │  (real D-Bus calls) │
                                        └─────────────────────┘
```

Callers get `Arc<ModemAccessService>` which implements `ModemAccess`.
Each call creates a `ModemRequest` with a oneshot reply channel, sends it
on the mpsc, and awaits the reply. The worker processes requests one at a
time, ensuring D-Bus single-threaded safety.

### Types to move from `sensors/lte.rs` → `services/modem_access.rs`
- `ModemType` enum + `Display` impl
- `MODEM_IDENTIFIERS` const
- `detect_modem_type()` fn
- `ModemError` enum + `Display` + `Error` impls
- `ModemAccess` trait
- `discover_modem()` fn
- `send_at_command()` fn
- `dbus` module (`DbusModemAccess` struct + `ModemAccess` impl)

### Types staying in `sensors/lte.rs`
- `LteSignalQuality`, `LteNeighborCell`, `LteReading` — sensor data types
- All `parse_*` functions — AT response parsing
- `LteSensor` struct + `Sensor` impl
- `expire_neighbors()`, `current_time_secs()`, `serving_cell_command()`, `supports_neighbor_query()`

### New types in `services/modem_access.rs`

```rust
/// Network registration status returned by the modem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRegistration {
    NotRegistered,
    RegisteredHome,
    Searching,
    Denied,
    Unknown,
    RegisteredRoaming,
}

/// Request submitted to the modem worker queue.
enum ModemRequest {
    Model {
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    Command {
        cmd: String,
        timeout_ms: u32,
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    /// Read SIM IMSI via AT+CIMI.
    Imsi {
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    /// Query network registration status via AT+CREG? or AT+CEREG?.
    RegistrationStatus {
        reply: oneshot::Sender<Result<NetworkRegistration, ModemError>>,
    },
}

/// Queue-based modem access proxy. Implements ModemAccess.
/// Safe to share across threads — requests are serialized by the worker.
pub struct ModemAccessService {
    tx: mpsc::Sender<ModemRequest>,
}

impl ModemAccessService {
    /// Discover modem (with retry), spawn worker, return service handle.
    pub async fn start(cancel: &CancellationToken) -> Result<Arc<Self>, ModemError>;
}

#[async_trait]
impl ModemAccess for ModemAccessService {
    async fn model(&self) -> Result<String, ModemError> { /* send Model request, await reply */ }
    async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError> { /* send Command, await reply */ }
    async fn imsi(&self) -> Result<String, ModemError> { /* send Imsi request, await reply */ }
    async fn registration_status(&self) -> Result<NetworkRegistration, ModemError> { /* send RegistrationStatus, await reply */ }
}
```

### Context field
```rust
pub modem: RwLock<Option<Arc<dyn ModemAccess>>>,
```

## Post-Completion

**Manual verification:**
- Test on hardware with actual modem to verify D-Bus discovery still works
- Verify LTE telemetry data appears in MAVLink stream after startup
- Verify concurrent AT command requests from multiple tasks are properly serialized
