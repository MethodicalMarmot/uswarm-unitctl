use std::sync::Arc;

use tracing::{error, info};

use crate::config::Config;
use crate::context::Context;
use crate::Task;

/// Generate the Fluent Bit YAML config from `config`.
///
/// Caller must ensure `config.fluentbit.enabled == true`. When `fluentbit.tls`
/// is set, all three of `general.{ca,client_cert,client_key}_path` must be
/// `Some(non_empty)`; otherwise this returns `Err`.
pub fn generate_fluentbit_config(config: &Config) -> Result<String, FluentbitGenError> {
    let f = &config.fluentbit;
    let g = &config.general;

    let mut out = String::new();
    out.push_str("service:\n");
    out.push_str("  flush: 1\n");
    out.push_str("  log_level: info\n");
    out.push('\n');
    out.push_str("pipeline:\n");
    out.push_str("  inputs:\n");
    out.push_str("    - name: systemd\n");
    out.push_str("      tag: host.*\n");
    out.push_str("      read_from_tail: off\n");
    if let Some(filters) = &f.systemd_filter {
        if !filters.is_empty() {
            out.push_str("      systemd_filter:\n");
            for entry in filters {
                out.push_str("        - ");
                out.push_str(entry);
                out.push('\n');
            }
        }
    }
    out.push('\n');
    out.push_str("  outputs:\n");
    out.push_str("    - name: forward\n");
    out.push_str("      match: '*'\n");
    out.push_str(&format!("      host: {}\n", f.host));
    out.push_str(&format!("      port: {}\n", f.port));

    if f.tls {
        let ca = g
            .ca_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.ca_cert_path"))?;
        let cert = g
            .client_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.client_cert_path"))?;
        let key = g
            .client_key_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.client_key_path"))?;

        out.push_str("      tls: on\n");
        out.push_str(&format!(
            "      tls.verify: {}\n",
            if f.tls_verify { "on" } else { "off" }
        ));
        out.push_str(&format!("      tls.ca_file: {ca}\n"));
        out.push_str(&format!("      tls.crt_file: {cert}\n"));
        out.push_str(&format!("      tls.key_file: {key}\n"));
    }

    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum FluentbitGenError {
    #[error("missing TLS config: {0}")]
    MissingCert(&'static str),
}

pub struct FluentbitEnvWriter {
    ctx: Arc<Context>,
}

impl FluentbitEnvWriter {
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }
}

impl Task for FluentbitEnvWriter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let ctx = Arc::clone(&self.ctx);
        let handle = tokio::spawn(async move {
            let cfg = &ctx.config;
            if !cfg.fluentbit.enabled {
                info!("fluentbit disabled, skipping config write");
                return;
            }

            let path = cfg.fluentbit.config_path.clone();

            let content = match generate_fluentbit_config(cfg) {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "failed to generate fluentbit config");
                    return;
                }
            };

            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        error!(path = %path, error = %e, "failed to create parent directory for fluentbit config");
                        return;
                    }
                }
            }

            // Atomic write: write to a sibling tmp file, then rename.
            let tmp_path = format!("{path}.tmp");
            if let Err(e) = std::fs::write(&tmp_path, &content) {
                error!(path = %tmp_path, error = %e, "failed to write fluentbit tmp file");
                return;
            }
            match std::fs::rename(&tmp_path, &path) {
                Ok(()) => info!(path = %path, "fluentbit config written"),
                Err(e) => {
                    error!(path = %path, error = %e, "failed to rename fluentbit tmp file");
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
        });
        vec![handle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;

    fn enabled_config() -> crate::config::Config {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.general.ca_cert_path = Some("/etc/ca.pem".to_string());
        cfg.general.client_cert_path = Some("/etc/cert.pem".to_string());
        cfg.general.client_key_path = Some("/etc/key.pem".to_string());
        cfg
    }

    #[test]
    fn test_generate_includes_systemd_input_with_read_from_tail_off() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("- name: systemd"));
        assert!(yaml.contains("read_from_tail: off"));
        assert!(yaml.contains("tag: host.*"));
    }

    #[test]
    fn test_generate_includes_forward_output() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("- name: forward"));
        assert!(yaml.contains("host: logs.example.com"));
        assert!(yaml.contains("port: 24224"));
    }

    #[test]
    fn test_generate_omits_systemd_filter_when_none() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(!yaml.contains("systemd_filter:"));
    }

    #[test]
    fn test_generate_renders_systemd_filter_when_present() {
        let mut cfg = enabled_config();
        cfg.fluentbit.systemd_filter = Some(vec![
            "_SYSTEMD_UNIT=unitctl.service".to_string(),
            "PRIORITY=4".to_string(),
        ]);
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(yaml.contains(
            "systemd_filter:\n        - _SYSTEMD_UNIT=unitctl.service\n        - PRIORITY=4\n"
        ));
    }

    #[test]
    fn test_generate_with_tls_includes_cert_paths() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("tls: on"));
        assert!(yaml.contains("tls.verify: on"));
        assert!(yaml.contains("tls.ca_file: /etc/ca.pem"));
        assert!(yaml.contains("tls.crt_file: /etc/cert.pem"));
        assert!(yaml.contains("tls.key_file: /etc/key.pem"));
    }

    #[test]
    fn test_generate_with_tls_verify_off() {
        let mut cfg = enabled_config();
        cfg.fluentbit.tls_verify = false;
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(yaml.contains("tls.verify: off"));
    }

    #[test]
    fn test_generate_without_tls_omits_tls_block() {
        let mut cfg = enabled_config();
        cfg.fluentbit.tls = false;
        // Cert paths can even be None now.
        cfg.general.ca_cert_path = None;
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(!yaml.contains("tls:"));
        assert!(!yaml.contains("tls.verify"));
        assert!(!yaml.contains("tls.ca_file"));
    }

    #[test]
    fn test_generate_with_tls_missing_ca_path_errors() {
        let mut cfg = enabled_config();
        cfg.general.ca_cert_path = None;
        let err = generate_fluentbit_config(&cfg).unwrap_err();
        match err {
            FluentbitGenError::MissingCert(field) => {
                assert_eq!(field, "general.ca_cert_path");
            }
        }
    }

    #[test]
    fn test_generate_with_tls_empty_client_cert_errors() {
        let mut cfg = enabled_config();
        cfg.general.client_cert_path = Some(String::new());
        let err = generate_fluentbit_config(&cfg).unwrap_err();
        match err {
            FluentbitGenError::MissingCert(field) => {
                assert_eq!(field, "general.client_cert_path");
            }
        }
    }

    #[tokio::test]
    async fn test_writer_writes_file_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("- name: systemd"));
        assert!(written.contains("- name: forward"));
    }

    #[tokio::test]
    async fn test_writer_skips_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = test_config();
        cfg.fluentbit.enabled = false;
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(!path.exists(), "no file should be written when disabled");
    }

    #[tokio::test]
    async fn test_writer_skips_when_tls_required_but_cert_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.general.ca_cert_path = None;
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(
            !path.exists(),
            "no file should be written when cert missing"
        );
    }

    #[tokio::test]
    async fn test_writer_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/a/b/fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(path.exists());
    }
}
