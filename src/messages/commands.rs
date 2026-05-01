use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{
    CameraConfig, Config, FluentbitConfig, GeneralConfig, MavlinkConfig, MqttConfig, SensorsConfig,
};

// ---------------------------------------------------------------------------
// SafeConfig — mirrors Config with sensitive fields redacted
// ---------------------------------------------------------------------------

/// A sanitized view of the full `Config`, safe for MQTT exposure.
/// TLS certificate paths in `general` are replaced with `"***"`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeConfig {
    pub general: SafeGeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
    pub camera: CameraConfig,
    pub mqtt: SafeMqttConfig,
    pub fluentbit: FluentbitConfig,
}

/// General config with TLS cert paths redacted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeGeneralConfig {
    pub debug: bool,
    pub interface: String,
    pub env_dir: String,
    pub ca_cert_path: Option<String>,
    pub client_cert_path: Option<String>,
    pub client_key_path: Option<String>,
}

/// MQTT config (no secrets — cert paths now live in `general`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeMqttConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub env_prefix: String,
    pub telemetry_interval_s: f64,
}

impl From<&Config> for SafeConfig {
    fn from(config: &Config) -> Self {
        Self {
            general: SafeGeneralConfig::from(&config.general),
            mavlink: config.mavlink.clone(),
            sensors: config.sensors.clone(),
            camera: config.camera.clone(),
            mqtt: SafeMqttConfig::from(&config.mqtt),
            fluentbit: config.fluentbit.clone(),
        }
    }
}

impl From<&GeneralConfig> for SafeGeneralConfig {
    fn from(general: &GeneralConfig) -> Self {
        let redact = |opt: &Option<String>| opt.as_ref().map(|_| "***".to_string());
        Self {
            debug: general.debug,
            interface: general.interface.clone(),
            env_dir: general.env_dir.clone(),
            ca_cert_path: redact(&general.ca_cert_path),
            client_cert_path: redact(&general.client_cert_path),
            client_key_path: redact(&general.client_key_path),
        }
    }
}

impl From<&MqttConfig> for SafeMqttConfig {
    fn from(mqtt: &MqttConfig) -> Self {
        Self {
            enabled: mqtt.enabled,
            host: mqtt.host.clone(),
            port: mqtt.port,
            env_prefix: mqtt.env_prefix.clone(),
            telemetry_interval_s: mqtt.telemetry_interval_s,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared command wrappers
// ---------------------------------------------------------------------------

/// Incoming command envelope — the JSON payload on `.../cmnd/{name}/in`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommandEnvelope {
    pub uuid: String,
    pub issued_at: DateTime<Utc>,
    pub ttl_sec: u64,
    pub payload: CommandPayload,
}

impl CommandEnvelope {
    /// Check if this command has expired.
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    /// Check if this command has expired relative to a given timestamp.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        // Cap TTL to avoid chrono::Duration::seconds() panic on overflow
        // (chrono stores nanoseconds internally, so i64::MAX seconds overflows).
        // 10 years is a generous upper bound for any command TTL.
        const MAX_TTL_SEC: i64 = 315_360_000;
        let ttl = i64::try_from(self.ttl_sec)
            .unwrap_or(MAX_TTL_SEC)
            .min(MAX_TTL_SEC);
        let expiry = self.issued_at + chrono::Duration::seconds(ttl);
        now > expiry
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CommandPayload {
    GetConfig(GetConfigPayload),
    ConfigUpdate(ConfigUpdatePayload),
    ModemCommands(ModemCommandPayload),
    UpdateRequest(UpdateRequestPayload),
    Restart(RestartPayload),
}

/// Status update published on `.../cmnd/{name}/status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommandStatus {
    pub uuid: String,
    pub state: CommandState,
    pub ts: DateTime<Utc>,
}

/// Command lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Accepted,
    InProgress,
    Completed,
    Failed,
    Rejected,
    Expired,
    Superseded,
}

/// Command result published on `.../cmnd/{name}/result`.
/// The `data` field contains a tagged `CommandResultData` enum (omitted when None).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommandResultMsg {
    pub uuid: String,
    pub ok: bool,
    pub ts: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<CommandResultData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CommandResultData {
    GetConfig(Box<GetConfigResult>),
    ConfigUpdate(ConfigUpdateResult),
    ModemCommands(ModemCommandResult),
    UpdateRequest(UpdateRequestResult),
    Restart(RestartResult),
}

// ---------------------------------------------------------------------------
// Per-command payload / result types
// ---------------------------------------------------------------------------

/// Payload for `get_config` — no fields needed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetConfigPayload {}

/// Result for `get_config` — returns the sanitized configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetConfigResult {
    pub config: SafeConfig,
}

/// Payload for `config_update` (placeholder).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigUpdatePayload {
    pub payload: serde_json::Value,
}

/// Result for `config_update` (placeholder).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigUpdateResult {
    pub message: String,
    pub fields_received: Vec<String>,
}

/// Payload for `update_request` (placeholder).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRequestPayload {
    pub version: String,
    pub url: String,
}

/// Result for `update_request` (placeholder).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRequestResult {
    pub message: String,
    pub version: String,
}

/// Payload for `modem_commands`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModemCommandPayload {
    pub command: String,
    pub timeout_ms: Option<u32>,
}

/// Result for `modem_commands`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModemCommandResult {
    pub command: String,
    pub response: String,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap()
    }

    // -----------------------------------------------------------------------
    // SafeConfig
    // -----------------------------------------------------------------------

    #[test]
    fn safe_config_redacts_cert_paths() {
        use crate::config::tests::test_config;
        let config = test_config();
        let safe = SafeConfig::from(&config);

        assert_eq!(safe.general.ca_cert_path.as_deref(), Some("***"));
        assert_eq!(safe.general.client_cert_path.as_deref(), Some("***"));
        assert_eq!(safe.general.client_key_path.as_deref(), Some("***"));
        // Non-sensitive fields preserved
        assert_eq!(safe.mqtt.host, config.mqtt.host);
        assert_eq!(safe.mqtt.port, config.mqtt.port);
        assert_eq!(safe.mqtt.enabled, config.mqtt.enabled);
    }

    #[test]
    fn safe_config_round_trip() {
        use crate::config::tests::test_config;
        let safe = SafeConfig::from(&test_config());
        let json = serde_json::to_string(&safe).unwrap();
        let parsed: SafeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.general.ca_cert_path.as_deref(), Some("***"));
        assert_eq!(parsed.general.debug, safe.general.debug);
    }

    #[test]
    fn test_safe_config_passes_through_none_cert_paths() {
        use crate::config::tests::test_config;
        let mut cfg = test_config();
        cfg.general.ca_cert_path = None;
        cfg.general.client_cert_path = None;
        cfg.general.client_key_path = None;
        let safe: SafeConfig = (&cfg).into();
        assert!(safe.general.ca_cert_path.is_none());
        assert!(safe.general.client_cert_path.is_none());
        assert!(safe.general.client_key_path.is_none());
    }

    #[test]
    fn test_safe_config_includes_fluentbit_unredacted() {
        use crate::config::tests::test_config;
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.fluentbit.host = "central.example.com".to_string();
        let safe: SafeConfig = (&cfg).into();
        assert!(safe.fluentbit.enabled);
        assert_eq!(safe.fluentbit.host, "central.example.com");
    }

    // -----------------------------------------------------------------------
    // CommandEnvelope
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_command_envelope_get_config() {
        let env = CommandEnvelope {
            uuid: "abc-123".to_string(),
            issued_at: sample_ts(),
            ttl_sec: 60,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed: CommandEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.uuid, "abc-123");
        assert_eq!(parsed.ttl_sec, 60);
    }

    #[test]
    fn round_trip_command_envelope_modem() {
        let env = CommandEnvelope {
            uuid: "modem-1".to_string(),
            issued_at: sample_ts(),
            ttl_sec: 30,
            payload: CommandPayload::ModemCommands(ModemCommandPayload {
                command: "ATI".to_string(),
                timeout_ms: Some(5000),
            }),
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed: CommandEnvelope = serde_json::from_str(&json).unwrap();
        match parsed.payload {
            CommandPayload::ModemCommands(ref p) => {
                assert_eq!(p.command, "ATI");
                assert_eq!(p.timeout_ms, Some(5000));
            }
            _ => panic!("expected ModemCommands payload"),
        }
    }

    // -----------------------------------------------------------------------
    // CommandEnvelope TTL expiry
    // -----------------------------------------------------------------------

    #[test]
    fn envelope_not_expired() {
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        assert!(!env.is_expired());
    }

    #[test]
    fn envelope_expired() {
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
            ttl_sec: 60,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        assert!(env.is_expired());
    }

    #[test]
    fn envelope_expired_at_specific_time() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: issued,
            ttl_sec: 60,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        // 30 seconds later — not expired
        let t1 = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 30).unwrap();
        assert!(!env.is_expired_at(t1));
        // 61 seconds later — expired
        let t2 = Utc.with_ymd_and_hms(2026, 3, 23, 10, 1, 1).unwrap();
        assert!(env.is_expired_at(t2));
    }

    #[test]
    fn envelope_max_ttl_does_not_panic() {
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: Utc::now(),
            ttl_sec: u64::MAX,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        // Should not panic from chrono overflow; treated as very long TTL
        assert!(!env.is_expired());
    }

    #[test]
    fn envelope_zero_ttl_expires_immediately() {
        let issued = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let env = CommandEnvelope {
            uuid: "test".to_string(),
            issued_at: issued,
            ttl_sec: 0,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        };
        let later = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 1).unwrap();
        assert!(env.is_expired_at(later));
    }

    // -----------------------------------------------------------------------
    // CommandStatus
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_command_status() {
        let status = CommandStatus {
            uuid: "s-1".to_string(),
            state: CommandState::InProgress,
            ts: sample_ts(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: CommandStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.state, CommandState::InProgress);
    }

    #[test]
    fn command_state_serde_snake_case() {
        let json = serde_json::to_value(CommandState::InProgress).unwrap();
        assert_eq!(json, "in_progress");
        let parsed: CommandState = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, CommandState::InProgress);

        let json = serde_json::to_value(CommandState::Superseded).unwrap();
        assert_eq!(json, "superseded");
        let parsed: CommandState = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, CommandState::Superseded);
    }

    // -----------------------------------------------------------------------
    // CommandResultMsg
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_command_result_get_config() {
        use crate::config::tests::test_config;
        let result = CommandResultMsg {
            uuid: "r-1".to_string(),
            ok: true,
            ts: sample_ts(),
            error: None,
            data: Some(CommandResultData::GetConfig(Box::new(GetConfigResult {
                config: SafeConfig::from(&test_config()),
            }))),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: CommandResultMsg = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
        match parsed.data.unwrap() {
            CommandResultData::GetConfig(ref r) => {
                assert_eq!(r.config.general.ca_cert_path.as_deref(), Some("***"));
            }
            _ => panic!("expected GetConfig"),
        }
    }

    #[test]
    fn command_result_msg_has_typed_data() {
        let result = CommandResultMsg {
            uuid: "r-2".to_string(),
            ok: true,
            ts: sample_ts(),
            error: None,
            data: Some(CommandResultData::ModemCommands(ModemCommandResult {
                command: "ATI".to_string(),
                response: "OK".to_string(),
            })),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: CommandResultMsg = serde_json::from_str(&json).unwrap();
        match parsed.data.unwrap() {
            CommandResultData::ModemCommands(r) => {
                assert_eq!(r.command, "ATI");
                assert_eq!(r.response, "OK");
            }
            _ => panic!("expected ModemCommands"),
        }
    }

    #[test]
    fn command_result_msg_with_error() {
        let result = CommandResultMsg {
            uuid: "r-3".to_string(),
            ok: false,
            ts: sample_ts(),
            error: Some("modem not available".to_string()),
            data: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("modem not available"));
        let parsed: CommandResultMsg = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("modem not available"));
        assert!(parsed.data.is_none());
    }

    #[test]
    fn command_result_msg_error_omitted_when_none() {
        let result = CommandResultMsg {
            uuid: "r-4".to_string(),
            ok: true,
            ts: sample_ts(),
            error: None,
            data: Some(CommandResultData::ModemCommands(ModemCommandResult {
                command: "ATI".to_string(),
                response: "OK".to_string(),
            })),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("\"error\""));
    }

    // -----------------------------------------------------------------------
    // Per-command payload/result round trips
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_config_update() {
        let payload = ConfigUpdatePayload {
            payload: serde_json::json!({"sensors": {"ping": {"enabled": false}}}),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: ConfigUpdatePayload = serde_json::from_str(&json).unwrap();
        assert!(parsed.payload.is_object());

        let result = ConfigUpdateResult {
            message: "Config updated".to_string(),
            fields_received: vec!["sensors.ping.enabled".to_string()],
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ConfigUpdateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fields_received.len(), 1);
    }

    #[test]
    fn round_trip_update_request() {
        let payload = UpdateRequestPayload {
            version: "1.2.3".to_string(),
            url: "https://example.com/update.tar.gz".to_string(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: UpdateRequestPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "1.2.3");

        let result = UpdateRequestResult {
            message: "Update queued".to_string(),
            version: "1.2.3".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: UpdateRequestResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "1.2.3");
    }

    #[test]
    fn round_trip_modem_command() {
        let payload = ModemCommandPayload {
            command: "AT+CSQ".to_string(),
            timeout_ms: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: ModemCommandPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.command, "AT+CSQ");
        assert!(parsed.timeout_ms.is_none());

        let result = ModemCommandResult {
            command: "AT+CSQ".to_string(),
            response: "+CSQ: 15,99".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ModemCommandResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.response, "+CSQ: 15,99");
    }

    // -----------------------------------------------------------------------
    // JSON Schema generation
    // -----------------------------------------------------------------------

    #[test]
    fn json_schema_generation() {
        // Verify schema generation works for all command types
        let schema = schemars::schema_for!(CommandEnvelope);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("CommandPayload"));

        let schema = schemars::schema_for!(CommandStatus);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("CommandState"));

        let schema = schemars::schema_for!(CommandResultMsg);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("CommandResultData"));

        let schema = schemars::schema_for!(RestartPayload);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("RestartTarget"));
    }

    // -----------------------------------------------------------------------
    // Restart command
    // -----------------------------------------------------------------------

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
}
