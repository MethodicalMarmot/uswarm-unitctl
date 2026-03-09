use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "unitctl",
    about = "MAVLink onboard controller for drone link management"
)]
pub struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,

    /// Enable debug logging
    #[arg(short, long)]
    pub debug: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    pub mavlink: MavlinkConfig,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
pub struct GeneralConfig {
    #[serde(default)]
    pub debug: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct MavlinkConfig {
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_self_sysid")]
    pub self_sysid: u8,
    #[serde(default = "default_self_compid")]
    pub self_compid: u8,
    #[serde(default = "default_gcs_sysid")]
    pub gcs_sysid: u8,
    #[serde(default = "default_gcs_compid")]
    pub gcs_compid: u8,
    #[serde(default = "default_sniffer_sysid")]
    pub sniffer_sysid: u8,
    #[serde(default = "default_bs_sysid")]
    pub bs_sysid: u8,
    #[serde(default = "default_iteration_period_ms")]
    pub iteration_period_ms: u64,
    #[serde(default)]
    pub fc: FcConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct FcConfig {
    #[serde(default = "default_fc_tty")]
    pub tty: String,
    #[serde(default = "default_fc_baudrate")]
    pub baudrate: u32,
}

impl Default for FcConfig {
    fn default() -> Self {
        Self {
            tty: default_fc_tty(),
            baudrate: default_fc_baudrate(),
        }
    }
}

fn default_protocol() -> String {
    "tcpout".to_string()
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    5760
}
fn default_self_sysid() -> u8 {
    1
}
fn default_self_compid() -> u8 {
    10
}
fn default_gcs_sysid() -> u8 {
    255
}
fn default_gcs_compid() -> u8 {
    190
}
fn default_sniffer_sysid() -> u8 {
    199
}
fn default_bs_sysid() -> u8 {
    200
}
fn default_iteration_period_ms() -> u64 {
    10
}
fn default_fc_tty() -> String {
    "/dev/ttyFC".to_string()
}
fn default_fc_baudrate() -> u32 {
    57600
}

impl MavlinkConfig {
    /// Returns the MAVLink connection string (e.g., "tcpout:127.0.0.1:5760")
    pub fn connection_string(&self) -> String {
        format!("{}:{}:{}", self.protocol, self.host, self.port)
    }
}

pub fn load_config(path: &std::path::Path) -> Result<Config, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    config.validate()?;
    Ok(config)
}

/// Protocols supported by unitctl's dual-connection architecture.
///
/// Only `tcpout` is supported because:
/// - Both drone and sniffer open independent connections to the same address.
///   Bind-oriented protocols (tcpin, udpin, serial) would cause one side to fail.
/// - The sniffer's cancellation-safe recv uses a 500ms timeout that relies on
///   tcpout's ~100ms read timeout. Other protocols block indefinitely.
/// - The connection string format (protocol:host:port) doesn't map to serial
///   device paths.
const VALID_PROTOCOLS: &[&str] = &["tcpout"];

impl Config {
    /// Validate configuration values that could cause runtime panics or incorrect behavior.
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.mavlink.iteration_period_ms == 0 {
            return Err("mavlink.iteration_period_ms must be greater than 0".into());
        }
        if !VALID_PROTOCOLS.contains(&self.mavlink.protocol.as_str()) {
            return Err(format!(
                "mavlink.protocol must be one of {:?}, got {:?}",
                VALID_PROTOCOLS, self.mavlink.protocol
            )
            .into());
        }
        let m = &self.mavlink;
        if m.self_sysid == m.sniffer_sysid {
            return Err("mavlink.self_sysid must differ from sniffer_sysid".into());
        }
        if m.self_sysid == m.bs_sysid {
            return Err("mavlink.self_sysid must differ from bs_sysid".into());
        }
        if m.sniffer_sysid == m.bs_sysid {
            return Err("mavlink.sniffer_sysid must differ from bs_sysid".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[general]
debug = true

[mavlink]
protocol = "tcpout"
host = "192.168.1.100"
port = 5761
self_sysid = 2
self_compid = 11
gcs_sysid = 254
gcs_compid = 191
sniffer_sysid = 198
bs_sysid = 201
iteration_period_ms = 20

[mavlink.fc]
tty = "/dev/ttyS0"
baudrate = 115200
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.general.debug);
        assert_eq!(config.mavlink.protocol, "tcpout");
        assert_eq!(config.mavlink.host, "192.168.1.100");
        assert_eq!(config.mavlink.port, 5761);
        assert_eq!(config.mavlink.self_sysid, 2);
        assert_eq!(config.mavlink.self_compid, 11);
        assert_eq!(config.mavlink.gcs_sysid, 254);
        assert_eq!(config.mavlink.gcs_compid, 191);
        assert_eq!(config.mavlink.sniffer_sysid, 198);
        assert_eq!(config.mavlink.bs_sysid, 201);
        assert_eq!(config.mavlink.iteration_period_ms, 20);
        assert_eq!(config.mavlink.fc.tty, "/dev/ttyS0");
        assert_eq!(config.mavlink.fc.baudrate, 115200);
    }

    #[test]
    fn test_parse_minimal_config_with_defaults() {
        let toml_str = r#"
[mavlink]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.general.debug);
        assert_eq!(config.mavlink.protocol, "tcpout");
        assert_eq!(config.mavlink.host, "127.0.0.1");
        assert_eq!(config.mavlink.port, 5760);
        assert_eq!(config.mavlink.self_sysid, 1);
        assert_eq!(config.mavlink.self_compid, 10);
        assert_eq!(config.mavlink.gcs_sysid, 255);
        assert_eq!(config.mavlink.gcs_compid, 190);
        assert_eq!(config.mavlink.sniffer_sysid, 199);
        assert_eq!(config.mavlink.bs_sysid, 200);
        assert_eq!(config.mavlink.iteration_period_ms, 10);
        assert_eq!(config.mavlink.fc.tty, "/dev/ttyFC");
        assert_eq!(config.mavlink.fc.baudrate, 57600);
    }

    #[test]
    fn test_missing_mavlink_section_fails() {
        let toml_str = r#"
[general]
debug = true
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_connection_string() {
        let toml_str = r#"
[mavlink]
protocol = "tcpout"
host = "10.0.0.1"
port = 5760
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mavlink.connection_string(), "tcpout:10.0.0.1:5760");
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = std::env::temp_dir().join("unitctl_test_config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_config.toml");
        std::fs::write(
            &path,
            r#"
[mavlink]
host = "10.0.0.5"
port = 5761
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.mavlink.host, "10.0.0.5");
        assert_eq!(config.mavlink.port, 5761);
        // defaults still apply
        assert_eq!(config.mavlink.protocol, "tcpout");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_config_missing_file() {
        let result = load_config(std::path::Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_iteration_period_ms_zero_rejected() {
        let toml_str = r#"
[mavlink]
iteration_period_ms = 0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_protocol_rejected() {
        let toml_str = r#"
[mavlink]
protocol = "tcp"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("protocol"));
    }

    #[test]
    fn test_valid_protocols_accepted() {
        for protocol in &["tcpout"] {
            let toml_str = format!("[mavlink]\nprotocol = \"{}\"", protocol);
            let config: Config = toml::from_str(&toml_str).unwrap();
            assert!(
                config.validate().is_ok(),
                "protocol {} should be valid",
                protocol
            );
        }
    }

    #[test]
    fn test_unsupported_protocols_rejected() {
        for protocol in &["tcpin", "udpin", "udpout", "serial"] {
            let toml_str = format!("[mavlink]\nprotocol = \"{}\"", protocol);
            let config: Config = toml::from_str(&toml_str).unwrap();
            assert!(
                config.validate().is_err(),
                "protocol {} should be rejected",
                protocol
            );
        }
    }

    #[test]
    fn test_iteration_period_ms_nonzero_accepted() {
        let toml_str = r#"
[mavlink]
iteration_period_ms = 1
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_duplicate_sysid_self_sniffer_rejected() {
        let toml_str = "[mavlink]\nself_sysid = 199\nsniffer_sysid = 199\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sniffer_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_self_bs_rejected() {
        let toml_str = "[mavlink]\nself_sysid = 200\nbs_sysid = 200\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("bs_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_sniffer_bs_rejected() {
        let toml_str = "[mavlink]\nsniffer_sysid = 200\nbs_sysid = 200\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sniffer_sysid"));
    }

    #[test]
    fn test_distinct_sysids_accepted() {
        let toml_str = "[mavlink]\nself_sysid = 1\nsniffer_sysid = 199\nbs_sysid = 200\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_load_config_invalid_toml() {
        let dir = std::env::temp_dir().join("unitctl_test_invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad_config.toml");
        std::fs::write(&path, "this is not valid toml {{{{").unwrap();

        let result = load_config(&path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
