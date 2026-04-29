# Restart command — design

**Status:** draft
**Date:** 2026-04-29
**Author:** brainstormed with Claude

## Summary

Add a new `Restart` MQTT command that restarts a target service or reboots the host. Five targets: `camera`, `mavlink`, `modem`, `unitctl`, `reboot`. Wired into the existing MQTT command processing pipeline (`services/mqtt/commands.rs` + `services/mqtt/handlers/`).

## Motivation

The MQTT control plane has handlers for `get_config`, `config_update`, `update_request`, and `modem_commands`. Operators need a way to restart individual subsystems (e.g. camera streamer after a config change) and to reboot the device, without SSH access. The README's MQTT-commands checklist already lists `* [ ] restart` as planned work.

## Scope

In scope:
- New `Restart` variant in `CommandPayload` / `CommandResultData`.
- Targets: `camera`, `mavlink`, `modem`, `unitctl`, `reboot`.
- Handler that shells out to `systemctl restart <unit>` (with post-restart liveness verification) or `reboot`.
- For self-restart of unitctl: deferred completion result published on the next boot.
- New `general.env_dir` config field (state directory).

Out of scope:
- SIGHUP-style reload (every target is a hard restart).
- Reload of unitctl's own config without restarting the process.
- Max-age / staleness check on the deferred-completion state file.
- Restart cancellation or queueing of overlapping restart commands.
- Refactor of `mavlink.env_path` / `camera.env_path` into a shared directory layout.

## Config changes

Add `env_dir: String` to `GeneralConfig` (`config.rs`).

- Required, no serde default (per project convention — every field is explicit in `config.toml`).
- Example value in `config.toml.example`: `/var/run/unitctl`.
- Used by the restart handler for the pending-uuid state file. Available for other components in the future.
- `Config::validate()`: ensure `env_dir` is non-empty. The handler creates the parent directory on first write; no startup pre-creation.
- Must be writable by the unitctl user (root, per existing assumption).

## Message types (`src/messages/commands.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestartTarget {
    Camera,
    Mavlink,
    Modem,
    Unitctl,
    Reboot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestartPayload {
    pub target: RestartTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestartResult {
    pub target: RestartTarget,
}
```

Extend the existing tagged enums:
- `CommandPayload` gains `Restart(RestartPayload)`.
- `CommandResultData` gains `Restart(RestartResult)`.

`RestartResult` carries no extra fields beyond `target` — `ok` and `error` already live on `CommandResultMsg`. On systemctl failure, the handler populates `CommandResultMsg::error` with the systemctl exit description / stderr / liveness-check failure reason.

Round-trip serde + JSON schema tests added to the existing `mod tests` block in `messages/commands.rs`.

## MQTT topic & handler wiring

- Command name: `restart`. Topic: `{prefix}/nodes/{nodeId}/cmnd/restart/in` (covered by the existing `cmnd/+/in` subscription).
- New handler module: `src/services/mqtt/handlers/restart.rs`.
  - `pub struct RestartHandler { ctx, runner }`.
  - `RestartHandler::NAME: &str = "restart"`.
  - Implements `CommandHandler`.
- Registration: alongside the other handlers wherever they are wired today (the `CommandProcessor`'s handler map).

## State machine

For every target:

```
receive cmd
  ├─ wrong payload variant → CommandError (mirrors existing handlers)
  ├─ TTL expired             → Expired status, no result
  └─ Accepted (status)
     └─ InProgress (status)
        └─ <per-target action>
           ├─ success → Completed status + Result{ok:true, data: Restart{target}}
           └─ failure → Failed status + Result{ok:false, error: "<details>", data: Restart{target}}
```

Per-target action:

| Target | Action | Success criterion | Failure → `error:` |
|---|---|---|---|
| `camera` | `systemctl restart camera` | exit 0 AND `is-active` reports `active` continuously for 10s (poll every 500ms) | systemctl stderr/exit code, or `"service did not stay active: <last_state>"` |
| `mavlink` | `systemctl restart mavlink` | same | same |
| `modem` | `systemctl restart modem-restart` | same | same |
| `unitctl` | write `<env_dir>/pending-restart-uuid` (uuid + `\n`), flush MQTT, exec `systemctl restart unitctl`; deferred Completed on next boot | next boot: read+delete the file, after MQTT connects publish `Completed{ok:true}` for that uuid | (no Failed path — if restart fails we're not running) |
| `reboot` | publish `Completed{ok:true}` immediately, flush MQTT, exec `reboot` | published before action | n/a |

Notes:
- `systemctl restart` is blocking; we trust its built-in `TimeoutStartSec` and exit code (no additional wall-clock timeout in the handler).
- The 10s post-restart liveness window catches the common failure mode where a `Type=simple` unit starts and crashes immediately.
- "Flush MQTT" = await the QoS-1 publish future on the rumqttc client so the broker has the message before exec.
- For `unitctl`, the state file is written *before* the publish-and-exec — once the new process is running it is the source of truth for whether the restart succeeded.

## Boot-time deferred-completion publisher

A new small `Task`-implementing type, e.g. `RestartCompletionPublisher`, lives in `src/services/mqtt/handlers/restart.rs` as a sibling of `RestartHandler`.

On startup:
1. Open `<env_dir>/pending-restart-uuid`.
   - Missing → no-op, task exits.
   - Present → read uuid, delete the file (single-write atomic file; read-then-delete is fine).
2. Subscribe to `MqttEvent` and wait for the first `Connected`.
3. Publish:
   - `CommandStatus{state: Completed}` to `{prefix}/nodes/{nodeId}/cmnd/restart/status`.
   - `CommandResultMsg{ok: true, data: Restart{target: Unitctl}}` to `.../cmnd/restart/result`.
4. Exit.

Wired into `main.rs` alongside the other MQTT-dependent tasks (only when `mqtt.enabled`).

## Permissions & systemd integration

- unitctl runs as root (existing assumption — DBus, raw network, modem control).
- `systemctl` and `reboot` are invoked directly via `tokio::process::Command`. No sudoers configuration needed.
- `unitctl.service` does **not** need any change for the state-file approach. (Earlier env-var approach via `PassEnvironment` was rejected: `PassEnvironment` reads from the systemd manager environment, not from the env of the process invoking `systemctl`, so `RESTART_COMMAND_UUID=... systemctl restart unitctl` would not work without `systemctl set-environment`.)

## Process abstraction (for testability)

Introduce a small trait local to the restart module:

```rust
#[async_trait]
trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<std::process::Output>;
}
```

Real impl wraps `tokio::process::Command`. Tests use a fake that records invocations and returns scripted exit codes / stdout / stderr. Used uniformly for `systemctl restart`, `systemctl is-active`, and `reboot`.

The polling loop and 10s window are parameterised so tests can use a short window with a mocked or accelerated clock. Default: 10s window, 500ms poll interval.

## Tests

`messages/commands.rs`:
- Round-trip `RestartPayload` and `RestartResult` (each target variant).
- Schema generation includes `RestartTarget`, `RestartPayload`, `RestartResult`.

`services/mqtt/handlers/restart.rs`:
- Wrong payload variant returns `CommandError`.
- `Camera` / `Mavlink` / `Modem` happy path: handler invokes `systemctl restart <unit>` then polls `is-active` for the configured window; returns `Restart` result with `ok:true`.
- `Camera` / `Mavlink` / `Modem` systemctl failure: non-zero exit yields `Failed` with stderr captured in `error`.
- `Camera` / `Mavlink` / `Modem` liveness failure: systemctl exits 0 but `is-active` reports `inactive`/`failed` mid-window → `Failed` with `"service did not stay active: <state>"`.
- `Unitctl` target: verifies state file written with correct uuid; verifies `systemctl restart unitctl` invoked via the runner.
- `Reboot` target: verifies `Completed` is published before `reboot` is invoked (ordering).
- `RestartCompletionPublisher`: file present → publishes Completed + result with the read uuid, deletes the file; file missing → no-op; corrupt/empty file → log + no publish + delete (fail-safe).

## README

After the feature lands, change `* [ ] restart` → `* [x] restart` in `README.md` (under the "mqtt commands processing" list, currently around line 400).

## Open questions

None at this time.

## Files touched (anticipated)

- `src/config.rs` — add `GeneralConfig::env_dir`, validation, tests.
- `config.toml.example` — add `env_dir = "/var/run/unitctl"` under `[general]`.
- `src/messages/commands.rs` — add `RestartTarget`, `RestartPayload`, `RestartResult`; extend `CommandPayload` and `CommandResultData`; tests.
- `src/services/mqtt/handlers/restart.rs` — new module with `RestartHandler`, `RestartCompletionPublisher`, `CommandRunner` trait + real impl; tests.
- `src/services/mqtt/handlers/mod.rs` — re-export.
- `src/services/mqtt/commands.rs` (or wherever handlers are registered) — register `RestartHandler`.
- `src/main.rs` — spawn `RestartCompletionPublisher` when MQTT enabled.
- `README.md` — flip checklist after implementation.
- `assets/schema/` — regenerated by `generate-schema` binary.
