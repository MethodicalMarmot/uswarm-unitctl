use crate::sensors::lte::LteReading;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shared envelope carrying a UTC timestamp for all telemetry types.
/// The `data` field contains a tagged `TelemetryData` enum, producing JSON like:
/// `{"ts": "...", "data": {"type": "Ping", "reachable": true, ...}}`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TelemetryEnvelope {
    pub ts: DateTime<Utc>,
    pub data: TelemetryData,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TelemetryData {
    Ping(PingTelemetry),
    Lte(LteTelemetry),
    System(SystemTelemetry),
}

/// ICMP ping telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PingTelemetry {
    pub reachable: bool,
    pub latency_ms: f64,
    pub loss_percent: u8,
}

impl Default for PingTelemetry {
    fn default() -> Self {
        Self {
            reachable: false,
            latency_ms: 0.0,
            loss_percent: 100,
        }
    }
}

/// LTE serving-cell signal quality and neighbor cells.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LteTelemetry {
    pub imsi: String,
    pub signal: LteSignalQuality,
    pub neighbors: Vec<LteNeighborCell>,
}

impl From<&LteReading> for LteTelemetry {
    fn from(value: &LteReading) -> LteTelemetry {
        LteTelemetry {
            imsi: value.imsi.clone(),
            signal: value.signal.clone(),
            neighbors: value.neighbors.values().cloned().collect(),
        }
    }
}

/// A single LTE neighbor cell observation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LteNeighborCell {
    pub pcid: i32,
    pub rsrp: i32,
    pub rsrq: i32,
    pub rssi: i32,
    pub rssnr: i32,
    pub earfcn: i32,
    /// Internal bookkeeping for neighbor expiry — not part of the wire format.
    #[serde(skip)]
    pub last_seen: u64,
}

/// LTE signal quality measurements (serving cell only, no neighbors).
///
/// Defined in the messages module and re-exported by `sensors::lte`.
/// Published as part of `LteTelemetry` within `TelemetryData::Lte`.
#[derive(Default, Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LteSignalQuality {
    pub rsrq: i32,
    pub rsrp: i32,
    pub rssi: i32,
    pub rssnr: i32,
    pub earfcn: i32,
    pub tx_power: i32,
    pub pcid: i32,
}

/// Aggregate host telemetry: CPU temp/usage, memory, disks, load, uptime,
/// network interfaces (with bandwidth + addresses), and connected cameras.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SystemTelemetry {
    /// CPU temperature in degrees Celsius. `None` if the sysfs read failed.
    pub cpu_temperature_c: Option<f64>,
    /// Aggregate CPU usage across all cores, 0..100.
    pub cpu_usage_percent: f32,
    pub ram: RamUsage,
    pub disks: Vec<DiskUsage>,
    pub load_avg: LoadAverage,
    pub uptime_s: u64,
    pub network_interfaces: Vec<NetworkInterfaceTelemetry>,
    pub cameras: Vec<CameraInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RamUsage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DiskUsage {
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct NetworkInterfaceTelemetry {
    pub name: String,
    pub ipv4: Vec<String>,
    /// Bits per second since the previous sensor tick.
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CameraInfo {
    pub device: String,
    pub name: Option<String>,
    pub driver: Option<String>,
    pub formats: Vec<CameraFormat>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CameraFormat {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap()
    }

    #[test]
    fn round_trip_ping_telemetry() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Ping(PingTelemetry {
                reachable: true,
                latency_ms: 25.5,
                loss_percent: 3,
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: TelemetryEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ts, msg.ts);
        match parsed.data {
            TelemetryData::Ping(p) => {
                assert!(p.reachable);
                assert!((p.latency_ms - 25.5).abs() < f64::EPSILON);
                assert_eq!(p.loss_percent, 3);
            }
            _ => panic!("expected Ping"),
        }
    }

    #[test]
    fn round_trip_lte_telemetry() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Lte(LteTelemetry {
                imsi: "310260123456789".to_string(),
                signal: LteSignalQuality {
                    rsrp: -85,
                    rsrq: -10,
                    rssi: -60,
                    rssnr: 15,
                    earfcn: 1300,
                    tx_power: 23,
                    pcid: 42,
                },
                neighbors: vec![LteNeighborCell {
                    pcid: 43,
                    rsrp: -90,
                    rsrq: -12,
                    rssi: -65,
                    rssnr: 10,
                    earfcn: 1300,
                    last_seen: 0,
                }],
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: TelemetryEnvelope = serde_json::from_str(&json).unwrap();
        match parsed.data {
            TelemetryData::Lte(lte) => {
                assert_eq!(lte.signal.rsrp, -85);
                assert_eq!(lte.neighbors.len(), 1);
                assert_eq!(lte.neighbors[0].pcid, 43);
            }
            _ => panic!("expected Lte"),
        }
    }

    #[test]
    fn ping_envelope_json_has_typed_data() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::Ping(PingTelemetry {
                reachable: false,
                latency_ms: 0.0,
                loss_percent: 100,
            }),
        };
        let value: serde_json::Value = serde_json::to_value(&msg).unwrap();
        // ts should be at root level
        assert!(value.get("ts").is_some());
        // data should contain the typed telemetry
        assert!(value.get("data").is_some());
    }

    #[test]
    fn from_lte_reading_converts_correctly() {
        use std::collections::HashMap;
        let mut neighbors = HashMap::new();
        neighbors.insert(
            43,
            LteNeighborCell {
                pcid: 43,
                rsrp: -90,
                rsrq: -12,
                rssi: -65,
                rssnr: 10,
                earfcn: 1300,
                last_seen: 1000,
            },
        );
        neighbors.insert(
            44,
            LteNeighborCell {
                pcid: 44,
                rsrp: -95,
                rsrq: -14,
                rssi: -70,
                rssnr: 8,
                earfcn: 1301,
                last_seen: 2000,
            },
        );
        let reading = LteReading {
            imsi: "310260123456789".to_string(),
            signal: LteSignalQuality {
                rsrq: -10,
                rsrp: -85,
                rssi: -60,
                rssnr: 15,
                earfcn: 1300,
                tx_power: 23,
                pcid: 42,
            },
            neighbors,
        };
        let telemetry = LteTelemetry::from(&reading);
        assert_eq!(telemetry.imsi, "310260123456789");
        assert_eq!(telemetry.signal.rsrp, -85);
        assert_eq!(telemetry.signal.tx_power, 23);
        assert_eq!(telemetry.neighbors.len(), 2);
        // Verify all neighbors present (order is non-deterministic)
        let pcids: std::collections::HashSet<i32> =
            telemetry.neighbors.iter().map(|n| n.pcid).collect();
        assert!(pcids.contains(&43));
        assert!(pcids.contains(&44));
    }

    #[test]
    fn json_schema_generation() {
        let schema = schemars::schema_for!(TelemetryEnvelope);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("TelemetryData"));
        assert!(json.contains("TelemetryEnvelope"));
    }

    #[test]
    fn round_trip_system_telemetry() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::System(SystemTelemetry {
                cpu_temperature_c: Some(42.5),
                cpu_usage_percent: 17.25,
                ram: RamUsage {
                    total_bytes: 8_000_000_000,
                    used_bytes: 3_000_000_000,
                    available_bytes: 5_000_000_000,
                },
                disks: vec![DiskUsage {
                    mount_point: "/".to_string(),
                    total_bytes: 100_000_000_000,
                    available_bytes: 60_000_000_000,
                }],
                load_avg: LoadAverage {
                    one: 0.5,
                    five: 0.7,
                    fifteen: 0.9,
                },
                uptime_s: 12345,
                network_interfaces: vec![NetworkInterfaceTelemetry {
                    name: "eth0".to_string(),
                    ipv4: vec!["10.0.0.1".to_string()],
                    rx_bps: 1_000_000,
                    tx_bps: 250_000,
                }],
                cameras: vec![CameraInfo {
                    device: "/dev/video0".to_string(),
                    name: Some("UVC Camera".to_string()),
                    driver: Some("uvcvideo".to_string()),
                    formats: vec![CameraFormat {
                        fourcc: "YUYV".to_string(),
                        width: 640,
                        height: 480,
                    }],
                }],
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: TelemetryEnvelope = serde_json::from_str(&json).unwrap();
        match parsed.data {
            TelemetryData::System(s) => {
                assert_eq!(s.cpu_temperature_c, Some(42.5));
                assert!((s.cpu_usage_percent - 17.25).abs() < f32::EPSILON);
                assert_eq!(s.ram.total_bytes, 8_000_000_000);
                assert_eq!(s.disks.len(), 1);
                assert_eq!(s.disks[0].mount_point, "/");
                assert!((s.load_avg.one - 0.5).abs() < f64::EPSILON);
                assert_eq!(s.uptime_s, 12345);
                assert_eq!(s.network_interfaces.len(), 1);
                assert_eq!(s.network_interfaces[0].name, "eth0");
                assert_eq!(s.network_interfaces[0].rx_bps, 1_000_000);
                assert_eq!(s.cameras.len(), 1);
                assert_eq!(s.cameras[0].device, "/dev/video0");
                assert_eq!(s.cameras[0].formats[0].fourcc, "YUYV");
            }
            _ => panic!("expected System"),
        }
    }

    #[test]
    fn system_envelope_has_type_tag() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::System(SystemTelemetry::default()),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["data"]["type"], "System");
    }
}
