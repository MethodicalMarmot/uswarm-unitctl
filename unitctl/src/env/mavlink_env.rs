use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::MavlinkConfig;
use crate::context::Context;
use crate::Task;

pub struct MavlinkEnvWriter {
    ctx: Arc<Context>,
    _cancel: CancellationToken,
}

impl MavlinkEnvWriter {
    pub fn new(ctx: Arc<Context>, cancel: CancellationToken) -> Self {
        Self {
            ctx,
            _cancel: cancel,
        }
    }
}

/// Generate the mavlink env file content from config.
pub fn generate_mavlink_env(config: &MavlinkConfig) -> String {
    format!(
        "GCS_IP={}\n\
         REMOTE_MAVLINK_PORT={}\n\
         SNIFFER_SYS_ID={}\n\
         LOCAL_MAVLINK_PORT={}\n\
         FC_TTY={}\n\
         FC_BAUDRATE={}",
        config.gcs_ip,
        config.remote_mavlink_port,
        config.sniffer_sysid,
        config.local_mavlink_port,
        config.fc.tty,
        config.fc.baudrate,
    )
}

impl Task for MavlinkEnvWriter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let ctx = Arc::clone(&self.ctx);
        let handle = tokio::spawn(async move {
            let content = generate_mavlink_env(&ctx.config.mavlink);
            let path = &ctx.config.mavlink.env_path;

            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        error!(path = %path, error = %e, "failed to create parent directory for mavlink env file");
                        return;
                    }
                }
            }

            match std::fs::write(path, &content) {
                Ok(()) => info!(path = %path, "mavlink env file written"),
                Err(e) => error!(path = %path, error = %e, "failed to write mavlink env file"),
            }
        });

        vec![handle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::context::Context;
    use std::sync::Arc;

    #[test]
    fn test_generate_mavlink_env_content() {
        let config = test_config();
        let content = generate_mavlink_env(&config.mavlink);

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "GCS_IP=10.101.0.1");
        assert_eq!(lines[1], "REMOTE_MAVLINK_PORT=5760");
        assert_eq!(lines[2], "SNIFFER_SYS_ID=199");
        assert_eq!(lines[3], "LOCAL_MAVLINK_PORT=5760");
        assert_eq!(lines[4], "FC_TTY=/dev/ttyFC");
        assert_eq!(lines[5], "FC_BAUDRATE=57600");
    }

    #[test]
    fn test_generate_mavlink_env_no_trailing_newline() {
        let config = test_config();
        let content = generate_mavlink_env(&config.mavlink);
        assert!(!content.ends_with('\n'));
    }

    #[test]
    fn test_generate_mavlink_env_key_value_format() {
        let config = test_config();
        let content = generate_mavlink_env(&config.mavlink);
        for line in content.lines() {
            assert!(
                line.contains('='),
                "each line must be KEY=VALUE format: {}",
                line
            );
            // No quotes around values
            assert!(!line.contains('"'), "values must not be quoted: {}", line);
            assert!(!line.contains('\''), "values must not be quoted: {}", line);
        }
    }

    #[test]
    fn test_generate_mavlink_env_custom_values() {
        let mut config = test_config();
        config.mavlink.gcs_ip = "192.168.1.100".to_string();
        config.mavlink.local_mavlink_port = 14550;
        config.mavlink.remote_mavlink_port = 14550;
        config.mavlink.sniffer_sysid = 250;
        config.mavlink.fc.tty = "/dev/ttyS1".to_string();
        config.mavlink.fc.baudrate = 115200;

        let content = generate_mavlink_env(&config.mavlink);
        assert!(content.contains("GCS_IP=192.168.1.100"));
        assert!(content.contains("REMOTE_MAVLINK_PORT=14550"));
        assert!(content.contains("SNIFFER_SYS_ID=250"));
        assert!(content.contains("LOCAL_MAVLINK_PORT=14550"));
        assert!(content.contains("FC_TTY=/dev/ttyS1"));
        assert!(content.contains("FC_BAUDRATE=115200"));
    }

    #[tokio::test]
    async fn test_mavlink_env_writer_writes_file() {
        let dir = std::env::temp_dir().join("unitctl_test_mavlink_env");
        std::fs::create_dir_all(&dir).unwrap();
        let env_path = dir.join("mavlink.env");

        let mut config = test_config();
        config.mavlink.env_path = env_path.to_string_lossy().to_string();

        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let writer = Arc::new(MavlinkEnvWriter::new(Arc::clone(&ctx), cancel));
        let handles = writer.run();

        for handle in handles {
            handle.await.unwrap();
        }

        let written = std::fs::read_to_string(&env_path).unwrap();
        assert!(written.contains("GCS_IP=10.101.0.1"));
        assert!(written.contains("REMOTE_MAVLINK_PORT=5760"));
        assert!(written.contains("FC_TTY=/dev/ttyFC"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn test_mavlink_env_writer_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("unitctl_test_mavlink_env_nested/a/b");
        // Ensure dir doesn't exist yet
        let _ =
            std::fs::remove_dir_all(std::env::temp_dir().join("unitctl_test_mavlink_env_nested"));

        let env_path = dir.join("mavlink.env");

        let mut config = test_config();
        config.mavlink.env_path = env_path.to_string_lossy().to_string();

        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let writer = Arc::new(MavlinkEnvWriter::new(Arc::clone(&ctx), cancel));
        let handles = writer.run();

        for handle in handles {
            handle.await.unwrap();
        }

        let written = std::fs::read_to_string(&env_path).unwrap();
        assert!(written.contains("GCS_IP=10.101.0.1"));

        std::fs::remove_dir_all(std::env::temp_dir().join("unitctl_test_mavlink_env_nested")).ok();
    }
}
