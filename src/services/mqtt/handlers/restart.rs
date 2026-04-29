use async_trait::async_trait;
use chrono::Utc;
use rumqttc::QoS;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, CommandResultMsg, CommandState,
    CommandStatus, RestartResult, RestartTarget,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};
use crate::services::mqtt::transport::{MqttEvent, MqttTransport};
use crate::Task;

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

const DEFAULT_VERIFY_WINDOW: Duration = Duration::from_secs(10);
const DEFAULT_VERIFY_INTERVAL: Duration = Duration::from_millis(500);
const PENDING_FILE_NAME: &str = "pending-restart-uuid";
const PENDING_INFLIGHT_PREFIX: &str = "pending-restart-uuid.in-flight.";

pub struct RestartHandler {
    runner: Arc<dyn CommandRunner>,
    env_dir: PathBuf,
    verify_window: Duration,
    verify_interval: Duration,
    reboot_delay: Duration,
}

impl RestartHandler {
    pub const NAME: &'static str = "restart";

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
            reboot_delay: Duration::from_millis(0),
        }
    }

    #[cfg(test)]
    fn new_with_runner_and_reboot_delay(
        runner: Arc<dyn CommandRunner>,
        env_dir: PathBuf,
        verify_window: Duration,
        verify_interval: Duration,
        reboot_delay: Duration,
    ) -> Self {
        Self {
            runner,
            env_dir,
            verify_window,
            verify_interval,
            reboot_delay,
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

    async fn restart_unit(
        &self,
        target: RestartTarget,
        unit: &str,
    ) -> Result<CommandResult, CommandError> {
        let restart_out = self
            .runner
            .run("systemctl", &["restart", unit])
            .await
            .map_err(|e| CommandError::new(format!("failed to invoke systemctl: {e}")))?;
        if !restart_out.status.success() {
            let stderr = String::from_utf8_lossy(&restart_out.stderr)
                .trim()
                .to_string();
            return Err(CommandError::new(format!(
                "systemctl restart {unit} exited with {:?}: {stderr}",
                restart_out.status.code()
            )));
        }
        verify_active(
            self.runner.as_ref(),
            unit,
            self.verify_window,
            self.verify_interval,
        )
        .await
        .map_err(|state| CommandError::new(format!("service did not stay active: {state}")))?;
        Ok(CommandResult {
            data: CommandResultData::Restart(RestartResult { target }),
        })
    }

    async fn exec_self_restart(&self, uuid: &str) -> Result<CommandResult, CommandError> {
        tokio::fs::create_dir_all(&self.env_dir)
            .await
            .map_err(|e| CommandError::new(format!("failed to create env_dir: {e}")))?;
        let pending = self.env_dir.join(PENDING_FILE_NAME);
        let mut contents = uuid.to_string();
        contents.push('\n');
        tokio::fs::write(&pending, contents.as_bytes())
            .await
            .map_err(|e| CommandError::new(format!("failed to write {pending:?}: {e}")))?;
        debug!(uuid = %uuid, "wrote pending restart uuid");

        let out = match self.runner.run("systemctl", &["restart", "unitctl"]).await {
            Ok(o) => o,
            Err(e) => {
                delete_pending_file(&self.env_dir).await;
                return Err(CommandError::new(format!(
                    "failed to invoke systemctl: {e}"
                )));
            }
        };
        if !out.status.success() && out.status.code().is_some() {
            delete_pending_file(&self.env_dir).await;
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(CommandError::new(format!(
                "systemctl restart unitctl exited with {:?}: {stderr}",
                out.status.code()
            )));
        }
        std::future::pending::<()>().await;
        unreachable!()
    }

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
            RestartTarget::Unitctl => self.exec_self_restart(&envelope.uuid).await,
            RestartTarget::Reboot => self.schedule_reboot().await,
        }
    }
}

/// Read the pending-restart-uuid file without deleting it.
///
/// Returns `Ok(None)` if the file does not exist.
#[cfg(test)]
pub(crate) async fn read_pending_file(env_dir: &Path) -> std::io::Result<Option<String>> {
    let path = env_dir.join(PENDING_FILE_NAME);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    Ok(Some(s))
}

pub(crate) async fn delete_pending_file(env_dir: &Path) {
    let path = env_dir.join(PENDING_FILE_NAME);
    remove_path_logging(&path).await;
}

async fn remove_path_logging(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(error = %e, path = ?path, "failed to delete pending restart uuid file");
        }
    }
}

fn unique_inflight_path(env_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    env_dir.join(format!(
        "{}{}.{}",
        PENDING_INFLIGHT_PREFIX,
        std::process::id(),
        nanos
    ))
}

/// Atomically claim the main pending-restart file by renaming it to a unique
/// scratch path. Once renamed, the scratch path is owned exclusively by this
/// publisher run — handlers only ever write to the main path, so subsequent
/// read and delete operations on the scratch path are race-free.
pub(crate) async fn claim_main_pending(
    env_dir: &Path,
) -> std::io::Result<Option<(PathBuf, String)>> {
    let main = env_dir.join(PENDING_FILE_NAME);
    let scratch = unique_inflight_path(env_dir);
    match tokio::fs::rename(&main, &scratch).await {
        Ok(()) => {
            let bytes = tokio::fs::read(&scratch).await?;
            let uuid = String::from_utf8_lossy(&bytes).trim().to_string();
            Ok(Some((scratch, uuid)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Collect orphan in-flight scratch files left by a prior boot whose publish
/// did not complete. The caller is responsible for deleting each entry by its
/// exact path after a successful publish.
pub(crate) async fn collect_orphan_inflight(
    env_dir: &Path,
) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    let mut dir = match tokio::fs::read_dir(env_dir).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.starts_with(PENDING_INFLIGHT_PREFIX) {
            continue;
        }
        let path = entry.path();
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        out.push((path, String::from_utf8_lossy(&bytes).trim().to_string()));
    }
    Ok(out)
}

/// Publishes a deferred Completed status + result for the most recent
/// `restart{target=unitctl}` command after the next-boot MQTT connection.
pub struct RestartCompletionPublisher {
    transport: Arc<MqttTransport>,
    env_dir: PathBuf,
    cancel: CancellationToken,
    event_rx: Mutex<Option<tokio::sync::broadcast::Receiver<MqttEvent>>>,
}

impl RestartCompletionPublisher {
    pub fn new(transport: Arc<MqttTransport>, env_dir: PathBuf, cancel: CancellationToken) -> Self {
        let event_rx = transport.subscribe_events();
        Self {
            transport,
            env_dir,
            cancel,
            event_rx: Mutex::new(Some(event_rx)),
        }
    }
}

impl Task for RestartCompletionPublisher {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let mut event_rx = self
            .event_rx
            .lock()
            .expect("event_rx mutex poisoned")
            .take()
            .expect("event_rx already taken — run() must only be called once");

        vec![tokio::spawn(async move {
            let mut entries = collect_orphan_inflight(&self.env_dir)
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "failed to scan for orphan in-flight restart files");
                    Vec::new()
                });
            match claim_main_pending(&self.env_dir).await {
                Ok(Some(claimed)) => entries.push(claimed),
                Ok(None) => {}
                Err(e) => {
                    warn!(error = %e, "failed to claim pending restart uuid");
                }
            }
            debug!(
                count = entries.len(),
                "collected pending restart uuid entries"
            );

            entries.retain(|(path, uuid)| {
                if uuid.is_empty() {
                    warn!(path = ?path, "pending restart uuid file is empty; discarding");
                    let path = path.clone();
                    tokio::spawn(async move { remove_path_logging(&path).await });
                    false
                } else {
                    true
                }
            });

            if entries.is_empty() {
                debug!("no pending restart uuid; nothing to publish");
                return;
            }

            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        debug!("restart completion publisher cancelled before connect");
                        return;
                    }
                    event = event_rx.recv() => {
                        match event {
                            Ok(MqttEvent::Connected) => break,
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                debug!("MQTT event channel closed before connect");
                                return;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                continue;
                            }
                        }
                    }
                }
            }

            for (path, uuid) in entries {
                let status = CommandStatus {
                    uuid: uuid.clone(),
                    state: CommandState::Completed,
                    ts: Utc::now(),
                };
                debug!(uuid = %uuid, "publishing deferred restart completion");
                let status_topic = self.transport.command_topic("restart", "status");
                let status_ok = match serde_json::to_string(&status) {
                    Ok(payload) => match self
                        .transport
                        .publish(&status_topic, payload.as_bytes(), QoS::AtLeastOnce, false)
                        .await
                    {
                        Ok(()) => true,
                        Err(e) => {
                            warn!(error = %e, topic = %status_topic, "failed to publish restart status");
                            false
                        }
                    },
                    Err(e) => {
                        warn!(error = %e, "failed to serialize restart status");
                        false
                    }
                };

                let result = CommandResultMsg {
                    uuid,
                    ok: true,
                    ts: Utc::now(),
                    error: None,
                    data: Some(CommandResultData::Restart(RestartResult {
                        target: RestartTarget::Unitctl,
                    })),
                };
                let result_topic = self.transport.command_topic("restart", "result");
                let result_ok = match serde_json::to_string(&result) {
                    Ok(payload) => match self
                        .transport
                        .publish(&result_topic, payload.as_bytes(), QoS::AtLeastOnce, false)
                        .await
                    {
                        Ok(()) => true,
                        Err(e) => {
                            warn!(error = %e, topic = %result_topic, "failed to publish restart result");
                            false
                        }
                    },
                    Err(e) => {
                        warn!(error = %e, "failed to serialize restart result");
                        false
                    }
                };

                if status_ok && result_ok {
                    remove_path_logging(&path).await;
                    info!(uuid = %result.uuid, "published deferred restart completion");
                } else {
                    warn!(
                        path = ?path,
                        "deferred restart completion publish incomplete; retaining for retry on next boot"
                    );
                }
            }
        })]
    }
}

async fn verify_active<R: CommandRunner + ?Sized>(
    runner: &R,
    unit: &str,
    window: Duration,
    interval: Duration,
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
        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[derive(Debug, Clone)]
    struct FakeInvocation {
        pub program: String,
        pub args: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct ScriptedResponse {
        pub exit_code: i32,
        pub stdout: Vec<u8>,
        pub stderr: Vec<u8>,
    }

    struct FakeCommandRunner {
        invocations: tokio::sync::Mutex<Vec<FakeInvocation>>,
        responses: tokio::sync::Mutex<std::collections::VecDeque<(String, ScriptedResponse)>>,
    }

    impl FakeCommandRunner {
        fn new() -> Self {
            Self {
                invocations: tokio::sync::Mutex::new(Vec::new()),
                responses: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
            }
        }

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

    #[tokio::test]
    async fn verify_active_returns_ok_when_continuously_active() {
        let fake = FakeCommandRunner::new();
        for _ in 0..10 {
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
        fake.push_response(3, "failed\n", "").await;
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

    #[tokio::test]
    async fn fake_runner_records_and_replays() {
        let fake = FakeCommandRunner::new();
        fake.push_response(0, "active\n", "").await;
        fake.push_response(3, "", "Unit foo.service not loaded.\n")
            .await;

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

    use crate::messages::commands::{
        CommandEnvelope, CommandPayload, GetConfigPayload, RestartPayload, RestartTarget,
    };
    use crate::services::mqtt::commands::CommandHandler;

    fn make_envelope(target: RestartTarget) -> CommandEnvelope {
        CommandEnvelope {
            uuid: format!("restart-{:?}", target).to_lowercase(),
            issued_at: chrono::Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::Restart(RestartPayload { target }),
        }
    }

    #[tokio::test]
    async fn handler_camera_happy_path() {
        let fake = Arc::new(FakeCommandRunner::new());
        fake.push_response(0, "", "").await;
        fake.push_response(0, "active\n", "").await;
        fake.push_response(0, "active\n", "").await;

        let handler = RestartHandler::new_with_runner(
            fake.clone(),
            std::path::PathBuf::from("/tmp/unitctl-test"),
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(5),
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
        fake.push_response(1, "", "Failed to restart camera.service: Unit not found\n")
            .await;

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
        fake.push_response(0, "", "").await;
        fake.push_response(0, "active\n", "").await;
        fake.push_response(3, "failed\n", "").await;

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

    #[tokio::test]
    async fn handler_unitctl_writes_state_file_and_execs_restart() {
        let fake = Arc::new(FakeCommandRunner::new());
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

        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(100), handler.handle(&env)).await;

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
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(100), handler.handle(&env)).await;
        assert!(env_dir.join(PENDING_FILE_NAME).exists());
    }

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
        let result = handler.handle(&env).await.unwrap();
        match result.data {
            crate::messages::commands::CommandResultData::Restart(r) => {
                assert_eq!(r.target, RestartTarget::Reboot);
            }
            _ => panic!("expected Restart"),
        }
        assert!(fake.invocations().await.is_empty());
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let invs = fake.invocations().await;
        assert_eq!(invs.len(), 1);
        assert_eq!(invs[0].program, "reboot");
    }

    #[tokio::test]
    async fn completion_publisher_no_op_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let read = read_pending_file(tmp.path()).await.unwrap();
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn completion_publisher_reads_file_without_deleting() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(PENDING_FILE_NAME);
        std::fs::write(&path, "uuid-xyz\n").unwrap();
        let read = read_pending_file(tmp.path()).await.unwrap();
        assert_eq!(read.as_deref(), Some("uuid-xyz"));
        assert!(
            path.exists(),
            "file should be retained until publish completes"
        );
        delete_pending_file(tmp.path()).await;
        assert!(
            !path.exists(),
            "file should be deleted after explicit delete"
        );
    }

    #[tokio::test]
    async fn completion_publisher_handles_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(PENDING_FILE_NAME);
        std::fs::write(&path, "").unwrap();
        let read = read_pending_file(tmp.path()).await.unwrap();
        assert_eq!(read, Some(String::new()));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn delete_pending_file_is_no_op_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        delete_pending_file(tmp.path()).await;
    }

    #[tokio::test]
    async fn claim_main_pending_renames_to_unique_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join(PENDING_FILE_NAME);
        std::fs::write(&main, "uuid-1\n").unwrap();
        let (scratch, uuid) = claim_main_pending(tmp.path()).await.unwrap().unwrap();
        assert_eq!(uuid, "uuid-1");
        assert!(scratch.exists(), "scratch file must exist after claim");
        assert!(!main.exists(), "main file must be renamed away");
        let name = scratch.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with(PENDING_INFLIGHT_PREFIX));
    }

    #[tokio::test]
    async fn claim_main_pending_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let claimed = claim_main_pending(tmp.path()).await.unwrap();
        assert!(claimed.is_none());
    }

    #[tokio::test]
    async fn claim_isolates_from_concurrent_overwrite() {
        // Once claim_main_pending has renamed the main file out of the way,
        // a concurrent write to the main path creates a fresh inode and does
        // NOT mutate the scratch file that the publisher owns.
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join(PENDING_FILE_NAME);
        std::fs::write(&main, "uuid-A\n").unwrap();
        let (scratch, uuid) = claim_main_pending(tmp.path()).await.unwrap().unwrap();
        assert_eq!(uuid, "uuid-A");

        // Simulate a concurrent restart-unitctl handler writing a new uuid.
        std::fs::write(&main, "uuid-B\n").unwrap();

        // Scratch content is still A (not aliased to main).
        let scratch_contents = std::fs::read_to_string(&scratch).unwrap();
        assert_eq!(scratch_contents.trim(), "uuid-A");

        // Removing the scratch path does NOT remove the new main entry.
        tokio::fs::remove_file(&scratch).await.unwrap();
        assert!(
            main.exists(),
            "concurrent write must survive scratch cleanup"
        );
        let main_contents = std::fs::read_to_string(&main).unwrap();
        assert_eq!(main_contents.trim(), "uuid-B");
    }

    #[tokio::test]
    async fn collect_orphan_inflight_picks_up_prior_boot_files() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = tmp
            .path()
            .join(format!("{}999.123", PENDING_INFLIGHT_PREFIX));
        std::fs::write(&orphan, "uuid-orphan\n").unwrap();
        std::fs::write(tmp.path().join("unrelated"), "x").unwrap();
        let entries = collect_orphan_inflight(tmp.path()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "uuid-orphan");
        assert_eq!(entries[0].0, orphan);
    }

    #[tokio::test]
    async fn collect_orphan_inflight_returns_empty_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let entries = collect_orphan_inflight(&missing).await.unwrap();
        assert!(entries.is_empty());
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
        let res = tokio::time::timeout(std::time::Duration::from_millis(100), handler.handle(&env))
            .await
            .expect("handler should return on failure");
        let err = res.unwrap_err();
        assert!(err.message.contains("Access denied") || err.message.contains("exit"));
    }
}
