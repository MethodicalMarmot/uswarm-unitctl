use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use crate::sensors::lte::LteReading;

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
    CpuTemp(CpuTempTelemetry),
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
    pub signal: LteSignalQuality,
    pub neighbors: Vec<LteNeighborCell>,
}

impl From<&LteReading> for LteTelemetry {
    fn from(value: &LteReading) -> LteTelemetry {
        LteTelemetry {
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

/// CPU temperature telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CpuTempTelemetry {
    pub temperature_c: f64,
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
    fn round_trip_cpu_temp_telemetry() {
        let msg = TelemetryEnvelope {
            ts: sample_ts(),
            data: TelemetryData::CpuTemp(CpuTempTelemetry {
                temperature_c: 42.5,
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: TelemetryEnvelope = serde_json::from_str(&json).unwrap();
        match parsed.data {
            TelemetryData::CpuTemp(c) => {
                assert!((c.temperature_c - 42.5).abs() < f64::EPSILON);
            }
            _ => panic!("expected CpuTemp"),
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
}
