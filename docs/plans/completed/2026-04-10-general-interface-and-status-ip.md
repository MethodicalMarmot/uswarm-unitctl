# Add unit IP to online status and unify interface config

## Overview

Add a shared `general.interface` configuration option that specifies the network interface name used by the unit. This interface is used for two purposes:

1. **PING sensor** — replaces the current `sensors.ping.interface` field. The ping subprocess is launched with `-I <interface>`.
2. **MQTT online status** — the IPv4 address of this interface is resolved and included in the online `NodeStatusEnvelope` published to `{prefix}/nodes/{nodeId}/status`.

Benefits:
- Single source of truth for "which interface represents this unit" instead of duplicating it per-sensor.
- Operators monitoring MQTT status can see each node's current IP without querying the device.

## Context (from discovery)

Files/components involved:
- `src/config.rs` — `GeneralConfig` (add `interface`), `PingSensorConfig` (remove `interface`), validation, embedded TOML test fixtures.
- `src/sensors/ping.rs` — `PingSensor::new` currently reads `config.interface`; must take interface from general config instead.
- `src/messages/status.rs` — `OnlineStatusData` gains an `ip: Option<String>` field.
- `src/services/mqtt/status.rs` — `StatusPublisher::publish_online` resolves IP before publishing.
- `src/main.rs` — wiring: pass `general.interface` to `PingSensor` and `StatusPublisher`; perform startup interface-IP check.
- `config.toml.example` — add `interface` under `[general]`, remove from `[sensors.ping]`.
- `Cargo.toml` — enable `net` feature on `nix`.
- `assets/schema/` — regenerated via `cargo run --bin generate-schema` / `make schema`.
- Tests embedded in `config.rs` (`test_dash_prefix_ping_interface_rejected` and full-config round-trip tests) need updates.

Related patterns:
- `GeneralConfig` currently only holds `debug: bool`; adding a `String` field is a pure extension.
- Ping validation at `config.rs:401` (reject leading `-`, alphanumeric/`._-` only) moves onto `general.interface`.
- `StatusPublisher` resolves state per-connect, not per-startup — ideal for also resolving IP on each reconnect so the published IP reflects current state.

Dependencies identified:
- `nix` 0.31.2 is already present with `signal, process` features. Enabling the `net` feature exposes `nix::ifaddrs::getifaddrs()` which yields `InterfaceAddress { interface_name, address: Option<SockaddrStorage>, .. }` — filter by name and extract IPv4.

## Development Approach

- **Testing approach**: Regular (code first, then tests).
- Complete each task fully before moving to the next.
- Make small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task.
- **CRITICAL: all tests must pass before starting next task** — no exceptions.
- **CRITICAL: update this plan file when scope changes during implementation**.
- Run `cargo test` and `cargo clippy` after each change.
- Maintain backward compatibility of MQTT message shape: `ip` is an `Option<String>` so receivers without the new field still parse old messages (omitted when `None` via `#[serde(skip_serializing_if = "Option::is_none")]`).

## Testing Strategy

- **Unit tests**: required for every task.
- No UI / e2e tests in this project.
- Specific coverage targets:
  - Config round-trip with `general.interface`.
  - Config validation: empty rejected, leading `-` rejected, bad chars rejected.
  - Ping sensor picks interface from general config.
  - IP resolver: named interface found, interface-missing error, no-IPv4 error, IPv6-only interface handled.
  - Online status JSON includes `ip` field when set and omits it (or is `null`) when unset.

## Progress Tracking

- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with +- prefix.
- Document issues/blockers with !! prefix.
- Update plan if implementation deviates from original scope.

## What Goes Where

- **Implementation Steps**: all in-repo code, tests, config examples, schema regeneration.
- **Post-Completion**: real-device verification, broker-side inspection of new `ip` field.

## Implementation Steps

### Task 1: Enable `nix` net feature and add interface IP resolver helper
- [x] update `Cargo.toml`: add `"net"` to the `nix` feature list.
- [x] create `src/net.rs` (new module) with `pub fn resolve_ipv4(interface: &str) -> Result<Ipv4Addr, ResolveIpError>` using `nix::ifaddrs::getifaddrs()`.
- [x] define `ResolveIpError` enum: `InterfaceNotFound`, `NoIpv4`, `Getifaddrs(nix::Error)`.
- [x] re-export module in `src/lib.rs`.
- [x] write unit test: resolving `"lo"` returns `127.0.0.1` (loopback is always present).
- [x] write unit test: unknown interface name returns `InterfaceNotFound`.
- [x] run `cargo test` and `cargo clippy` — must pass before next task.

### Task 2: Add `interface` to `GeneralConfig` with validation
- [x] add `pub interface: String` to `GeneralConfig` in `src/config.rs`.
- [x] move interface-name validation (non-empty, no leading `-`, alphanumeric/`.`/`_`/`-` only) from ping-specific block to a new `validate_general` check.
- [x] remove the `if !self.sensors.ping.interface.is_empty()` block around `config.rs:401` along with its now-unused branch.
- [x] update `GeneralConfig` default construction used in tests (`config.rs:145` area).
- [x] update every embedded `toml!` fixture in `config.rs` tests (`config.rs:508`, `:575`, `:733`, `:921`, `:1450`) to include `interface = "eth0"` (or `"lo"` where a real interface is needed).
- [x] write unit test: `general.interface = ""` is rejected with clear error.
- [x] write unit test: `general.interface = "-evil"` rejected (replaces `test_dash_prefix_ping_interface_rejected`).
- [x] write unit test: `general.interface = "eth0"` round-trips.
- [x] run `cargo test` and `cargo clippy` — must pass before next task.

### Task 3: Remove `sensors.ping.interface` and rewire ping sensor
- [x] delete `interface` field from `PingSensorConfig` in `src/config.rs`.
- [x] delete ping-specific interface validation (already moved in Task 2, confirm removal).
- [x] update `PingSensor::new` (and any constructor signature) in `src/sensors/ping.rs` to accept the interface from a different source — change the signature to `new(config: &PingSensorConfig, interface: String, ...)` or pass via `Context`.
- [x] update call site(s) in `src/sensors/mod.rs` / `src/main.rs` to thread `general.interface` into the ping sensor.
- [x] remove the `interface: ""` lines from `config.toml.example` under `[sensors.ping]`.
- [x] update existing ping sensor tests at `sensors/ping.rs:452` and `:466` to reflect new construction.
- [x] write unit test: ping sensor is built with the general interface value and stores it.
- [x] write unit test: the ping `Command` built in `run_ping_subprocess` includes `-I <interface>` when interface is non-empty (since `general.interface` is now required non-empty, this should always be true).
- [x] run `cargo test` and `cargo clippy` — must pass before next task.

### Task 4: Add `ip` to `OnlineStatusData`
- [x] add `pub ip: Option<String>` to `OnlineStatusData` in `src/messages/status.rs`.
- [x] add `#[serde(skip_serializing_if = "Option::is_none")]` so omitted when `None` for schema stability.
- [x] update all `OnlineStatusData { .. }` literals in existing tests (`messages/status.rs:56`, `:99`, and `services/mqtt/status.rs:111`) to include `ip: Some("192.0.2.1".to_string())` or `None`.
- [x] write unit test: JSON round-trip with `ip = Some("10.0.0.5")`.
- [x] write unit test: JSON with `ip = None` omits the field (and still round-trips).
- [x] write unit test: schema generation still succeeds and contains `"ip"` somewhere.
- [x] run `cargo test` — must pass before next task.

### Task 5: Resolve and publish IP in `StatusPublisher`
- [x] add an `interface: String` field to `StatusPublisher`; update `StatusPublisher::new` signature.
- [x] in `publish_online` (src/services/mqtt/status.rs:37), call `net::resolve_ipv4(&self.interface)` and convert to `Some(addr.to_string())`; on error log a `warn!` and use `None` (runtime resilience — startup already verified the interface).
- [x] populate `OnlineStatusData.ip` from the resolver result.
- [x] update call site in `src/main.rs` to pass `config.general.interface.clone()`.
- [x] update `status.rs` tests that construct `StatusPublisher::new(transport.clone(), cancel)` to include an interface argument (use `"lo"`).
- [x] write unit test: `publish_online` payload contains an `ip` field when resolver succeeds (use `"lo"` — `127.0.0.1` is deterministic).
- [x] write unit test: unknown interface path yields `ip: None` in payload (use a deliberately-invalid name like `"nonexistent9999"`).
- [x] run `cargo test` and `cargo clippy` — must pass before next task.

### Task 6: Startup interface-IP check in `main.rs`
- [x] after `config::load_config()` in `src/main.rs`, call `net::resolve_ipv4(&config.general.interface)`; on error, log with context and `exit(1)` (or return `Err` propagating out of `main`).
- [x] the check runs once at startup; subsequent failures during reconnect are non-fatal (handled in Task 5).
- [x] write integration-style unit test if feasible, otherwise document manual verification. (Not unit-testable due to process::exit; resolve_ipv4 is covered by net.rs tests; manual verification: run with invalid interface and confirm exit code 1.)
- [x] run `cargo test` and `cargo clippy` — must pass before next task.

### Task 7: Update `config.toml.example` and regenerate schemas
- [x] add `interface = "eth0"` under `[general]` in `config.toml.example`.
- [x] remove the now-stale `interface = ""` line under `[sensors.ping]`.
- [x] run `cargo run --bin generate-schema` (or `make schema`) to regenerate `assets/schema/*.json`.
- [x] verify diff of regenerated schemas looks correct (new `ip` field on status, `interface` on general, no `interface` on ping sensor).
- [x] run `cargo test` — must pass before next task.

### Task 8: Update `CLAUDE.md`
- [x] update the `[general]` bullet in "Configuration" section to document `interface`.
- [x] remove `interface` from the `[sensors.ping]` description.
- [x] update the `OnlineStatusData` entry in "Key Types" to mention `ip`.
- [x] update the `StatusPublisher` description to note it resolves IP per-connect from `general.interface`.

### Task 9: Verify acceptance criteria
- [x] `general.interface` is required and validated.
- [x] ping sensor uses `general.interface` and `sensors.ping.interface` no longer exists.
- [x] online MQTT status includes the IPv4 of `general.interface`.
- [x] startup fails fast if the interface has no IPv4.
- [x] `cargo test` passes.
- [x] `cargo clippy` is clean.
- [x] `cargo fmt --check` passes.
- [x] `cargo run --bin generate-schema` regenerates without drift.

## Technical Details

**Config change (before → after):**
```toml
[general]
debug = false
interface = "eth0"   # NEW — required

[sensors.ping]
enabled = true
host = "8.8.8.8"
# interface = ""     # REMOVED
```

**`OnlineStatusData` JSON shape (when IP resolves):**
```json
{
  "ts": "2026-04-10T12:00:00Z",
  "data": {
    "type": "Online",
    "session": "a8f2c1",
    "version": "0.2.0",
    "ip": "10.0.0.5"
  }
}
```
When resolution fails at runtime the `ip` field is omitted.

**IP resolver sketch:**
```rust
use std::net::Ipv4Addr;
use nix::ifaddrs::getifaddrs;
use nix::sys::socket::SockaddrLike;

pub fn resolve_ipv4(interface: &str) -> Result<Ipv4Addr, ResolveIpError> {
    let mut found_iface = false;
    for ifa in getifaddrs().map_err(ResolveIpError::Getifaddrs)? {
        if ifa.interface_name != interface { continue; }
        found_iface = true;
        if let Some(addr) = ifa.address {
            if let Some(sin) = addr.as_sockaddr_in() {
                return Ok(Ipv4Addr::from(sin.ip()));
            }
        }
    }
    if found_iface { Err(ResolveIpError::NoIpv4) } else { Err(ResolveIpError::InterfaceNotFound) }
}
```

**Validation rules for `general.interface`** (moved from ping):
- must be non-empty
- must not start with `-`
- must contain only alphanumeric, `.`, `_`, `-`

**Runtime vs startup failure semantics:**
- Startup: hard-fail if IP cannot be resolved — guarantees configuration correctness.
- Reconnect: soft-fail (publish with `ip` omitted + warning) — avoids killing a running daemon on a transient interface flap.

## Post-Completion

**Manual verification:**
- Deploy to a real unit, confirm MQTT online status message shows the correct IPv4 for the configured interface.
- Kill/restore the interface while running: verify reconnect publishes update without crashing; verify log warning on resolver failure.
- Verify ping sensor still binds to the configured interface (`tcpdump -i eth0 icmp`).

**External system updates:**
- MQTT broker consumers that parse `OnlineStatusData` can opt in to read the new `ip` field; old parsers continue to work because the field is optional.
- Any operator dashboards should be updated to surface the new `ip` field in online status displays.
