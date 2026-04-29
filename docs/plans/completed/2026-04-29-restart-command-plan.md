# Restart Command Implementation Plan


**Goal:** Add a `restart` MQTT command that restarts a target service (`camera`, `mavlink`, `modem`), restarts unitctl itself with deferred result publication, or reboots the host.

**Architecture:** New `Restart` variant in `CommandPayload`/`CommandResultData`; new `RestartHandler` in `services/mqtt/handlers/restart.rs` that shells out via an injectable `CommandRunner` trait (so tests don't actually call `systemctl`). Synchronous targets (camera/mavlink/modem) verify liveness with a 10s post-restart `is-active` poll. Self-restart of unitctl writes a state file at `<general.env_dir>/pending-restart-uuid`, then execs `systemctl restart unitctl`; on next boot a `RestartCompletionPublisher` task reads+deletes the file, waits for the first MQTT `Connected`, and publishes the deferred Completed status + result. Reboot publishes Completed+result then execs `reboot`.

**Tech Stack:** Rust, tokio, tokio::process, rumqttc (existing), async_trait, schemars, serde, chrono.

**Spec:** `docs/plans/2026-04-29-restart-command-design.md`.

---

## File Structure

- `src/config.rs` — add `GeneralConfig::env_dir`, validate, update `FULL_TEST_CONFIG` and parse tests.
- `config.toml.example` — add `env_dir = "/var/run/unitctl"` under `[general]`.
- `src/messages/commands.rs` — add `RestartTarget`, `RestartPayload`, `RestartResult`; extend `CommandPayload` and `CommandResultData`; round-trip + schema tests.
- `src/services/mqtt/handlers/restart.rs` — new module. Contains:
  - `CommandRunner` trait + `TokioCommandRunner` (real impl).
  - `RestartHandler` (implements `CommandHandler`).
  - `RestartCompletionPublisher` (implements `Task`).
  - Helper functions for the liveness-poll loop.
  - Tests using a `FakeCommandRunner`.
- `src/services/mqtt/handlers/mod.rs` — `pub mod restart;`.
- `src/services/mqtt/commands.rs` — register `RestartHandler` in `register_commands`.
- `src/main.rs` — spawn `RestartCompletionPublisher` when `mqtt.enabled`.
- `README.md` — flip `* [ ] restart` → `* [x] restart` after merge.

---

## Task 1: Add `general.env_dir` config field

**Files:**
- Modify: `src/config.rs:44-48` (struct), `:215-473` (validate), `:480-540` (FULL_TEST_CONFIG), `:546-700` (parse tests), tests around line 480 and the second `test_parse_full_config` toml literal (~`:548-608`).
- Modify: `config.toml.example`

- [x] **Step 1: Add failing test for `env_dir` parsing**

In `src/config.rs` `tests` module, add at the end (alongside other parse assertions in `test_parse_config_from_constant`, after the existing `general.interface` assertion):

```rust
assert_eq!(config.general.env_dir, "/var/run/unitctl");
```

Also add a new test:

```rust
#[test]
fn test_validate_rejects_empty_env_dir() {
    let mut cfg = test_config();
    cfg.general.env_dir = String::new();
    let err = cfg.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("general.env_dir"));
}
```

- [x] **Step 2: Run the parse test, verify it fails**

```bash
cargo test --lib config::tests::test_parse_config_from_constant
cargo test --lib config::tests::test_validate_rejects_empty_env_dir
```

Expected: compile failure (`env_dir` doesn't exist) or assertion failure.

- [x] **Step 3: Add `env_dir` to `GeneralConfig`**

In `src/config.rs` (around line 44):

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct GeneralConfig {
    pub debug: bool,
    pub interface: String,
    pub env_dir: String,
}
```

- [x] **Step 4: Add validation for `env_dir`**

In `Config::validate()`, after the existing `general.interface` checks (around line 386), insert:

```rust
if self.general.env_dir.is_empty() {
    return Err(ConfigError::Validation(
        "general.env_dir must not be empty".to_string(),
    ));
}
if self.general.env_dir.contains('\n') || self.general.env_dir.contains('\r') {
    return Err(ConfigError::Validation(
        "general.env_dir must not contain newline characters".to_string(),
    ));
}
```

- [x] **Step 5: Update both TOML literals in tests**

Add `env_dir = "/var/run/unitctl"` under `[general]` in:
1. `FULL_TEST_CONFIG` constant (around line 482).
2. The inline TOML in `test_parse_full_config` (around line 549) — use a different value such as `/run/unitctl-alt` so the parse test exercises a non-default value, and add `assert_eq!(config.general.env_dir, "/run/unitctl-alt");` to that test.

- [x] **Step 6: Update `config.toml.example`**

Under `[general]`, add the line:

```toml
env_dir = "/var/run/unitctl"
```

- [x] **Step 7: Run all config tests**

```bash
cargo test --lib config::
```

Expected: all pass.

- [x] **Step 8: Commit**

```bash
git add src/config.rs config.toml.example
git commit -m "feat(config): add general.env_dir for runtime state files"
```

---

## Task 2: Add `RestartTarget`, `RestartPayload`, `RestartResult` and extend command enums

**Files:**
- Modify: `src/messages/commands.rs` (struct definitions ~`:97-148`, payload/result section ~`:150-202`, tests `:204-543`).

- [x] **Step 1: Write failing round-trip and schema tests**

Add to the `mod tests` block in `src/messages/commands.rs`:

```rust
#[test]
fn round_trip_restart_payload_each_target() {
    for target in [
        RestartTarget::Camera,
        RestartTarget::Mavlink,
        RestartTarget::Modem,
        RestartTarget::Unitctl,
        RestartTarget::Reboot,
    ] {
        let payload = RestartPayload { target };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: RestartPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.target, target);
    }
}

#[test]
fn restart_target_serializes_snake_case() {
    let json = serde_json::to_value(RestartTarget::Unitctl).unwrap();
    assert_eq!(json, "unitctl");
    let parsed: RestartTarget = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, RestartTarget::Unitctl);
}

#[test]
fn round_trip_command_envelope_restart() {
    let env = CommandEnvelope {
        uuid: "restart-1".to_string(),
        issued_at: sample_ts(),
        ttl_sec: 60,
        payload: CommandPayload::Restart(RestartPayload {
            target: RestartTarget::Camera,
        }),
    };
    let json = serde_json::to_string(&env).unwrap();
    let parsed: CommandEnvelope = serde_json::from_str(&json).unwrap();
    match parsed.payload {
        CommandPayload::Restart(ref p) => assert_eq!(p.target, RestartTarget::Camera),
        _ => panic!("expected Restart payload"),
    }
}

#[test]
fn round_trip_command_result_restart() {
    let result = CommandResultMsg {
        uuid: "rr-1".to_string(),
        ok: true,
        ts: sample_ts(),
        error: None,
        data: Some(CommandResultData::Restart(RestartResult {
            target: RestartTarget::Reboot,
        })),
    };
    let json = serde_json::to_string(&result).unwrap();
    let parsed: CommandResultMsg = serde_json::from_str(&json).unwrap();
    match parsed.data.unwrap() {
        CommandResultData::Restart(r) => assert_eq!(r.target, RestartTarget::Reboot),
        _ => panic!("expected Restart"),
    }
}
```

Also extend the existing `json_schema_generation` test:

```rust
let schema = schemars::schema_for!(RestartPayload);
let json = serde_json::to_string_pretty(&schema).unwrap();
assert!(json.contains("RestartTarget"));
```

- [x] **Step 2: Run tests, verify they fail**

```bash
cargo test --lib messages::commands::
```

Expected: compile errors (types don't exist).

- [x] **Step 3: Add types and extend enums**

In `src/messages/commands.rs`, add after the existing per-command sections (before `mod tests`):

```rust
/// Target unit/operation for a `restart` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestartTarget {
    Camera,
    Mavlink,
    Modem,
    Unitctl,
    Reboot,
}

/// Payload for `restart`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestartPayload {
    pub target: RestartTarget,
}

/// Result for `restart`. `ok` and `error` live on `CommandResultMsg`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestartResult {
    pub target: RestartTarget,
}
```

Extend `CommandPayload`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CommandPayload {
    GetConfig(GetConfigPayload),
    ConfigUpdate(ConfigUpdatePayload),
    ModemCommands(ModemCommandPayload),
    UpdateRequest(UpdateRequestPayload),
    Restart(RestartPayload),
}
```

Extend `CommandResultData`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CommandResultData {
    GetConfig(Box<GetConfigResult>),
    ConfigUpdate(ConfigUpdateResult),
    ModemCommands(ModemCommandResult),
    UpdateRequest(UpdateRequestResult),
    Restart(RestartResult),
}
```

- [x] **Step 4: Run tests, verify they pass**

```bash
cargo test --lib messages::commands::
cargo build
```

Expected: pass + clean build (no missing match arms — existing handlers always pattern-match the variant they expect).

- [x] **Step 5: Regenerate schemas**

```bash
cargo run --bin generate-schema
```

Verify `assets/schema/` was updated; `git diff --stat assets/schema/` should show files changed.

- [x] **Step 6: Commit**

```bash
git add src/messages/commands.rs assets/schema/
git commit -m "feat(messages): add Restart command payload and result"
```

---

## Task 3: Scaffold `restart.rs` module with `CommandRunner` trait

**Files:**
- Create: `src/services/mqtt/handlers/restart.rs`
- Modify: `src/services/mqtt/handlers/mod.rs` (add `pub mod restart;`)

- [x] **Step 1: Write failing test for `TokioCommandRunner` running `/bin/true`**

Create `src/services/mqtt/handlers/restart.rs` with:

```rust
use async_trait::async_trait;
use std::process::Output;
use std::sync::Arc;
use tokio::process::Command;

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output>;
}

pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output> {
        Command::new(program).args(args).output().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tokio_command_runner_executes_true() {
        let runner = TokioCommandRunner;
        let out = runner.run("/bin/true", &[]).await.unwrap();
        assert!(out.status.success());
    }

    #[tokio::test]
    async fn tokio_command_runner_executes_false() {
        let runner = TokioCommandRunner;
        let out = runner.run("/bin/false", &[]).await.unwrap();
        assert!(!out.status.success());
    }
}
```

In `src/services/mqtt/handlers/mod.rs` add:

```rust
pub mod restart;
```

(Suppress the unused warning if necessary by adding `#[allow(dead_code)]` to `Arc` import and any unused items — preferable to re-export later.)

- [x] **Step 2: Run tests, verify pass**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::tokio_command_runner_
```

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs src/services/mqtt/handlers/mod.rs
git commit -m "feat(restart): scaffold CommandRunner trait"
```

---

## Task 4: Add `FakeCommandRunner` test helper

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

- [x] **Step 1: Write the helper inside the `tests` module**

```rust
#[cfg(test)]
use std::os::unix::process::ExitStatusExt;

#[cfg(test)]
#[derive(Debug, Clone)]
struct FakeInvocation {
    pub program: String,
    pub args: Vec<String>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct ScriptedResponse {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[cfg(test)]
struct FakeCommandRunner {
    invocations: tokio::sync::Mutex<Vec<FakeInvocation>>,
    /// Programmed responses keyed by (program, first_arg) — pop-front per match.
    /// Anything unmatched returns exit 0 with empty output.
    responses: tokio::sync::Mutex<std::collections::VecDeque<(String, ScriptedResponse)>>,
}

#[cfg(test)]
impl FakeCommandRunner {
    fn new() -> Self {
        Self {
            invocations: tokio::sync::Mutex::new(Vec::new()),
            responses: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Push a response to the queue; consumed in order regardless of program name.
    /// (Keeps the helper minimal — tests can sequence multiple commands.)
    async fn push_response(&self, exit_code: i32, stdout: &str, stderr: &str) {
        self.responses.lock().await.push_back((
            String::new(),
            ScriptedResponse {
                exit_code,
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            },
        ));
    }

    async fn invocations(&self) -> Vec<FakeInvocation> {
        self.invocations.lock().await.clone()
    }
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.invocations.lock().await.push(FakeInvocation {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
        let response = self
            .responses
            .lock()
            .await
            .pop_front()
            .map(|(_, r)| r)
            .unwrap_or(ScriptedResponse {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(response.exit_code << 8),
            stdout: response.stdout,
            stderr: response.stderr,
        })
    }
}

#[tokio::test]
async fn fake_runner_records_and_replays() {
    let fake = FakeCommandRunner::new();
    fake.push_response(0, "active\n", "").await;
    fake.push_response(3, "", "Unit foo.service not loaded.\n").await;

    let r1 = fake.run("systemctl", &["is-active", "foo"]).await.unwrap();
    assert!(r1.status.success());
    assert_eq!(r1.stdout, b"active\n");

    let r2 = fake.run("systemctl", &["restart", "foo"]).await.unwrap();
    assert_eq!(r2.status.code(), Some(3));

    let invs = fake.invocations().await;
    assert_eq!(invs.len(), 2);
    assert_eq!(invs[0].program, "systemctl");
    assert_eq!(invs[0].args, vec!["is-active", "foo"]);
}
```

- [x] **Step 2: Run tests**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::fake_runner_
```

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs
git commit -m "test(restart): add FakeCommandRunner helper"
```

---

## Task 5: Implement liveness-verification helper

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

This implements the 10s post-restart `is-active` poll. The interval and window are parameters so tests can use short windows.

- [x] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn verify_active_returns_ok_when_continuously_active() {
    let fake = FakeCommandRunner::new();
    // 4 polls, all "active"
    for _ in 0..4 {
        fake.push_response(0, "active\n", "").await;
    }
    let result = verify_active(
        &fake,
        "camera",
        std::time::Duration::from_millis(40),
        std::time::Duration::from_millis(10),
    )
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn verify_active_returns_err_when_state_changes() {
    let fake = FakeCommandRunner::new();
    fake.push_response(0, "active\n", "").await;
    fake.push_response(3, "failed\n", "").await; // is-active returns non-zero for non-active
    let result = verify_active(
        &fake,
        "camera",
        std::time::Duration::from_millis(40),
        std::time::Duration::from_millis(10),
    )
    .await;
    let err = result.unwrap_err();
    assert!(err.contains("failed"));
}
```

- [x] **Step 2: Run tests, verify failure**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::verify_active_
```

Expected: compile failure (no `verify_active` yet).

- [x] **Step 3: Implement `verify_active`**

Add to `restart.rs` (outside `tests`):

```rust
/// Poll `systemctl is-active <unit>` every `interval` for `window` total time.
/// Succeeds only if every poll reports `active`. Returns `Err(state)` on the first
/// non-active reading, where `state` is the trimmed stdout (e.g. "inactive",
/// "failed", "activating") or `"unknown"` if stdout was empty.
async fn verify_active<R: CommandRunner + ?Sized>(
    runner: &R,
    unit: &str,
    window: std::time::Duration,
    interval: std::time::Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let out = runner
            .run("systemctl", &["is-active", unit])
            .await
            .map_err(|e| format!("failed to invoke systemctl is-active: {e}"))?;
        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || state != "active" {
            let reported = if state.is_empty() {
                "unknown".to_string()
            } else {
                state
            };
            return Err(reported);
        }
        if tokio::time::Instant::now() + interval >= deadline {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}
```

- [x] **Step 4: Run tests, verify pass**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::verify_active_
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs
git commit -m "feat(restart): add liveness-verification helper"
```

---

## Task 6: Implement `RestartHandler` for camera/mavlink/modem targets

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

Constants and the synchronous-target portion of the handler. `unitctl` and `reboot` come in tasks 7 and 8.

- [x] **Step 1: Write failing tests for the synchronous path**

```rust
#[tokio::test]
async fn handler_camera_happy_path() {
    let fake = Arc::new(FakeCommandRunner::new());
    // restart returns 0
    fake.push_response(0, "", "").await;
    // 1 is-active poll returns "active"
    fake.push_response(0, "active\n", "").await;

    let handler = RestartHandler::new_with_runner(
        fake.clone(),
        std::path::PathBuf::from("/tmp/unitctl-test"),
        std::time::Duration::from_millis(5),  // verification window
        std::time::Duration::from_millis(5),  // poll interval
    );
    let env = make_envelope(RestartTarget::Camera);
    let res = handler.handle(&env).await.unwrap();
    match res.data {
        crate::messages::commands::CommandResultData::Restart(r) => {
            assert_eq!(r.target, RestartTarget::Camera);
        }
        _ => panic!("expected Restart"),
    }
    let invs = fake.invocations().await;
    assert_eq!(invs[0].program, "systemctl");
    assert_eq!(invs[0].args, vec!["restart", "camera"]);
    assert_eq!(invs[1].program, "systemctl");
    assert_eq!(invs[1].args, vec!["is-active", "camera"]);
}

#[tokio::test]
async fn handler_systemctl_restart_failure() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(1, "", "Failed to restart camera.service: Unit not found\n").await;

    let handler = RestartHandler::new_with_runner(
        fake,
        std::path::PathBuf::from("/tmp/unitctl-test"),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = make_envelope(RestartTarget::Camera);
    let err = handler.handle(&env).await.unwrap_err();
    assert!(err.message.contains("Unit not found") || err.message.contains("exit"));
}

#[tokio::test]
async fn handler_liveness_failure() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(0, "", "").await;          // restart ok
    fake.push_response(0, "active\n", "").await;  // poll 1 ok
    fake.push_response(3, "failed\n", "").await;  // poll 2 not active

    let handler = RestartHandler::new_with_runner(
        fake,
        std::path::PathBuf::from("/tmp/unitctl-test"),
        std::time::Duration::from_millis(40),
        std::time::Duration::from_millis(10),
    );
    let env = make_envelope(RestartTarget::Mavlink);
    let err = handler.handle(&env).await.unwrap_err();
    assert!(err.message.contains("did not stay active"));
    assert!(err.message.contains("failed"));
}

#[tokio::test]
async fn handler_modem_uses_modem_restart_unit() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(0, "", "").await;
    fake.push_response(0, "active\n", "").await;

    let handler = RestartHandler::new_with_runner(
        fake.clone(),
        std::path::PathBuf::from("/tmp/unitctl-test"),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = make_envelope(RestartTarget::Modem);
    handler.handle(&env).await.unwrap();
    let invs = fake.invocations().await;
    assert_eq!(invs[0].args, vec!["restart", "modem-restart"]);
}

#[tokio::test]
async fn handler_wrong_payload_variant() {
    let fake = Arc::new(FakeCommandRunner::new());
    let handler = RestartHandler::new_with_runner(
        fake,
        std::path::PathBuf::from("/tmp/unitctl-test"),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = CommandEnvelope {
        uuid: "x".to_string(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 60,
        payload: CommandPayload::GetConfig(GetConfigPayload {}),
    };
    let err = handler.handle(&env).await.unwrap_err();
    assert!(err.message.to_lowercase().contains("restart"));
}

#[cfg(test)]
fn make_envelope(target: RestartTarget) -> CommandEnvelope {
    CommandEnvelope {
        uuid: format!("restart-{:?}", target).to_lowercase(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 300,
        payload: CommandPayload::Restart(RestartPayload { target }),
    }
}
```

Add the necessary imports to the test module:

```rust
use crate::messages::commands::{
    CommandEnvelope, CommandPayload, GetConfigPayload, RestartPayload, RestartTarget,
};
use crate::services::mqtt::commands::CommandHandler;
```

- [x] **Step 2: Run tests, verify failure**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::handler_
```

Expected: compile failure.

- [x] **Step 3: Implement `RestartHandler`**

Add to `restart.rs`:

```rust
use std::path::PathBuf;
use std::time::Duration;

use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, RestartPayload, RestartResult,
    RestartTarget,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};

/// Default post-restart liveness verification window.
const DEFAULT_VERIFY_WINDOW: Duration = Duration::from_secs(10);
/// Default poll interval inside the verification window.
const DEFAULT_VERIFY_INTERVAL: Duration = Duration::from_millis(500);
/// File written before self-restart so the next-boot publisher can ack the command.
const PENDING_FILE_NAME: &str = "pending-restart-uuid";

pub struct RestartHandler {
    runner: Arc<dyn CommandRunner>,
    env_dir: PathBuf,
    verify_window: Duration,
    verify_interval: Duration,
}

impl RestartHandler {
    pub const NAME: &str = "restart";

    pub fn new(runner: Arc<dyn CommandRunner>, env_dir: PathBuf) -> Self {
        Self {
            runner,
            env_dir,
            verify_window: DEFAULT_VERIFY_WINDOW,
            verify_interval: DEFAULT_VERIFY_INTERVAL,
        }
    }

    #[cfg(test)]
    fn new_with_runner(
        runner: Arc<dyn CommandRunner>,
        env_dir: PathBuf,
        verify_window: Duration,
        verify_interval: Duration,
    ) -> Self {
        Self {
            runner,
            env_dir,
            verify_window,
            verify_interval,
        }
    }

    fn unit_for(target: RestartTarget) -> Option<&'static str> {
        match target {
            RestartTarget::Camera => Some("camera"),
            RestartTarget::Mavlink => Some("mavlink"),
            RestartTarget::Modem => Some("modem-restart"),
            RestartTarget::Unitctl | RestartTarget::Reboot => None,
        }
    }

    async fn restart_unit(&self, target: RestartTarget, unit: &str) -> Result<CommandResult, CommandError> {
        let restart_out = self
            .runner
            .run("systemctl", &["restart", unit])
            .await
            .map_err(|e| CommandError::new(format!("failed to invoke systemctl: {e}")))?;
        if !restart_out.status.success() {
            let stderr = String::from_utf8_lossy(&restart_out.stderr).trim().to_string();
            return Err(CommandError::new(format!(
                "systemctl restart {unit} exited with {:?}: {stderr}",
                restart_out.status.code()
            )));
        }
        verify_active(self.runner.as_ref(), unit, self.verify_window, self.verify_interval)
            .await
            .map_err(|state| {
                CommandError::new(format!("service did not stay active: {state}"))
            })?;
        Ok(CommandResult {
            data: CommandResultData::Restart(RestartResult { target }),
        })
    }
}

#[async_trait]
impl CommandHandler for RestartHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        let payload = match &envelope.payload {
            CommandPayload::Restart(p) => p,
            _ => return Err(CommandError::new("expected Restart payload")),
        };
        match payload.target {
            t @ (RestartTarget::Camera | RestartTarget::Mavlink | RestartTarget::Modem) => {
                let unit = Self::unit_for(t).expect("synchronous targets have units");
                self.restart_unit(t, unit).await
            }
            RestartTarget::Unitctl => Err(CommandError::new("unitctl target not yet implemented")),
            RestartTarget::Reboot => Err(CommandError::new("reboot target not yet implemented")),
        }
    }
}
```

(The `Unitctl`/`Reboot` arms are placeholders so tests for sync targets compile and pass — they will be implemented in tasks 7 and 8.)

- [x] **Step 4: Run tests, verify pass**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::handler_
cargo clippy -- -D warnings
```

Expected: PASS, no warnings.

- [x] **Step 5: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs
git commit -m "feat(restart): handle camera/mavlink/modem targets"
```

---

## Task 7: Implement `Unitctl` self-restart path

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

Self-restart writes the uuid to `<env_dir>/pending-restart-uuid` and execs `systemctl restart unitctl`. The handler's future never resolves (the process is killed by systemd mid-execution); on next boot the deferred publisher (Task 9) will ack the command. We don't return Ok from the handler in production because returning Ok would let the processor publish a Completed status for *this* dying process — but the handler must publish nothing itself. The simplest sound implementation: write the file, await `systemctl restart unitctl`, then sleep forever (the process will be killed before the sleep wakes). If `systemctl restart` fails synchronously (e.g. systemd unreachable, permission denied), return an error so the operator gets a Failed result on this boot.

- [x] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn handler_unitctl_writes_state_file_and_execs_restart() {
    let fake = Arc::new(FakeCommandRunner::new());
    // systemctl restart unitctl — return 0 but block so the handler never returns
    // (in real life systemd kills the process; the FakeCommandRunner returns
    // immediately, after which the handler should sleep forever)
    fake.push_response(0, "", "").await;

    let tmp = tempfile::tempdir().unwrap();
    let handler = RestartHandler::new_with_runner(
        fake.clone(),
        tmp.path().to_path_buf(),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = CommandEnvelope {
        uuid: "uuid-self-restart".to_string(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 60,
        payload: CommandPayload::Restart(RestartPayload {
            target: RestartTarget::Unitctl,
        }),
    };

    // Race: the handler future never resolves in the success path. Drive it for
    // long enough to exec systemctl, then time out and assert side effects.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handler.handle(&env),
    )
    .await;

    let written = std::fs::read_to_string(tmp.path().join(PENDING_FILE_NAME)).unwrap();
    assert_eq!(written.trim(), "uuid-self-restart");

    let invs = fake.invocations().await;
    assert!(invs.iter().any(|i| i.args == vec!["restart", "unitctl"]));
}

#[tokio::test]
async fn handler_unitctl_creates_env_dir_if_missing() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(0, "", "").await;

    let tmp = tempfile::tempdir().unwrap();
    let env_dir = tmp.path().join("nested/dir");
    let handler = RestartHandler::new_with_runner(
        fake,
        env_dir.clone(),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = CommandEnvelope {
        uuid: "u2".to_string(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 60,
        payload: CommandPayload::Restart(RestartPayload {
            target: RestartTarget::Unitctl,
        }),
    };
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handler.handle(&env),
    )
    .await;
    assert!(env_dir.join(PENDING_FILE_NAME).exists());
}

#[tokio::test]
async fn handler_unitctl_returns_error_on_systemctl_failure() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(1, "", "Access denied\n").await;

    let tmp = tempfile::tempdir().unwrap();
    let handler = RestartHandler::new_with_runner(
        fake,
        tmp.path().to_path_buf(),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
    );
    let env = CommandEnvelope {
        uuid: "u-fail".to_string(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 60,
        payload: CommandPayload::Restart(RestartPayload {
            target: RestartTarget::Unitctl,
        }),
    };
    let res = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handler.handle(&env),
    )
    .await
    .expect("handler should return on failure");
    let err = res.unwrap_err();
    assert!(err.message.contains("Access denied") || err.message.contains("exit"));
}
```

Add `tempfile = "3"` to `[dev-dependencies]` in `Cargo.toml` if not already present.

- [x] **Step 2: Run tests, verify failure**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::handler_unitctl_
```

Expected: tests fail (`unitctl target not yet implemented`).

- [x] **Step 3: Implement the unitctl branch**

In `restart.rs`, replace the `RestartTarget::Unitctl` arm in `CommandHandler::handle`:

```rust
RestartTarget::Unitctl => self.exec_self_restart(&envelope.uuid).await,
```

Add the method on `RestartHandler`:

```rust
async fn exec_self_restart(&self, uuid: &str) -> Result<CommandResult, CommandError> {
    // Ensure env_dir exists.
    tokio::fs::create_dir_all(&self.env_dir)
        .await
        .map_err(|e| CommandError::new(format!("failed to create env_dir: {e}")))?;
    let pending = self.env_dir.join(PENDING_FILE_NAME);
    let mut contents = uuid.to_string();
    contents.push('\n');
    tokio::fs::write(&pending, contents.as_bytes())
        .await
        .map_err(|e| CommandError::new(format!("failed to write {pending:?}: {e}")))?;

    let out = self
        .runner
        .run("systemctl", &["restart", "unitctl"])
        .await
        .map_err(|e| CommandError::new(format!("failed to invoke systemctl: {e}")))?;
    if !out.status.success() {
        // Best-effort cleanup — the restart didn't happen, no point leaving the file.
        let _ = tokio::fs::remove_file(&pending).await;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(CommandError::new(format!(
            "systemctl restart unitctl exited with {:?}: {stderr}",
            out.status.code()
        )));
    }
    // systemctl returned success — systemd is now restarting us. Wait to be killed
    // rather than letting the processor publish a stale Completed status.
    std::future::pending::<()>().await;
    unreachable!()
}
```

- [x] **Step 4: Run tests, verify pass**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::handler_unitctl_
cargo clippy -- -D warnings
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs Cargo.toml Cargo.lock
git commit -m "feat(restart): unitctl self-restart writes pending uuid file"
```

---

## Task 8: Implement `Reboot` target

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

Reboot must publish Completed before the host reboots. The `CommandProcessor` already publishes Completed *after* the handler returns — so for `Reboot`, the handler returns Ok normally, but BEFORE returning we kick off a delayed `reboot` exec on a detached task. The delay gives rumqttc's event loop time to flush the Completed publish.

- [x] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn handler_reboot_returns_ok_and_invokes_reboot_after_delay() {
    let fake = Arc::new(FakeCommandRunner::new());
    fake.push_response(0, "", "").await;

    let tmp = tempfile::tempdir().unwrap();
    let handler = RestartHandler::new_with_runner_and_reboot_delay(
        fake.clone(),
        tmp.path().to_path_buf(),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(20),
    );
    let env = CommandEnvelope {
        uuid: "reb-1".to_string(),
        issued_at: chrono::Utc::now(),
        ttl_sec: 60,
        payload: CommandPayload::Restart(RestartPayload {
            target: RestartTarget::Reboot,
        }),
    };
    // Handler returns immediately
    let result = handler.handle(&env).await.unwrap();
    match result.data {
        crate::messages::commands::CommandResultData::Restart(r) => {
            assert_eq!(r.target, RestartTarget::Reboot);
        }
        _ => panic!("expected Restart"),
    }
    // reboot has not yet been called (delay is 20ms)
    assert!(fake.invocations().await.is_empty());
    // Wait for the spawned reboot
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let invs = fake.invocations().await;
    assert_eq!(invs.len(), 1);
    assert_eq!(invs[0].program, "reboot");
}
```

- [x] **Step 2: Run, verify failure**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::handler_reboot_
```

Expected: compile failure (no `new_with_runner_and_reboot_delay`).

- [x] **Step 3: Implement the reboot branch**

Add a `reboot_delay: Duration` field to `RestartHandler` (default 1 second). Update both constructors:

```rust
pub struct RestartHandler {
    runner: Arc<dyn CommandRunner>,
    env_dir: PathBuf,
    verify_window: Duration,
    verify_interval: Duration,
    reboot_delay: Duration,
}

impl RestartHandler {
    // ...
    pub fn new(runner: Arc<dyn CommandRunner>, env_dir: PathBuf) -> Self {
        Self {
            runner,
            env_dir,
            verify_window: DEFAULT_VERIFY_WINDOW,
            verify_interval: DEFAULT_VERIFY_INTERVAL,
            reboot_delay: Duration::from_secs(1),
        }
    }

    #[cfg(test)]
    fn new_with_runner(/* existing args */) -> Self {
        // ... set reboot_delay: Duration::from_millis(0)
    }

    #[cfg(test)]
    fn new_with_runner_and_reboot_delay(
        runner: Arc<dyn CommandRunner>,
        env_dir: PathBuf,
        verify_window: Duration,
        verify_interval: Duration,
        reboot_delay: Duration,
    ) -> Self {
        Self { runner, env_dir, verify_window, verify_interval, reboot_delay }
    }
}
```

Replace the `RestartTarget::Reboot` arm:

```rust
RestartTarget::Reboot => self.schedule_reboot().await,
```

Add the method:

```rust
async fn schedule_reboot(&self) -> Result<CommandResult, CommandError> {
    let runner = Arc::clone(&self.runner);
    let delay = self.reboot_delay;
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        match runner.run("reboot", &[]).await {
            Ok(out) if !out.status.success() => {
                tracing::error!(
                    code = ?out.status.code(),
                    "reboot exited non-zero"
                );
            }
            Err(e) => tracing::error!(error = %e, "failed to invoke reboot"),
            _ => {}
        }
    });
    Ok(CommandResult {
        data: CommandResultData::Restart(RestartResult {
            target: RestartTarget::Reboot,
        }),
    })
}
```

- [x] **Step 4: Run tests**

```bash
cargo test --lib services::mqtt::handlers::restart
cargo clippy -- -D warnings
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs
git commit -m "feat(restart): reboot target with delayed reboot exec"
```

---

## Task 9: Implement `RestartCompletionPublisher` task

**Files:**
- Modify: `src/services/mqtt/handlers/restart.rs`

Reads `<env_dir>/pending-restart-uuid` on startup, deletes it, waits for the first `MqttEvent::Connected`, publishes `CommandStatus{Completed}` to `cmnd/restart/status` and `CommandResultMsg{ok:true, data: Restart{Unitctl}}` to `cmnd/restart/result`.

- [x] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn completion_publisher_no_op_when_file_missing() {
    let tmp = tempfile::tempdir().unwrap();
    // No file written. read_pending should return None.
    let read = read_and_consume_pending_file(tmp.path()).await.unwrap();
    assert!(read.is_none());
}

#[tokio::test]
async fn completion_publisher_reads_and_deletes_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PENDING_FILE_NAME);
    std::fs::write(&path, "uuid-xyz\n").unwrap();
    let read = read_and_consume_pending_file(tmp.path()).await.unwrap();
    assert_eq!(read.as_deref(), Some("uuid-xyz"));
    assert!(!path.exists(), "file should be deleted after read");
}

#[tokio::test]
async fn completion_publisher_handles_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PENDING_FILE_NAME);
    std::fs::write(&path, "").unwrap();
    let read = read_and_consume_pending_file(tmp.path()).await.unwrap();
    assert_eq!(read, Some(String::new()));
    assert!(!path.exists());
}
```

(End-to-end MQTT publish behaviour is verified manually / via integration test outside this plan — the unit-testable seam is the file read/delete + the published topic strings.)

- [x] **Step 2: Run, verify failure**

```bash
cargo test --lib services::mqtt::handlers::restart::tests::completion_publisher_
```

- [x] **Step 3: Implement `read_and_consume_pending_file` and the task struct**

Add to `restart.rs`:

```rust
use crate::lib_or_main_task_trait::*; // see below — match existing Task import
use crate::messages::commands::{CommandResultMsg, CommandState, CommandStatus};
use crate::services::mqtt::transport::{MqttEvent, MqttTransport};
use chrono::Utc;
use rumqttc::QoS;
use tokio_util::sync::CancellationToken;

/// Read+delete the pending-restart-uuid file. Returns `Ok(None)` if the file does
/// not exist. Trims a trailing newline if present.
pub(crate) async fn read_and_consume_pending_file(
    env_dir: &std::path::Path,
) -> std::io::Result<Option<String>> {
    let path = env_dir.join(PENDING_FILE_NAME);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // Best-effort delete; keep going even if delete fails so we still publish.
    let _ = tokio::fs::remove_file(&path).await;
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    Ok(Some(s))
}

pub struct RestartCompletionPublisher {
    transport: Arc<MqttTransport>,
    env_dir: PathBuf,
    cancel: CancellationToken,
}

impl RestartCompletionPublisher {
    pub fn new(
        transport: Arc<MqttTransport>,
        env_dir: PathBuf,
        cancel: CancellationToken,
    ) -> Self {
        Self { transport, env_dir, cancel }
    }
}
```

Implement the `Task` trait — match the trait import path used by other publishers (`StatusPublisher`, `TelemetryPublisher`). Look at `src/services/mqtt/status.rs` for the exact import and pattern, then mirror it.

The `run()` method:
1. Read+consume the file. If `None` or the uuid is empty, log and exit.
2. Subscribe to `transport.subscribe_events()`.
3. Wait for the first `MqttEvent::Connected` (or cancellation).
4. Publish `CommandStatus{state: Completed}` to `transport.command_topic("restart", "status")` (QoS 1, not retained).
5. Publish `CommandResultMsg{ok: true, error: None, data: Some(Restart{Unitctl})}` to `transport.command_topic("restart", "result")` (QoS 1, not retained).
6. Exit.

Pseudocode (concrete shape — mirror `StatusPublisher`'s structure):

```rust
fn run(self: Arc<Self>) -> Vec<JoinHandle<()>> {
    let me = Arc::clone(&self);
    vec![tokio::spawn(async move {
        let uuid = match read_and_consume_pending_file(&me.env_dir).await {
            Ok(Some(s)) if !s.is_empty() => s,
            Ok(_) => {
                tracing::debug!("no pending restart uuid; nothing to publish");
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read pending restart uuid");
                return;
            }
        };

        let mut events = me.transport.subscribe_events();
        loop {
            tokio::select! {
                _ = me.cancel.cancelled() => return,
                evt = events.recv() => {
                    match evt {
                        Ok(MqttEvent::Connected) => break,
                        Ok(_) => continue,
                        Err(_) => return,
                    }
                }
            }
        }

        let status = CommandStatus {
            uuid: uuid.clone(),
            state: CommandState::Completed,
            ts: Utc::now(),
        };
        let status_topic = me.transport.command_topic("restart", "status");
        if let Ok(json) = serde_json::to_string(&status) {
            let _ = me
                .transport
                .publish(&status_topic, json.as_bytes(), QoS::AtLeastOnce, false)
                .await;
        }

        let result = CommandResultMsg {
            uuid,
            ok: true,
            ts: Utc::now(),
            error: None,
            data: Some(CommandResultData::Restart(RestartResult {
                target: RestartTarget::Unitctl,
            })),
        };
        let result_topic = me.transport.command_topic("restart", "result");
        if let Ok(json) = serde_json::to_string(&result) {
            let _ = me
                .transport
                .publish(&result_topic, json.as_bytes(), QoS::AtLeastOnce, false)
                .await;
        }
        tracing::info!("published deferred restart completion");
    })]
}
```

(Resolve the `Task` trait import by reading `src/services/mqtt/status.rs` lines around the `impl Task for StatusPublisher` and copying the import.)

- [x] **Step 4: Run tests**

```bash
cargo test --lib services::mqtt::handlers::restart
cargo clippy -- -D warnings
cargo build
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/services/mqtt/handlers/restart.rs
git commit -m "feat(restart): deferred-completion publisher for unitctl restart"
```

---

## Task 10: Register `RestartHandler` and spawn `RestartCompletionPublisher`

**Files:**
- Modify: `src/services/mqtt/commands.rs:118-129` (`register_commands`)
- Modify: `src/main.rs:138-184` (MQTT block)

- [x] **Step 1: Add registration**

In `src/services/mqtt/commands.rs`, in `register_commands` add at the end:

```rust
self.register(
    crate::services::mqtt::handlers::restart::RestartHandler::NAME,
    crate::services::mqtt::handlers::restart::RestartHandler::new(
        std::sync::Arc::new(crate::services::mqtt::handlers::restart::TokioCommandRunner),
        std::path::PathBuf::from(&ctx.config.general.env_dir),
    ),
);
```

- [x] **Step 2: Spawn the publisher in `main.rs`**

In `src/main.rs` inside the `if ctx.config.mqtt.enabled` block, after the existing publishers (around line 179), add:

```rust
let restart_completion = Arc::new(
    crate::services::mqtt::handlers::restart::RestartCompletionPublisher::new(
        Arc::clone(&transport),
        std::path::PathBuf::from(&ctx.config.general.env_dir),
        cancel.clone(),
    ),
);
handles.extend(restart_completion.run());
```

(Adjust import path to match how other handler types are imported in `main.rs`.)

- [x] **Step 3: Build and run existing tests**

```bash
cargo build
cargo test
cargo clippy -- -D warnings
```

Expected: clean.

- [x] **Step 4: Commit**

```bash
git add src/services/mqtt/commands.rs src/main.rs
git commit -m "feat(restart): register handler and spawn completion publisher"
```

---

## Task 11: README checklist update

**Files:**
- Modify: `README.md` (around line 400)

- [x] **Step 1: Flip the checkbox**

Change:

```
* [ ] restart
```

to

```
* [x] restart
```

- [x] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: mark restart command as completed in README"
```

---

## Final Verification

- [x] `cargo test` passes (431 lib + 2 bin + 5 main unit tests pass; mqtt_integration tests require Docker mosquitto bind mounts unavailable in this sandbox — env-only failure, unrelated to restart code)
- [x] `cargo clippy -- -D warnings` clean
- [x] `cargo fmt --check` clean
- [x] `cargo run --bin generate-schema` produces no diff after committing `assets/schema/`
- [x] Manual integration test (skipped - not automatable; requires running MQTT broker and systemd-managed unitctl)
