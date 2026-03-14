use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::CameraConfig;
use crate::context::Context;
use crate::Task;

pub struct CameraEnvWriter {
    ctx: Arc<Context>,
    _cancel: CancellationToken,
}

impl CameraEnvWriter {
    pub fn new(ctx: Arc<Context>, cancel: CancellationToken) -> Self {
        Self {
            ctx,
            _cancel: cancel,
        }
    }
}

/// Generate the camera env file content from config.
pub fn generate_camera_env(config: &CameraConfig) -> String {
    format!(
        "GCS_IP={}\n\
         REMOTE_VIDEO_PORT={}\n\
         CAMERA_WIDTH={}\n\
         CAMERA_HEIGHT={}\n\
         CAMERA_FRAMERATE={}\n\
         CAMERA_BITRATE={}\n\
         CAMERA_FLIP={}\n\
         CAMERA_TYPE={}\n\
         CAMERA_DEVICE={}",
        config.gcs_ip,
        config.remote_video_port,
        config.width,
        config.height,
        config.framerate,
        config.bitrate,
        config.flip,
        config.camera_type,
        config.device,
    )
}

impl Task for CameraEnvWriter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let ctx = Arc::clone(&self.ctx);
        let handle = tokio::spawn(async move {
            let content = generate_camera_env(&ctx.config.camera);
            let path = &ctx.config.camera.env_path;

            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        error!(path = %path, error = %e, "failed to create parent directory for camera env file");
                        return;
                    }
                }
            }

            match std::fs::write(path, &content) {
                Ok(()) => info!(path = %path, "camera env file written"),
                Err(e) => error!(path = %path, error = %e, "failed to write camera env file"),
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
    fn test_generate_camera_env_content() {
        let config = test_config();
        let content = generate_camera_env(&config.camera);

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[0], "GCS_IP=10.101.0.1");
        assert_eq!(lines[1], "REMOTE_VIDEO_PORT=5600");
        assert_eq!(lines[2], "CAMERA_WIDTH=640");
        assert_eq!(lines[3], "CAMERA_HEIGHT=360");
        assert_eq!(lines[4], "CAMERA_FRAMERATE=60");
        assert_eq!(lines[5], "CAMERA_BITRATE=1664000");
        assert_eq!(lines[6], "CAMERA_FLIP=0");
        assert_eq!(lines[7], "CAMERA_TYPE=rpi");
        assert_eq!(lines[8], "CAMERA_DEVICE=/dev/video1");
    }

    #[test]
    fn test_generate_camera_env_no_trailing_newline() {
        let config = test_config();
        let content = generate_camera_env(&config.camera);
        assert!(!content.ends_with('\n'));
    }

    #[test]
    fn test_generate_camera_env_key_value_format() {
        let config = test_config();
        let content = generate_camera_env(&config.camera);
        for line in content.lines() {
            assert!(
                line.contains('='),
                "each line must be KEY=VALUE format: {}",
                line
            );
            assert!(!line.contains('"'), "values must not be quoted: {}", line);
            assert!(!line.contains('\''), "values must not be quoted: {}", line);
        }
    }

    #[test]
    fn test_generate_camera_env_custom_values() {
        let mut config = test_config();
        config.camera.gcs_ip = "192.168.1.100".to_string();
        config.camera.remote_video_port = 5601;
        config.camera.width = 1920;
        config.camera.height = 1080;
        config.camera.framerate = 30;
        config.camera.bitrate = 4000000;
        config.camera.flip = 2;
        config.camera.camera_type = "usb".to_string();
        config.camera.device = "/dev/video0".to_string();

        let content = generate_camera_env(&config.camera);
        assert!(content.contains("GCS_IP=192.168.1.100"));
        assert!(content.contains("REMOTE_VIDEO_PORT=5601"));
        assert!(content.contains("CAMERA_WIDTH=1920"));
        assert!(content.contains("CAMERA_HEIGHT=1080"));
        assert!(content.contains("CAMERA_FRAMERATE=30"));
        assert!(content.contains("CAMERA_BITRATE=4000000"));
        assert!(content.contains("CAMERA_FLIP=2"));
        assert!(content.contains("CAMERA_TYPE=usb"));
        assert!(content.contains("CAMERA_DEVICE=/dev/video0"));
    }

    #[tokio::test]
    async fn test_camera_env_writer_writes_file() {
        let dir = std::env::temp_dir().join("unitctl_test_camera_env");
        std::fs::create_dir_all(&dir).unwrap();
        let env_path = dir.join("camera.env");

        let mut config = test_config();
        config.camera.env_path = env_path.to_string_lossy().to_string();

        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let writer = Arc::new(CameraEnvWriter::new(Arc::clone(&ctx), cancel));
        let handles = writer.run();

        for handle in handles {
            handle.await.unwrap();
        }

        let written = std::fs::read_to_string(&env_path).unwrap();
        assert!(written.contains("GCS_IP=10.101.0.1"));
        assert!(written.contains("REMOTE_VIDEO_PORT=5600"));
        assert!(written.contains("CAMERA_WIDTH=640"));
        assert!(written.contains("CAMERA_TYPE=rpi"));
        assert!(written.contains("CAMERA_DEVICE=/dev/video1"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn test_camera_env_writer_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("unitctl_test_camera_env_nested/a/b");
        let _ =
            std::fs::remove_dir_all(std::env::temp_dir().join("unitctl_test_camera_env_nested"));

        let env_path = dir.join("camera.env");

        let mut config = test_config();
        config.camera.env_path = env_path.to_string_lossy().to_string();

        let ctx = Context::new(config);
        let cancel = CancellationToken::new();

        let writer = Arc::new(CameraEnvWriter::new(Arc::clone(&ctx), cancel));
        let handles = writer.run();

        for handle in handles {
            handle.await.unwrap();
        }

        let written = std::fs::read_to_string(&env_path).unwrap();
        assert!(written.contains("GCS_IP=10.101.0.1"));

        std::fs::remove_dir_all(std::env::temp_dir().join("unitctl_test_camera_env_nested")).ok();
    }
}
