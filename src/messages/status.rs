use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shared envelope carrying a UTC timestamp for node status messages.
/// The `data` field contains a tagged `StatusData` enum, producing JSON like:
/// `{"ts": "...", "data": {"type": "Online", "session": "a8f2c1", "version": "0.1.0"}}`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeStatusEnvelope {
    pub ts: DateTime<Utc>,
    pub data: StatusData,
}

/// Discriminated union for node status — either online or offline.
/// Serializes with a `"type"` tag following the `TelemetryData` pattern.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum StatusData {
    Online(OnlineStatusData),
    Offline(OfflineStatusData),
}

/// Status data published when the node connects to the MQTT broker.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OnlineStatusData {
    /// Short random session ID generated per connection (6-char hex).
    pub session: String,
    /// Application version from Cargo.toml.
    pub version: String,
    /// IPv4 address of the configured network interface, if resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

/// Status data set as MQTT Last Will — published by the broker when the
/// node disconnects unexpectedly.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OfflineStatusData {
    /// Session ID from the last successful connection.
    pub last_session: String,
    /// Timestamp of the last successful connection.
    pub last_online: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap()
    }

    #[test]
    fn round_trip_online_status() {
        let msg = NodeStatusEnvelope {
            ts: sample_ts(),
            data: StatusData::Online(OnlineStatusData {
                session: "a8f2c1".to_string(),
                version: "0.1.0".to_string(),
                ip: Some("192.0.2.1".to_string()),
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: NodeStatusEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ts, msg.ts);
        match parsed.data {
            StatusData::Online(data) => {
                assert_eq!(data.session, "a8f2c1");
                assert_eq!(data.version, "0.1.0");
                assert_eq!(data.ip, Some("192.0.2.1".to_string()));
            }
            _ => panic!("expected Online"),
        }
    }

    #[test]
    fn round_trip_offline_status() {
        let last_online = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let msg = NodeStatusEnvelope {
            ts: sample_ts(),
            data: StatusData::Offline(OfflineStatusData {
                last_session: "a8f2c1".to_string(),
                last_online,
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: NodeStatusEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ts, msg.ts);
        match parsed.data {
            StatusData::Offline(data) => {
                assert_eq!(data.last_session, "a8f2c1");
                assert_eq!(data.last_online, last_online);
            }
            _ => panic!("expected Offline"),
        }
    }

    #[test]
    fn online_json_has_correct_type_tag_and_fields() {
        let msg = NodeStatusEnvelope {
            ts: sample_ts(),
            data: StatusData::Online(OnlineStatusData {
                session: "b3d4e5".to_string(),
                version: "1.2.3".to_string(),
                ip: Some("10.0.0.5".to_string()),
            }),
        };
        let value: serde_json::Value = serde_json::to_value(&msg).unwrap();

        // Root has ts and data
        assert!(value.get("ts").is_some());
        let data = value.get("data").expect("missing data field");

        // Data has type tag
        assert_eq!(data.get("type").unwrap(), "Online");
        // Data has online-specific fields
        assert_eq!(data.get("session").unwrap(), "b3d4e5");
        assert_eq!(data.get("version").unwrap(), "1.2.3");
        assert_eq!(data.get("ip").unwrap(), "10.0.0.5");
        // Data does NOT have offline-specific fields
        assert!(data.get("last_session").is_none());
        assert!(data.get("last_online").is_none());
    }

    #[test]
    fn offline_json_has_correct_type_tag_and_fields() {
        let last_online = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let msg = NodeStatusEnvelope {
            ts: sample_ts(),
            data: StatusData::Offline(OfflineStatusData {
                last_session: "a8f2c1".to_string(),
                last_online,
            }),
        };
        let value: serde_json::Value = serde_json::to_value(&msg).unwrap();

        // Root has ts and data
        assert!(value.get("ts").is_some());
        let data = value.get("data").expect("missing data field");

        // Data has type tag
        assert_eq!(data.get("type").unwrap(), "Offline");
        // Data has offline-specific fields
        assert_eq!(data.get("last_session").unwrap(), "a8f2c1");
        assert!(data.get("last_online").is_some());
        // Data does NOT have online-specific fields
        assert!(data.get("session").is_none());
        assert!(data.get("version").is_none());
    }

    #[test]
    fn json_schema_generation() {
        let schema = schemars::schema_for!(NodeStatusEnvelope);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("StatusData"));
        assert!(json.contains("NodeStatusEnvelope"));
        // Internally tagged enums inline variant fields into oneOf;
        // check for the discriminator values instead of struct names.
        assert!(json.contains("\"Online\""));
        assert!(json.contains("\"Offline\""));
    }

    #[test]
    fn online_status_ip_none_omits_field() {
        let msg = NodeStatusEnvelope {
            ts: sample_ts(),
            data: StatusData::Online(OnlineStatusData {
                session: "d4e5f6".to_string(),
                version: "0.2.0".to_string(),
                ip: None,
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        // The "ip" key should not appear in serialized JSON
        assert!(
            !json.contains("\"ip\""),
            "ip field should be omitted when None"
        );
        // Round-trip still works
        let parsed: NodeStatusEnvelope = serde_json::from_str(&json).unwrap();
        match parsed.data {
            StatusData::Online(data) => {
                assert_eq!(data.ip, None);
            }
            _ => panic!("expected Online"),
        }
    }

    #[test]
    fn json_schema_contains_ip_field() {
        let schema = schemars::schema_for!(NodeStatusEnvelope);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("\"ip\""), "schema should contain ip field");
    }
}
