use clap::Parser;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Errors that can occur when loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Failed to read the configuration file.
    #[error("failed to read configuration file: {0}")]
    Io(#[from] std::io::Error),
    /// Failed to parse the configuration file as TOML.
    #[error("failed to parse configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// Configuration values failed validation.
    #[error("invalid configuration: {0}")]
    Validation(String),
}

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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct Config {
    pub general: GeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
    pub camera: CameraConfig,
    pub mqtt: MqttConfig,
    pub fluentbit: FluentbitConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct GeneralConfig {
    pub debug: bool,
    pub interface: String,
    pub env_dir: String,
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    #[serde(default)]
    pub client_cert_path: Option<String>,
    #[serde(default)]
    pub client_key_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct MavlinkConfig {
    pub protocol: String,
    pub host: String,
    pub local_mavlink_port: u16,
    pub remote_mavlink_port: u16,
    pub self_sysid: u8,
    pub self_compid: u8,
    pub gcs_sysid: u8,
    pub gcs_compid: u8,
    pub sniffer_sysid: u8,
    pub bs_sysid: u8,
    pub iteration_period_ms: u64,
    pub gcs_ip: String,
    pub env_path: String,
    pub fc: FcConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct FcConfig {
    pub tty: String,
    pub baudrate: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct CameraConfig {
    pub gcs_ip: String,
    pub env_path: String,
    pub remote_video_port: u16,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub bitrate: u32,
    pub flip: u8,
    pub camera_type: String,
    pub device: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct SensorsConfig {
    pub default_interval_s: f64,
    pub ping: PingSensorConfig,
    pub lte: LteSensorConfig,
    pub system: SystemSensorConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct PingSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
    pub host: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct LteSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
    pub neighbor_expiry_s: f64,
    pub modem_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct SystemSensorConfig {
    pub enabled: bool,
    pub interval_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct MqttConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub env_prefix: String,
    pub telemetry_interval_s: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct FluentbitConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub tls_verify: bool,
    pub config_path: String,
    #[serde(default)]
    pub systemd_filter: Option<Vec<String>>,
}

impl Default for FluentbitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "logs.example.com".to_string(),
            port: 24224,
            tls: true,
            tls_verify: true,
            config_path: "/etc/fluent-bit.conf".to_string(),
            systemd_filter: None,
        }
    }
}

impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            default_interval_s: 5.0,
            ping: PingSensorConfig::default(),
            lte: LteSensorConfig::default(),
            system: SystemSensorConfig::default(),
        }
    }
}

impl Default for PingSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
            host: "10.45.0.2".to_string(),
        }
    }
}

impl Default for LteSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
            neighbor_expiry_s: 30.0,
            modem_type: "dbus".to_string(),
        }
    }
}

impl Default for SystemSensorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: None,
        }
    }
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "mqtt.example.com".to_string(),
            port: 8883,
            env_prefix: "prod".to_string(),
            telemetry_interval_s: 5.0,
        }
    }
}

impl MavlinkConfig {
    /// Returns the MAVLink connection string (e.g., "tcpout:127.0.0.1:5760")
    pub fn connection_string(&self) -> String {
        format!(
            "{}:{}:{}",
            self.protocol, self.host, self.local_mavlink_port
        )
    }
}

pub fn load_config(path: &std::path::Path) -> Result<Config, ConfigError> {
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
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.mavlink.iteration_period_ms == 0 {
            return Err(ConfigError::Validation(
                "mavlink.iteration_period_ms must be greater than 0".to_string(),
            ));
        }
        if !VALID_PROTOCOLS.contains(&self.mavlink.protocol.as_str()) {
            return Err(ConfigError::Validation(format!(
                "mavlink.protocol must be one of {:?}, got {:?}",
                VALID_PROTOCOLS, self.mavlink.protocol
            )));
        }
        let m = &self.mavlink;
        // self_sysid == 0 is the autodiscovery sentinel: take min of available_systems at runtime.
        // Skip uniqueness checks involving self_sysid in that case.
        if m.self_sysid != 0 && m.self_sysid == m.sniffer_sysid {
            return Err(ConfigError::Validation(
                "mavlink.self_sysid must differ from sniffer_sysid".to_string(),
            ));
        }
        if m.self_sysid != 0 && m.self_sysid == m.bs_sysid {
            return Err(ConfigError::Validation(
                "mavlink.self_sysid must differ from bs_sysid".to_string(),
            ));
        }
        if m.sniffer_sysid == m.bs_sysid {
            return Err(ConfigError::Validation(
                "mavlink.sniffer_sysid must differ from bs_sysid".to_string(),
            ));
        }
        if m.self_sysid != 0 && m.self_sysid == m.gcs_sysid {
            return Err(ConfigError::Validation(
                "mavlink.self_sysid must differ from gcs_sysid".to_string(),
            ));
        }
        if m.gcs_sysid == m.sniffer_sysid {
            return Err(ConfigError::Validation(
                "mavlink.gcs_sysid must differ from sniffer_sysid".to_string(),
            ));
        }
        if m.gcs_sysid == m.bs_sysid {
            return Err(ConfigError::Validation(
                "mavlink.gcs_sysid must differ from bs_sysid".to_string(),
            ));
        }
        if m.env_path.is_empty() {
            return Err(ConfigError::Validation(
                "mavlink.env_path must not be empty".to_string(),
            ));
        }
        if m.gcs_ip.is_empty() || m.gcs_ip.starts_with('-') {
            return Err(ConfigError::Validation(
                "mavlink.gcs_ip must be a non-empty value that does not start with '-'".to_string(),
            ));
        }
        if m.fc.tty.is_empty() {
            return Err(ConfigError::Validation(
                "mavlink.fc.tty must not be empty".to_string(),
            ));
        }
        if m.fc.baudrate == 0 {
            return Err(ConfigError::Validation(
                "mavlink.fc.baudrate must be greater than 0".to_string(),
            ));
        }
        if m.local_mavlink_port == 0 {
            return Err(ConfigError::Validation(
                "mavlink.local_mavlink_port must be greater than 0".to_string(),
            ));
        }
        if m.remote_mavlink_port == 0 {
            return Err(ConfigError::Validation(
                "mavlink.remote_mavlink_port must be greater than 0".to_string(),
            ));
        }

        // Validate camera config
        let c = &self.camera;
        if c.env_path.is_empty() {
            return Err(ConfigError::Validation(
                "camera.env_path must not be empty".to_string(),
            ));
        }
        if c.gcs_ip.is_empty() || c.gcs_ip.starts_with('-') {
            return Err(ConfigError::Validation(
                "camera.gcs_ip must be a non-empty value that does not start with '-'".to_string(),
            ));
        }
        if c.camera_type.is_empty() {
            return Err(ConfigError::Validation(
                "camera.camera_type must not be empty".to_string(),
            ));
        }
        if c.device.is_empty() {
            return Err(ConfigError::Validation(
                "camera.device must not be empty".to_string(),
            ));
        }
        if c.width == 0 {
            return Err(ConfigError::Validation(
                "camera.width must be greater than 0".to_string(),
            ));
        }
        if c.height == 0 {
            return Err(ConfigError::Validation(
                "camera.height must be greater than 0".to_string(),
            ));
        }
        if c.framerate == 0 {
            return Err(ConfigError::Validation(
                "camera.framerate must be greater than 0".to_string(),
            ));
        }
        if c.bitrate == 0 {
            return Err(ConfigError::Validation(
                "camera.bitrate must be greater than 0".to_string(),
            ));
        }
        if c.remote_video_port == 0 {
            return Err(ConfigError::Validation(
                "camera.remote_video_port must be greater than 0".to_string(),
            ));
        }

        // Validate mavlink.host
        if m.host.is_empty() || m.host.starts_with('-') {
            return Err(ConfigError::Validation(
                "mavlink.host must be a non-empty value that does not start with '-'".to_string(),
            ));
        }

        // Validate string config values don't contain newlines (prevents env variable injection
        // in generated env files, and malformed connection strings)
        for (field, value) in [
            ("mavlink.host", m.host.as_str()),
            ("mavlink.gcs_ip", m.gcs_ip.as_str()),
            ("mavlink.env_path", m.env_path.as_str()),
            ("mavlink.fc.tty", m.fc.tty.as_str()),
            ("camera.gcs_ip", c.gcs_ip.as_str()),
            ("camera.env_path", c.env_path.as_str()),
            ("camera.camera_type", c.camera_type.as_str()),
            ("camera.device", c.device.as_str()),
        ] {
            if value.contains('\n') || value.contains('\r') {
                return Err(ConfigError::Validation(format!(
                    "{field} must not contain newline characters"
                )));
            }
        }

        // Validate general.interface
        if self.general.interface.is_empty() {
            return Err(ConfigError::Validation(
                "general.interface must not be empty".to_string(),
            ));
        }
        if self.general.interface.starts_with('-') {
            return Err(ConfigError::Validation(
                "general.interface must not start with '-'".to_string(),
            ));
        }
        if !self
            .general
            .interface
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(ConfigError::Validation(
                "general.interface must contain only alphanumeric, '.', '_', or '-' characters"
                    .to_string(),
            ));
        }

        // Validate general.env_dir
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

        // Validate optional general TLS cert paths.
        for (field, value) in [
            ("general.ca_cert_path", self.general.ca_cert_path.as_deref()),
            (
                "general.client_cert_path",
                self.general.client_cert_path.as_deref(),
            ),
            (
                "general.client_key_path",
                self.general.client_key_path.as_deref(),
            ),
        ] {
            if let Some(v) = value {
                if v.is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "{field} must not be empty when set"
                    )));
                }
                if v.contains('\n') || v.contains('\r') {
                    return Err(ConfigError::Validation(format!(
                        "{field} must not contain newline characters"
                    )));
                }
            }
        }

        // Validate sensor intervals (must be finite and positive; TOML allows inf/nan)
        if !self.sensors.default_interval_s.is_finite() || self.sensors.default_interval_s <= 0.0 {
            return Err(ConfigError::Validation(
                "sensors.default_interval_s must be a finite positive number".to_string(),
            ));
        }
        if let Some(v) = self.sensors.ping.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err(ConfigError::Validation(
                    "sensors.ping.interval_s must be a finite positive number".to_string(),
                ));
            }
        }
        if let Some(v) = self.sensors.lte.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err(ConfigError::Validation(
                    "sensors.lte.interval_s must be a finite positive number".to_string(),
                ));
            }
        }
        if let Some(v) = self.sensors.system.interval_s {
            if !v.is_finite() || v <= 0.0 {
                return Err(ConfigError::Validation(
                    "sensors.system.interval_s must be a finite positive number".to_string(),
                ));
            }
        }
        if !self.sensors.lte.neighbor_expiry_s.is_finite()
            || self.sensors.lte.neighbor_expiry_s <= 0.0
        {
            return Err(ConfigError::Validation(
                "sensors.lte.neighbor_expiry_s must be a finite positive number".to_string(),
            ));
        }
        if !matches!(self.sensors.lte.modem_type.as_str(), "dbus" | "fake") {
            return Err(ConfigError::Validation(format!(
                "sensors.lte.modem_type must be \"dbus\" or \"fake\", got {:?}",
                self.sensors.lte.modem_type
            )));
        }
        if self.sensors.ping.host.is_empty() || self.sensors.ping.host.starts_with('-') {
            return Err(ConfigError::Validation(
                "sensors.ping.host must be a valid hostname or IP address".to_string(),
            ));
        }
        // Validate MQTT config (only when enabled)
        if self.mqtt.enabled {
            if self.mqtt.host.is_empty() {
                return Err(ConfigError::Validation(
                    "mqtt.host must not be empty when MQTT is enabled".to_string(),
                ));
            }
            if self.mqtt.port == 0 {
                return Err(ConfigError::Validation(
                    "mqtt.port must be greater than 0".to_string(),
                ));
            }
            if self.mqtt.env_prefix.is_empty() {
                return Err(ConfigError::Validation(
                    "mqtt.env_prefix must not be empty when MQTT is enabled".to_string(),
                ));
            }
            if !self.mqtt.telemetry_interval_s.is_finite() || self.mqtt.telemetry_interval_s < 1.0 {
                return Err(ConfigError::Validation(
                    "mqtt.telemetry_interval_s must be a finite number >= 1.0".to_string(),
                ));
            }
        }

        // Validate fluentbit config (only when enabled)
        if self.fluentbit.enabled {
            let f = &self.fluentbit;
            if f.host.is_empty() || f.host.starts_with('-') {
                return Err(ConfigError::Validation(
                    "fluentbit.host must be a non-empty value that does not start with '-'"
                        .to_string(),
                ));
            }
            if f.host.contains('\n') || f.host.contains('\r') {
                return Err(ConfigError::Validation(
                    "fluentbit.host must not contain newline characters".to_string(),
                ));
            }
            if f.tls {
                for (field, v) in [
                    ("general.ca_cert_path", self.general.ca_cert_path.as_deref()),
                    (
                        "general.client_cert_path",
                        self.general.client_cert_path.as_deref(),
                    ),
                    (
                        "general.client_key_path",
                        self.general.client_key_path.as_deref(),
                    ),
                ] {
                    if !matches!(v, Some(s) if !s.is_empty()) {
                        return Err(ConfigError::Validation(format!(
                            "{field} must be set when fluentbit.tls = true"
                        )));
                    }
                }
            }
            if f.port == 0 {
                return Err(ConfigError::Validation(
                    "fluentbit.port must be greater than 0".to_string(),
                ));
            }
            if f.config_path.is_empty() {
                return Err(ConfigError::Validation(
                    "fluentbit.config_path must not be empty".to_string(),
                ));
            }
            if f.config_path.contains('\n') || f.config_path.contains('\r') {
                return Err(ConfigError::Validation(
                    "fluentbit.config_path must not contain newline characters".to_string(),
                ));
            }
            if let Some(filters) = &f.systemd_filter {
                for entry in filters {
                    if entry.contains('\n') || entry.contains('\r') {
                        return Err(ConfigError::Validation(format!(
                            "fluentbit.systemd_filter entry {entry:?} must not contain newline characters"
                        )));
                    }
                    let mut parts = entry.splitn(2, '=');
                    let key = parts.next().unwrap_or("");
                    let value_present = parts.next().is_some();
                    if !value_present {
                        return Err(ConfigError::Validation(format!(
                            "fluentbit.systemd_filter entry {entry:?} must be KEY=VALUE"
                        )));
                    }
                    let key_ok = !key.is_empty()
                        && key
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
                        && key
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                    if !key_ok {
                        return Err(ConfigError::Validation(format!(
                            "fluentbit.systemd_filter entry {entry:?} key must match [A-Z_][A-Z0-9_]*"
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}

#[doc(hidden)]
pub mod tests {
    use super::*;

    pub const FULL_TEST_CONFIG: &str = r#"
[general]
debug = false
interface = "eth0"
env_dir = "/var/run/unitctl"
ca_cert_path = "/etc/unitctl/certs/ca.pem"
client_cert_path = "/etc/unitctl/certs/client.pem"
client_key_path = "/etc/unitctl/certs/client.key"

[mavlink]
protocol = "tcpout"
host = "127.0.0.1"
local_mavlink_port = 5760
remote_mavlink_port = 5760
self_sysid = 1
self_compid = 10
gcs_sysid = 255
gcs_compid = 190
sniffer_sysid = 199
bs_sysid = 200
iteration_period_ms = 10
gcs_ip = "10.101.0.1"
env_path = "/etc/mavlink.env"

[mavlink.fc]
tty = "/dev/ttyFC"
baudrate = 57600

[camera]
gcs_ip = "10.101.0.1"
env_path = "/etc/camera.env"
remote_video_port = 5600
width = 640
height = 360
framerate = 60
bitrate = 1664000
flip = 0
camera_type = "rpi"
device = "/dev/video1"

[sensors]
default_interval_s = 5.0

[sensors.ping]
enabled = true
host = "10.45.0.2"

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0
modem_type = "dbus"

[sensors.system]
enabled = true

[mqtt]
enabled = false
host = "mqtt.example.com"
port = 8883
env_prefix = "test"
telemetry_interval_s = 1.0

[fluentbit]
enabled = false
host = "logs.example.com"
port = 24224
tls = true
tls_verify = true
config_path = "/etc/fluent-bit.conf"
# systemd_filter placeholder
"#;

    pub fn test_config() -> Config {
        toml::from_str(FULL_TEST_CONFIG).unwrap()
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[general]
debug = true
interface = "eth0"
env_dir = "/run/unitctl-alt"

[mavlink]
protocol = "tcpout"
host = "192.168.1.100"
local_mavlink_port = 5761
remote_mavlink_port = 5761
self_sysid = 2
self_compid = 11
gcs_sysid = 254
gcs_compid = 191
sniffer_sysid = 198
bs_sysid = 201
iteration_period_ms = 20
gcs_ip = "10.101.0.2"
env_path = "/tmp/mavlink.env"

[mavlink.fc]
tty = "/dev/ttyS0"
baudrate = 115200

[camera]
gcs_ip = "10.101.0.2"
env_path = "/tmp/camera.env"
remote_video_port = 5601
width = 1280
height = 720
framerate = 30
bitrate = 3000000
flip = 2
camera_type = "usb"
device = "/dev/video0"

[sensors]
default_interval_s = 5.0

[sensors.ping]
enabled = true
host = "10.45.0.2"

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0
modem_type = "dbus"

[sensors.system]
enabled = true

[mqtt]
enabled = false
host = "mqtt.example.com"
port = 8883
env_prefix = "test"
telemetry_interval_s = 1.0

[fluentbit]
enabled = false
host = "logs.example.com"
port = 24224
tls = true
tls_verify = true
config_path = "/etc/fluent-bit.conf"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.general.debug);
        assert_eq!(config.general.env_dir, "/run/unitctl-alt");
        assert_eq!(config.mavlink.protocol, "tcpout");
        assert_eq!(config.mavlink.host, "192.168.1.100");
        assert_eq!(config.mavlink.local_mavlink_port, 5761);
        assert_eq!(config.mavlink.remote_mavlink_port, 5761);
        assert_eq!(config.mavlink.self_sysid, 2);
        assert_eq!(config.mavlink.self_compid, 11);
        assert_eq!(config.mavlink.gcs_sysid, 254);
        assert_eq!(config.mavlink.gcs_compid, 191);
        assert_eq!(config.mavlink.sniffer_sysid, 198);
        assert_eq!(config.mavlink.bs_sysid, 201);
        assert_eq!(config.mavlink.iteration_period_ms, 20);
        assert_eq!(config.mavlink.gcs_ip, "10.101.0.2");
        assert_eq!(config.mavlink.env_path, "/tmp/mavlink.env");
        assert_eq!(config.mavlink.fc.tty, "/dev/ttyS0");
        assert_eq!(config.mavlink.fc.baudrate, 115200);
        assert_eq!(config.camera.gcs_ip, "10.101.0.2");
        assert_eq!(config.camera.env_path, "/tmp/camera.env");
        assert_eq!(config.camera.remote_video_port, 5601);
        assert_eq!(config.camera.width, 1280);
        assert_eq!(config.camera.height, 720);
        assert_eq!(config.camera.framerate, 30);
        assert_eq!(config.camera.bitrate, 3000000);
        assert_eq!(config.camera.flip, 2);
        assert_eq!(config.camera.camera_type, "usb");
        assert_eq!(config.camera.device, "/dev/video0");
    }

    #[test]
    fn test_parse_config_from_constant() {
        let config = test_config();
        assert!(!config.general.debug);
        assert_eq!(config.general.interface, "eth0");
        assert_eq!(config.general.env_dir, "/var/run/unitctl");
        assert_eq!(config.mavlink.protocol, "tcpout");
        assert_eq!(config.mavlink.host, "127.0.0.1");
        assert_eq!(config.mavlink.local_mavlink_port, 5760);
        assert_eq!(config.mavlink.remote_mavlink_port, 5760);
        assert_eq!(config.mavlink.self_sysid, 1);
        assert_eq!(config.mavlink.self_compid, 10);
        assert_eq!(config.mavlink.gcs_sysid, 255);
        assert_eq!(config.mavlink.gcs_compid, 190);
        assert_eq!(config.mavlink.sniffer_sysid, 199);
        assert_eq!(config.mavlink.bs_sysid, 200);
        assert_eq!(config.mavlink.iteration_period_ms, 10);
        assert_eq!(config.mavlink.gcs_ip, "10.101.0.1");
        assert_eq!(config.mavlink.env_path, "/etc/mavlink.env");
        assert_eq!(config.mavlink.fc.tty, "/dev/ttyFC");
        assert_eq!(config.mavlink.fc.baudrate, 57600);
        assert_eq!(config.camera.gcs_ip, "10.101.0.1");
        assert_eq!(config.camera.env_path, "/etc/camera.env");
        assert_eq!(config.camera.remote_video_port, 5600);
        assert_eq!(config.camera.width, 640);
        assert_eq!(config.camera.height, 360);
        assert_eq!(config.camera.framerate, 60);
        assert_eq!(config.camera.bitrate, 1664000);
        assert_eq!(config.camera.flip, 0);
        assert_eq!(config.camera.camera_type, "rpi");
        assert_eq!(config.camera.device, "/dev/video1");
        assert_eq!(config.sensors.default_interval_s, 5.0);
        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, None);
        assert_eq!(config.sensors.ping.host, "10.45.0.2");
        assert!(config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, None);
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 30.0);
        assert!(config.sensors.system.enabled);
        assert_eq!(config.sensors.system.interval_s, None);
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
local_mavlink_port = 5760
remote_mavlink_port = 5760
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
local_mavlink_port = 5760
remote_mavlink_port = 5760
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

[camera]
gcs_ip = "10.101.0.1"
env_path = "/etc/camera.env"
remote_video_port = 5600
width = 640
height = 360
framerate = 60
bitrate = 1664000
flip = 0
camera_type = "rpi"
device = "/dev/video1"

[sensors]
default_interval_s = 1.0

[sensors.ping]
enabled = true
host = "10.45.0.2"

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0
modem_type = "dbus"

[sensors.system]
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
        assert_eq!(config.mavlink.local_mavlink_port, 5760);
        assert_eq!(config.mavlink.remote_mavlink_port, 5760);
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

        assert_eq!(config.sensors.default_interval_s, 5.0);
        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, None);
        assert_eq!(config.sensors.ping.host, "10.45.0.2");
        assert!(config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, None);
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 30.0);
        assert!(config.sensors.system.enabled);
        assert_eq!(config.sensors.system.interval_s, None);
    }

    #[test]
    fn test_sensor_config_full() {
        let toml_str = r#"
[general]
debug = false
interface = "eth0"
env_dir = "/var/run/unitctl"

[mavlink]
protocol = "tcpout"
host = "127.0.0.1"
local_mavlink_port = 5760
remote_mavlink_port = 5760
self_sysid = 1
self_compid = 10
gcs_sysid = 255
gcs_compid = 190
sniffer_sysid = 199
bs_sysid = 200
iteration_period_ms = 10
gcs_ip = "10.101.0.1"
env_path = "/etc/mavlink.env"

[mavlink.fc]
tty = "/dev/ttyFC"
baudrate = 57600

[camera]
gcs_ip = "10.101.0.1"
env_path = "/etc/camera.env"
remote_video_port = 5600
width = 640
height = 360
framerate = 60
bitrate = 1664000
flip = 0
camera_type = "rpi"
device = "/dev/video1"

[sensors]
default_interval_s = 2.0

[sensors.ping]
enabled = true
interval_s = 0.5
host = "192.168.1.1"

[sensors.lte]
enabled = false
interval_s = 3.0
neighbor_expiry_s = 60.0
modem_type = "dbus"

[sensors.system]
enabled = true
interval_s = 10.0

[mqtt]
enabled = false
host = "mqtt.example.com"
port = 8883
env_prefix = "test"
telemetry_interval_s = 1.0

[fluentbit]
enabled = false
host = "logs.example.com"
port = 24224
tls = true
tls_verify = true
config_path = "/etc/fluent-bit.conf"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();

        assert_eq!(config.sensors.default_interval_s, 2.0);

        assert!(config.sensors.ping.enabled);
        assert_eq!(config.sensors.ping.interval_s, Some(0.5));
        assert_eq!(config.sensors.ping.host, "192.168.1.1");

        assert!(!config.sensors.lte.enabled);
        assert_eq!(config.sensors.lte.interval_s, Some(3.0));
        assert_eq!(config.sensors.lte.neighbor_expiry_s, 60.0);

        assert!(config.sensors.system.enabled);
        assert_eq!(config.sensors.system.interval_s, Some(10.0));
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
    fn test_system_sensor_default() {
        let config = test_config();
        assert!(config.sensors.system.enabled);
        assert_eq!(config.sensors.system.interval_s, None);
    }

    #[test]
    fn test_system_sensor_negative_interval_rejected() {
        let mut config = test_config();
        config.sensors.system.interval_s = Some(-1.0);
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
        config.sensors.system.interval_s = Some(10.0);

        assert!(config.validate().is_ok());
        assert_eq!(config.sensors.ping.interval_s, Some(0.5));
        assert_eq!(config.sensors.lte.interval_s, Some(3.0));
        assert_eq!(config.sensors.system.interval_s, Some(10.0));
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
    fn test_dash_prefix_general_interface_rejected() {
        let mut config = test_config();
        config.general.interface = "-evil".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("general.interface"));
    }

    #[test]
    fn test_empty_general_interface_rejected() {
        let mut config = test_config();
        config.general.interface = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("general.interface"));
    }

    #[test]
    fn test_invalid_chars_general_interface_rejected() {
        let mut config = test_config();
        config.general.interface = "eth 0".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("general.interface"));
    }

    #[test]
    fn test_general_interface_roundtrips() {
        let config = test_config();
        assert_eq!(config.general.interface, "eth0");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_general_cert_paths_parsed_when_present() {
        let config = test_config();
        assert_eq!(
            config.general.ca_cert_path.as_deref(),
            Some("/etc/unitctl/certs/ca.pem")
        );
        assert_eq!(
            config.general.client_cert_path.as_deref(),
            Some("/etc/unitctl/certs/client.pem")
        );
        assert_eq!(
            config.general.client_key_path.as_deref(),
            Some("/etc/unitctl/certs/client.key")
        );
    }

    #[test]
    fn test_general_cert_paths_default_to_none_when_absent() {
        let toml_str = FULL_TEST_CONFIG
            .replace("ca_cert_path = \"/etc/unitctl/certs/ca.pem\"\n", "")
            .replace("client_cert_path = \"/etc/unitctl/certs/client.pem\"\n", "")
            .replace("client_key_path = \"/etc/unitctl/certs/client.key\"\n", "");
        let config: Config = toml::from_str(&toml_str).expect("parse without cert paths");
        assert!(config.general.ca_cert_path.is_none());
        assert!(config.general.client_cert_path.is_none());
        assert!(config.general.client_key_path.is_none());
    }

    #[test]
    fn test_validate_rejects_empty_general_ca_cert_path() {
        let mut cfg = test_config();
        cfg.general.ca_cert_path = Some(String::new());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("general.ca_cert_path"));
    }

    #[test]
    fn test_validate_rejects_newline_general_client_cert_path() {
        let mut cfg = test_config();
        cfg.general.client_cert_path = Some("/etc/cert.pem\nEVIL".to_string());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("general.client_cert_path"));
    }

    #[test]
    fn test_validate_rejects_carriage_return_general_client_key_path() {
        let mut cfg = test_config();
        cfg.general.client_key_path = Some("/etc/key.pem\rEVIL".to_string());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("general.client_key_path"));
    }

    #[test]
    fn test_validate_accepts_none_general_cert_paths() {
        let mut cfg = test_config();
        cfg.general.ca_cert_path = None;
        cfg.general.client_cert_path = None;
        cfg.general.client_key_path = None;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_empty_env_dir() {
        let mut cfg = test_config();
        cfg.general.env_dir = String::new();
        let err = cfg.validate().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("general.env_dir"));
    }

    #[test]
    fn test_validate_rejects_newline_env_dir() {
        let mut cfg = test_config();
        cfg.general.env_dir = "/var/run/unitctl\nEVIL=1".to_string();
        let err = cfg.validate().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("general.env_dir"));
    }

    #[test]
    fn test_empty_env_path_rejected() {
        let mut config = test_config();
        config.mavlink.env_path = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("env_path"));
    }

    #[test]
    fn test_empty_gcs_ip_rejected() {
        let mut config = test_config();
        config.mavlink.gcs_ip = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("gcs_ip"));
    }

    #[test]
    fn test_dash_prefix_gcs_ip_rejected() {
        let mut config = test_config();
        config.mavlink.gcs_ip = "-malicious".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("gcs_ip"));
    }

    #[test]
    fn test_camera_config_parsed() {
        let config = test_config();
        assert_eq!(config.camera.gcs_ip, "10.101.0.1");
        assert_eq!(config.camera.env_path, "/etc/camera.env");
        assert_eq!(config.camera.remote_video_port, 5600);
        assert_eq!(config.camera.width, 640);
        assert_eq!(config.camera.height, 360);
        assert_eq!(config.camera.framerate, 60);
        assert_eq!(config.camera.bitrate, 1664000);
        assert_eq!(config.camera.flip, 0);
        assert_eq!(config.camera.camera_type, "rpi");
        assert_eq!(config.camera.device, "/dev/video1");
    }

    #[test]
    fn test_camera_empty_env_path_rejected() {
        let mut config = test_config();
        config.camera.env_path = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.env_path"));
    }

    #[test]
    fn test_camera_empty_gcs_ip_rejected() {
        let mut config = test_config();
        config.camera.gcs_ip = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.gcs_ip"));
    }

    #[test]
    fn test_camera_dash_prefix_gcs_ip_rejected() {
        let mut config = test_config();
        config.camera.gcs_ip = "-bad".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.gcs_ip"));
    }

    #[test]
    fn test_camera_empty_camera_type_rejected() {
        let mut config = test_config();
        config.camera.camera_type = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.camera_type"));
    }

    #[test]
    fn test_camera_empty_device_rejected() {
        let mut config = test_config();
        config.camera.device = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.device"));
    }

    #[test]
    fn test_camera_zero_width_rejected() {
        let mut config = test_config();
        config.camera.width = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.width"));
    }

    #[test]
    fn test_camera_zero_height_rejected() {
        let mut config = test_config();
        config.camera.height = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.height"));
    }

    #[test]
    fn test_camera_zero_framerate_rejected() {
        let mut config = test_config();
        config.camera.framerate = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.framerate"));
    }

    #[test]
    fn test_camera_zero_bitrate_rejected() {
        let mut config = test_config();
        config.camera.bitrate = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.bitrate"));
    }

    #[test]
    fn test_camera_zero_remote_video_port_rejected() {
        let mut config = test_config();
        config.camera.remote_video_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("camera.remote_video_port"));
    }

    #[test]
    fn test_empty_mavlink_host_rejected() {
        let mut config = test_config();
        config.mavlink.host = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.host"));
    }

    #[test]
    fn test_dash_prefix_mavlink_host_rejected() {
        let mut config = test_config();
        config.mavlink.host = "-evil".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.host"));
    }

    #[test]
    fn test_newline_in_gcs_ip_rejected() {
        let mut config = test_config();
        config.mavlink.gcs_ip = "10.0.0.1\nEVIL=yes".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("newline"));
    }

    #[test]
    fn test_newline_in_camera_type_rejected() {
        let mut config = test_config();
        config.camera.camera_type = "rpi\nEVIL=yes".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("newline"));
    }

    #[test]
    fn test_carriage_return_in_device_rejected() {
        let mut config = test_config();
        config.camera.device = "/dev/video0\rEVIL".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("newline"));
    }

    #[test]
    fn test_newline_in_fc_tty_rejected() {
        let mut config = test_config();
        config.mavlink.fc.tty = "/dev/ttyFC\nEVIL=yes".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("newline"));
    }

    #[test]
    fn test_newline_in_mavlink_host_rejected() {
        let mut config = test_config();
        config.mavlink.host = "127.0.0.1\nEVIL".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("newline"));
    }

    #[test]
    fn test_empty_fc_tty_rejected() {
        let mut config = test_config();
        config.mavlink.fc.tty = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.fc.tty"));
    }

    #[test]
    fn test_zero_fc_baudrate_rejected() {
        let mut config = test_config();
        config.mavlink.fc.baudrate = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.fc.baudrate"));
    }

    #[test]
    fn test_zero_local_mavlink_port_rejected() {
        let mut config = test_config();
        config.mavlink.local_mavlink_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.local_mavlink_port"));
    }

    #[test]
    fn test_zero_remote_mavlink_port_rejected() {
        let mut config = test_config();
        config.mavlink.remote_mavlink_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mavlink.remote_mavlink_port"));
    }

    #[test]
    fn test_mqtt_config_parsed() {
        let config = test_config();
        assert!(!config.mqtt.enabled);
        assert_eq!(config.mqtt.host, "mqtt.example.com");
        assert_eq!(config.mqtt.port, 8883);
        assert_eq!(config.mqtt.env_prefix, "test");
        assert_eq!(config.mqtt.telemetry_interval_s, 1.0);
        assert_eq!(
            config.general.ca_cert_path.as_deref(),
            Some("/etc/unitctl/certs/ca.pem")
        );
        assert_eq!(
            config.general.client_cert_path.as_deref(),
            Some("/etc/unitctl/certs/client.pem")
        );
        assert_eq!(
            config.general.client_key_path.as_deref(),
            Some("/etc/unitctl/certs/client.key")
        );
    }

    #[test]
    fn test_mqtt_config_default() {
        let default = MqttConfig::default();
        assert!(!default.enabled);
        assert_eq!(default.host, "mqtt.example.com");
        assert_eq!(default.port, 8883);
        assert_eq!(default.telemetry_interval_s, 5.0);
    }

    #[test]
    fn test_mqtt_disabled_skips_validation() {
        let mut config = test_config();
        config.mqtt.enabled = false;
        config.mqtt.host = "".to_string();
        config.mqtt.port = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_mqtt_enabled_empty_host_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.host = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.host"));
    }

    #[test]
    fn test_mqtt_enabled_zero_port_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.port"));
    }

    #[test]
    fn test_mqtt_enabled_empty_env_prefix_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.env_prefix = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.env_prefix"));
    }

    #[test]
    fn test_mqtt_enabled_zero_telemetry_interval_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.telemetry_interval_s = 0.0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.telemetry_interval_s"));
    }

    #[test]
    fn test_mqtt_enabled_negative_telemetry_interval_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.telemetry_interval_s = -1.0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.telemetry_interval_s"));
    }

    #[test]
    fn test_mqtt_enabled_inf_telemetry_interval_rejected() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.mqtt.telemetry_interval_s = f64::INFINITY;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt.telemetry_interval_s"));
    }

    #[test]
    fn test_mqtt_enabled_valid_config_accepted() {
        let mut config = test_config();
        config.mqtt.enabled = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_mqtt_missing_section_fails() {
        let toml_str = r#"
[general]
debug = false
interface = "eth0"

[mavlink]
protocol = "tcpout"
host = "127.0.0.1"
local_mavlink_port = 5760
remote_mavlink_port = 5760
self_sysid = 1
self_compid = 10
gcs_sysid = 255
gcs_compid = 190
sniffer_sysid = 199
bs_sysid = 200
iteration_period_ms = 10
gcs_ip = "10.101.0.1"
env_path = "/etc/mavlink.env"

[mavlink.fc]
tty = "/dev/ttyFC"
baudrate = 57600

[camera]
gcs_ip = "10.101.0.1"
env_path = "/etc/camera.env"
remote_video_port = 5600
width = 640
height = 360
framerate = 60
bitrate = 1664000
flip = 0
camera_type = "rpi"
device = "/dev/video1"

[sensors]
default_interval_s = 5.0

[sensors.ping]
enabled = true
host = "10.45.0.2"

[sensors.lte]
enabled = true
neighbor_expiry_s = 30.0
modem_type = "dbus"

[sensors.system]
enabled = true
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_lte_modem_type_dbus_accepted() {
        let mut config = test_config();
        config.sensors.lte.modem_type = "dbus".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_lte_modem_type_fake_accepted() {
        let mut config = test_config();
        config.sensors.lte.modem_type = "fake".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_lte_modem_type_empty_rejected() {
        let mut config = test_config();
        config.sensors.lte.modem_type = "".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sensors.lte.modem_type"));
    }

    #[test]
    fn test_lte_modem_type_wrong_case_rejected() {
        let mut config = test_config();
        config.sensors.lte.modem_type = "DBUS".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sensors.lte.modem_type"));
    }

    #[test]
    fn test_lte_modem_type_unknown_rejected() {
        let mut config = test_config();
        config.sensors.lte.modem_type = "serial".to_string();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("sensors.lte.modem_type"));
    }

    #[test]
    fn test_fluentbit_config_parsed() {
        let config = test_config();
        assert!(!config.fluentbit.enabled);
        assert_eq!(config.fluentbit.host, "logs.example.com");
        assert_eq!(config.fluentbit.port, 24224);
        assert!(config.fluentbit.tls);
        assert!(config.fluentbit.tls_verify);
        assert_eq!(config.fluentbit.config_path, "/etc/fluent-bit.conf");
        assert!(config.fluentbit.systemd_filter.is_none());
    }

    #[test]
    fn test_fluentbit_systemd_filter_parsed() {
        let toml_str = FULL_TEST_CONFIG.replace(
            "# systemd_filter placeholder",
            "systemd_filter = [\"_SYSTEMD_UNIT=unitctl.service\", \"_SYSTEMD_UNIT=mavlink.service\"]",
        );
        let config: Config = toml::from_str(&toml_str).unwrap();
        let filter = config.fluentbit.systemd_filter.unwrap();
        assert_eq!(filter.len(), 2);
        assert_eq!(filter[0], "_SYSTEMD_UNIT=unitctl.service");
        assert_eq!(filter[1], "_SYSTEMD_UNIT=mavlink.service");
    }

    #[test]
    fn test_fluentbit_disabled_skips_validation() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = false;
        cfg.fluentbit.host = String::new();
        cfg.fluentbit.port = 0;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_fluentbit_enabled_empty_host_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.host = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.host"));
    }

    #[test]
    fn test_fluentbit_enabled_dash_prefix_host_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.host = "-evil".to_string();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.host"));
    }

    #[test]
    fn test_fluentbit_enabled_zero_port_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.port = 0;
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.port"));
    }

    #[test]
    fn test_fluentbit_enabled_empty_config_path_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.config_path = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.config_path"));
    }

    #[test]
    fn test_fluentbit_enabled_newline_config_path_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.config_path = "/etc/fb.conf\nEVIL".to_string();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.config_path"));
    }

    #[test]
    fn test_fluentbit_systemd_filter_valid_accepted() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.systemd_filter = Some(vec![
            "_SYSTEMD_UNIT=unitctl.service".to_string(),
            "PRIORITY=4".to_string(),
        ]);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_fluentbit_systemd_filter_missing_eq_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.systemd_filter = Some(vec!["_SYSTEMD_UNIT".to_string()]);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.systemd_filter"));
    }

    #[test]
    fn test_fluentbit_systemd_filter_newline_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.systemd_filter = Some(vec!["_FOO=bar\nEVIL=1".to_string()]);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.systemd_filter"));
    }

    #[test]
    fn test_fluentbit_systemd_filter_lowercase_key_rejected() {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.systemd_filter = Some(vec!["bad_key=1".to_string()]);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("fluentbit.systemd_filter"));
    }

    #[test]
    fn test_config_toml_example_parses() {
        let content = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml.example"),
        )
        .expect("read config.toml.example");
        let config: Config = toml::from_str(&content).expect("parse config.toml.example");
        config.validate().expect("validate config.toml.example");
    }
}
