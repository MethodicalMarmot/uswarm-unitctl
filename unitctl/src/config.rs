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
    pub general: GeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct GeneralConfig {
    pub debug: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct MavlinkConfig {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub self_sysid: u8,
    pub self_compid: u8,
    pub gcs_sysid: u8,
    pub gcs_compid: u8,
    pub sniffer_sysid: u8,
    pub bs_sysid: u8,
    pub iteration_period_ms: u64,
    pub fc: FcConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct FcConfig {
    pub tty: String,
    pub baudrate: u32,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SensorsConfig {
    pub default_interval_s: f64,
    pub ping: PingSensorConfig,
    pub lte: LteSensorConfig,
    pub cpu_temp: CpuTempSensorConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct PingSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
    pub host: String,
    pub interface: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct LteSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
    pub neighbor_expiry_s: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct CpuTempSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
}

impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            default_interval_s: 1.0,
            ping: PingSensorConfig::default(),
            lte: LteSensorConfig::default(),
            cpu_temp: CpuTempSensorConfig::default(),
        }
    }
}

impl Default for PingSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
            host: "10.45.0.2".to_string(),
            interface: String::new(),
        }
    }
}

impl Default for LteSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
            neighbor_expiry_s: 30.0,
        }
    }
}

impl Default for CpuTempSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
        }
    }
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
        if m.self_sysid == m.gcs_sysid {
            return Err("mavlink.self_sysid must differ from gcs_sysid".into());
        }
        if m.gcs_sysid == m.sniffer_sysid {
            return Err("mavlink.gcs_sysid must differ from sniffer_sysid".into());
        }
        if m.gcs_sysid == m.bs_sysid {
            return Err("mavlink.gcs_sysid must differ from bs_sysid".into());
        }

        // Validate sensor intervals (must be finite and positive; TOML allows inf/nan)
        if !self.sensors.default_interval_s.is_finite() || self.sensors.default_interval_s <= 0.0 {
            return Err("sensors.default_interval_s must be a finite positive number".into());
        }
        if let Some(v) = self.sensors.ping.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err("sensors.ping.interval_s must be a finite positive number".into());
            }
        }
        if let Some(v) = self.sensors.lte.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err("sensors.lte.interval_s must be a finite positive number".into());
            }
        }
        if let Some(v) = self.sensors.cpu_temp.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err("sensors.cpu_temp.interval_s must be a finite positive number".into());
            }
        }
        if !self.sensors.lte.neighbor_expiry_s.is_finite()
            || self.sensors.lte.neighbor_expiry_s <= 0.0
        {
            return Err("sensors.lte.neighbor_expiry_s must be a finite positive number".into());
        }
        if self.sensors.ping.host.is_empty() || self.sensors.ping.host.starts_with('-') {
            return Err("sensors.ping.host must be a valid hostname or IP address".into());
        }
        if !self.sensors.ping.interface.is_empty() && self.sensors.ping.interface.starts_with('-') {
            return Err("sensors.ping.interface must not start with '-'".into());
        }

        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const FULL_TEST_CONFIG: &str = r#"
[general]
debug = false

[mavlink]
protocol = "tcpout"
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

[sensors]
default_interval_s = 1.0

[sensors.ping]
enabled = true
host = "10.45.0.2"
interface = ""

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0

[sensors.cpu_temp]
enabled = true
"#;

    pub fn test_config() -> Config {
        toml::from_str(FULL_TEST_CONFIG).unwrap()
    }

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

[sensors]
default_interval_s = 1.0

[sensors.ping]
enabled = true
host = "10.45.0.2"
interface = ""

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0

[sensors.cpu_temp]
enabled = true
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
    fn test_parse_config_from_constant() {
        let config = test_config();
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
        assert_eq!(config.sensors.default_interval_s, 1.0);
        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, None);
        assert_eq!(config.sensors.ping.host, "10.45.0.2");
        assert_eq!(config.sensors.ping.interface, "");
        assert!(config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, None);
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 30.0);
        assert!(config.sensors.cpu_temp.enabled);
        assert_eq!(config.sensors.cpu_temp.interval_s, None);
    }

    #[test]
    fn test_missing_section_fails() {
        // Missing [sensors] section
        let toml_str = r#"
[general]
debug = true

[mavlink]
protocol = "tcpout"
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
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_field_fails() {
        // Missing protocol field
        let toml_str = r#"
[general]
debug = false

[mavlink]
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

[sensors]
default_interval_s = 1.0

[sensors.ping]
enabled = true
host = "10.45.0.2"
interface = ""

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0

[sensors.cpu_temp]
enabled = true
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_connection_string() {
        let config = test_config();
        assert_eq!(config.mavlink.connection_string(), "tcpout:127.0.0.1:5760");
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = std::env::temp_dir().join("unitctl_test_config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_config.toml");
        std::fs::write(&path, FULL_TEST_CONFIG).unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.mavlink.host, "127.0.0.1");
        assert_eq!(config.mavlink.port, 5760);
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
        let mut config = test_config();
        config.mavlink.iteration_period_ms = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_protocol_rejected() {
        let mut config = test_config();
        config.mavlink.protocol = "tcp".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("protocol"));
    }

    #[test]
    fn test_valid_protocols_accepted() {
        let config = test_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_unsupported_protocols_rejected() {
        for protocol in &["tcpin", "udpin", "udpout", "serial"] {
            let mut config = test_config();
            config.mavlink.protocol = protocol.to_string();
            assert!(
                config.validate().is_err(),
                "protocol {} should be rejected",
                protocol
            );
        }
    }

    #[test]
    fn test_iteration_period_ms_nonzero_accepted() {
        let mut config = test_config();
        config.mavlink.iteration_period_ms = 1;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_duplicate_sysid_self_sniffer_rejected() {
        let mut config = test_config();
        config.mavlink.self_sysid = 199;
        config.mavlink.sniffer_sysid = 199;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sniffer_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_self_bs_rejected() {
        let mut config = test_config();
        config.mavlink.self_sysid = 200;
        config.mavlink.bs_sysid = 200;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("bs_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_sniffer_bs_rejected() {
        let mut config = test_config();
        config.mavlink.sniffer_sysid = 200;
        config.mavlink.bs_sysid = 200;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sniffer_sysid"));
    }

    #[test]
    fn test_distinct_sysids_accepted() {
        let config = test_config();
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

    #[test]
    fn test_sensor_config_values() {
        let config = test_config();

        assert_eq!(config.sensors.default_interval_s, 1.0);
        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, None);
        assert_eq!(config.sensors.ping.host, "10.45.0.2");
        assert_eq!(config.sensors.ping.interface, "");
        assert!(config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, None);
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 30.0);
        assert!(config.sensors.cpu_temp.enabled);
        assert_eq!(config.sensors.cpu_temp.interval_s, None);
    }

    #[test]
    fn test_sensor_config_full() {
        let toml_str = r#"
[general]
debug = false

[mavlink]
protocol = "tcpout"
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

[sensors]
default_interval_s = 2.0

[sensors.ping]
enabled = true
interval_s = 0.5
host = "192.168.1.1"
interface = "eth0"

[sensors.lte]
enabled = false
interval_s = 3.0
neighbor_expiry_s = 60.0

[sensors.cpu_temp]
enabled = true
interval_s = 10.0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();

        assert_eq!(config.sensors.default_interval_s, 2.0);

        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, Some(0.5));
        assert_eq!(config.sensors.ping.host, "192.168.1.1");
        assert_eq!(config.sensors.ping.interface, "eth0");

        assert!(!config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, Some(3.0));
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 60.0);

        assert!(config.sensors.cpu_temp.enabled);
        assert_eq!(config.sensors.cpu_temp.interval_s, Some(10.0));
    }

    #[test]
    fn test_duplicate_sysid_self_gcs_rejected() {
        let mut config = test_config();
        config.mavlink.self_sysid = 255;
        config.mavlink.gcs_sysid = 255;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("gcs_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_gcs_sniffer_rejected() {
        let mut config = test_config();
        config.mavlink.gcs_sysid = 199;
        config.mavlink.sniffer_sysid = 199;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("gcs_sysid"));
    }

    #[test]
    fn test_duplicate_sysid_gcs_bs_rejected() {
        let mut config = test_config();
        config.mavlink.gcs_sysid = 200;
        config.mavlink.bs_sysid = 200;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("gcs_sysid"));
    }

    #[test]
    fn test_zero_default_sensor_interval_rejected() {
        let mut config = test_config();
        config.sensors.default_interval_s = 0.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_negative_ping_interval_rejected() {
        let mut config = test_config();
        config.sensors.ping.interval_s = Some(-1.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_negative_lte_interval_rejected() {
        let mut config = test_config();
        config.sensors.lte.interval_s = Some(-1.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_negative_cpu_temp_interval_rejected() {
        let mut config = test_config();
        config.sensors.cpu_temp.interval_s = Some(-1.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_zero_neighbor_expiry_rejected() {
        let mut config = test_config();
        config.sensors.lte.neighbor_expiry_s = 0.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_empty_ping_host_rejected() {
        let mut config = test_config();
        config.sensors.ping.host = "".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_dash_prefix_ping_host_rejected() {
        let mut config = test_config();
        config.sensors.ping.host = "-n 1".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_per_sensor_interval_override() {
        let mut config = test_config();
        config.sensors.default_interval_s = 2.0;
        config.sensors.ping.interval_s = Some(0.5);
        config.sensors.lte.interval_s = Some(3.0);
        config.sensors.cpu_temp.interval_s = Some(10.0);

        assert!(config.validate().is_ok());
        assert_eq!(config.sensors.ping.interval_s, Some(0.5));
        assert_eq!(config.sensors.lte.interval_s, Some(3.0));
        assert_eq!(config.sensors.cpu_temp.interval_s, Some(10.0));
    }

    #[test]
    fn test_inf_default_interval_rejected() {
        let mut config = test_config();
        config.sensors.default_interval_s = f64::INFINITY;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_nan_default_interval_rejected() {
        let mut config = test_config();
        config.sensors.default_interval_s = f64::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_inf_neighbor_expiry_rejected() {
        let mut config = test_config();
        config.sensors.lte.neighbor_expiry_s = f64::INFINITY;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_dash_prefix_ping_interface_rejected() {
        let mut config = test_config();
        config.sensors.ping.interface = "-evil".to_string();
        assert!(config.validate().is_err());
    }
}
