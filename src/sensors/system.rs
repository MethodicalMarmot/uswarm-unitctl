//! System-wide telemetry sensor: CPU temp/usage, memory, disks, load,
//! uptime, network bandwidth/IPs, and V4L2-probed cameras.

use std::time::Duration;

/// Compute outbound bits-per-second from a byte counter delta.
///
/// `prev_bytes` and `curr_bytes` are cumulative interface byte counters
/// (rx or tx). Returns 0 if `elapsed` is non-positive, the counter wrapped
/// or was reset (curr < prev), or `prev_bytes` is `None` (first tick).
pub fn compute_bps(prev_bytes: Option<u64>, curr_bytes: u64, elapsed: Duration) -> u64 {
    let Some(prev) = prev_bytes else {
        return 0;
    };
    if curr_bytes < prev {
        return 0;
    }
    let elapsed_s = elapsed.as_secs_f64();
    if elapsed_s <= 0.0 {
        return 0;
    }
    let delta_bytes = curr_bytes - prev;
    let bps = (delta_bytes as f64 * 8.0 / elapsed_s).round();
    if bps < 0.0 || !bps.is_finite() {
        0
    } else if bps > u64::MAX as f64 {
        u64::MAX
    } else {
        bps as u64
    }
}

/// Returns true for interface names we always skip (loopback).
pub fn is_loopback(name: &str) -> bool {
    name == "lo"
}

use crate::messages::telemetry::{CameraFormat, CameraInfo};

/// List `/dev/video*` device paths via glob.
pub fn enumerate_video_devices() -> Vec<String> {
    glob::glob("/dev/video*")
        .expect("static glob pattern compiles")
        .filter_map(|res| res.ok())
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .collect()
}

/// Probe a single V4L2 device for capability + supported formats.
/// Returns `None` if the device cannot be opened or queried (busy,
/// permission-denied, missing kernel module, etc.). Frame size enumeration
/// only emits *discrete* sizes (stepwise/continuous ranges are skipped).
pub fn probe_camera(device_path: &str) -> Option<CameraInfo> {
    use v4l::framesize::FrameSizeEnum;
    use v4l::video::Capture;
    use v4l::Device;

    let dev = Device::with_path(device_path).ok()?;
    let caps = dev.query_caps().ok()?;

    let mut formats: Vec<CameraFormat> = Vec::new();
    for fmt in dev.enum_formats().ok()? {
        let fourcc = std::str::from_utf8(&fmt.fourcc.repr)
            .map(|s| s.trim_end_matches('\0').to_string())
            .unwrap_or_else(|_| format!("{:?}", fmt.fourcc.repr));

        let Ok(framesizes) = dev.enum_framesizes(fmt.fourcc) else {
            continue;
        };
        for fs in framesizes {
            if let FrameSizeEnum::Discrete(d) = fs.size {
                formats.push(CameraFormat {
                    fourcc: fourcc.clone(),
                    width: d.width,
                    height: d.height,
                });
            }
        }
    }

    Some(CameraInfo {
        device: device_path.to_string(),
        name: Some(caps.card),
        driver: Some(caps.driver),
        formats,
    })
}

const DEFAULT_THERMAL_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";

/// Read the CPU temperature from a sysfs thermal zone file.
/// File contains millidegrees Celsius as an integer (e.g. "42500" = 42.5°C).
pub fn read_temperature(path: &str) -> Result<f64, TempReadError> {
    let content = std::fs::read_to_string(path).map_err(TempReadError::Io)?;
    parse_temperature(&content)
}

/// Parse a temperature string from sysfs (millidegrees Celsius).
pub fn parse_temperature(content: &str) -> Result<f64, TempReadError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(TempReadError::Parse("empty file".to_string()));
    }
    let millidegrees: i64 = trimmed
        .parse()
        .map_err(|e| TempReadError::Parse(format!("invalid integer: {}", e)))?;
    Ok(millidegrees as f64 / 1000.0)
}

#[derive(Debug, thiserror::Error)]
pub enum TempReadError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use sysinfo::{Disks, Networks, System, MINIMUM_CPU_UPDATE_INTERVAL};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::config::SystemSensorConfig;
use crate::context::Context;
use crate::messages::telemetry::{
    DiskUsage, LoadAverage, NetworkInterfaceTelemetry, RamUsage, SystemTelemetry,
};
use crate::net;

use super::Sensor;

/// SystemSensor — gathers host telemetry once per interval.
///
/// Sysinfo state is held across ticks because deltas (CPU usage,
/// network bandwidth) are only meaningful when computed against the
/// previous tick's snapshot.
pub struct SystemSensor {
    interval: Duration,
    thermal_path: String,
    state: Mutex<SensorState>,
    /// Once the thermal sysfs path has failed to read, downgrade subsequent
    /// failures to debug-level so we don't flood logs every interval.
    thermal_warned: AtomicBool,
}

struct SensorState {
    sys: System,
    disks: Disks,
    networks: Networks,
    /// Per-iface `(rx_bytes, tx_bytes)` at the previous tick.
    prev_net_bytes: HashMap<String, (u64, u64)>,
    prev_tick_at: Option<Instant>,
}

impl SystemSensor {
    pub fn new(config: &SystemSensorConfig, default_interval: f64) -> Self {
        let interval_s = config.interval_s.unwrap_or(default_interval);
        Self {
            interval: Duration::from_secs_f64(interval_s),
            thermal_path: DEFAULT_THERMAL_PATH.to_string(),
            state: Mutex::new(SensorState {
                sys: System::new(),
                disks: Disks::new_with_refreshed_list(),
                networks: Networks::new_with_refreshed_list(),
                prev_net_bytes: HashMap::new(),
                prev_tick_at: None,
            }),
            thermal_warned: AtomicBool::new(false),
        }
    }

    async fn sample(&self) -> SystemTelemetry {
        let mut st = self.state.lock().await;
        let first_tick = st.prev_tick_at.is_none();

        // sysinfo's first refresh_cpu_usage() reading is meaningless: the
        // value is computed as a delta against the previous refresh, and on
        // a freshly-built System there is none. Prime it on the first tick
        // by refreshing, sleeping the documented minimum, then refreshing
        // again so the published reading is real.
        if first_tick {
            st.sys.refresh_cpu_usage();
            sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
        }

        // Record the tick timestamp after the priming sleep so the next
        // tick's network-rate denominator matches the interval over which
        // prev_net_bytes were captured (set further below).
        let now = Instant::now();
        let elapsed = st
            .prev_tick_at
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default();
        st.prev_tick_at = Some(now);
        st.sys.refresh_cpu_usage();
        st.sys.refresh_memory();
        let cpu_usage_percent = st.sys.global_cpu_usage();
        let total_memory = st.sys.total_memory();
        let used_memory = st.sys.used_memory();
        let available_memory = st.sys.available_memory();

        st.disks.refresh_list();
        let disks: Vec<DiskUsage> = st
            .disks
            .iter()
            .map(|d| DiskUsage {
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                total_bytes: d.total_space(),
                available_bytes: d.available_space(),
            })
            .collect();

        let la = System::load_average();
        let load_avg = LoadAverage {
            one: la.one,
            five: la.five,
            fifteen: la.fifteen,
        };

        let uptime_s = System::uptime();

        // refresh_list rather than refresh, so interfaces that come up after
        // startup (e.g. modem PPP, USB tethers) are picked up.
        st.networks.refresh_list();
        let ipv4_by_iface = net::ipv4_map_for_all_interfaces();
        let mut ifaces: Vec<NetworkInterfaceTelemetry> = Vec::new();
        for (name, data) in st.networks.iter() {
            if is_loopback(name) {
                continue;
            }
            let prev = st.prev_net_bytes.get(name).copied();
            let curr_rx = data.total_received();
            let curr_tx = data.total_transmitted();
            let rx_bps = compute_bps(prev.map(|(rx, _)| rx), curr_rx, elapsed);
            let tx_bps = compute_bps(prev.map(|(_, tx)| tx), curr_tx, elapsed);
            ifaces.push(NetworkInterfaceTelemetry {
                name: name.clone(),
                ipv4: ipv4_by_iface.get(name).cloned().unwrap_or_default(),
                rx_bps,
                tx_bps,
            });
        }
        st.prev_net_bytes = st
            .networks
            .iter()
            .map(|(n, d)| (n.clone(), (d.total_received(), d.total_transmitted())))
            .collect();

        let cameras = enumerate_video_devices()
            .into_iter()
            .filter_map(|p| {
                let probed = probe_camera(&p);
                if probed.is_none() {
                    debug!(device = %p, "camera probe failed (busy/permission/missing)");
                }
                probed
            })
            .collect();

        let cpu_temperature_c = match read_temperature(&self.thermal_path) {
            Ok(t) => Some(t),
            Err(e) => {
                if self.thermal_warned.swap(true, Ordering::Relaxed) {
                    debug!(error = %e, path = %self.thermal_path, "failed to read cpu temperature");
                } else {
                    warn!(error = %e, path = %self.thermal_path, "failed to read cpu temperature (further failures will log at debug)");
                }
                None
            }
        };

        SystemTelemetry {
            cpu_temperature_c,
            cpu_usage_percent,
            ram: RamUsage {
                total_bytes: total_memory,
                used_bytes: used_memory,
                available_bytes: available_memory,
            },
            disks,
            load_avg,
            uptime_s,
            network_interfaces: ifaces,
            cameras,
        }
    }
}

#[async_trait]
impl Sensor for SystemSensor {
    fn name(&self) -> &str {
        "system"
    }

    async fn run(&self, ctx: Arc<Context>, cancel: CancellationToken) {
        loop {
            let snapshot = self.sample().await;
            *ctx.sensors.system.write().await = Some(snapshot);

            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sleep(self.interval) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bps_first_tick_is_zero() {
        assert_eq!(compute_bps(None, 1_000_000, Duration::from_secs(1)), 0);
    }

    #[test]
    fn bps_normal_case() {
        assert_eq!(
            compute_bps(Some(0), 125_000, Duration::from_secs(1)),
            1_000_000
        );
    }

    #[test]
    fn bps_counter_wrap_is_zero() {
        assert_eq!(compute_bps(Some(2_000_000), 100, Duration::from_secs(1)), 0);
    }

    #[test]
    fn bps_zero_elapsed_is_zero() {
        assert_eq!(compute_bps(Some(0), 1_000_000, Duration::ZERO), 0);
    }

    #[test]
    fn bps_sub_second_elapsed() {
        assert_eq!(
            compute_bps(Some(0), 12_500, Duration::from_millis(100)),
            1_000_000
        );
    }

    #[test]
    fn loopback_filter() {
        assert!(is_loopback("lo"));
        assert!(!is_loopback("eth0"));
        assert!(!is_loopback("wlan0"));
        assert!(!is_loopback("lo:1"));
    }

    #[test]
    fn enumerate_video_devices_paths_are_under_dev() {
        for p in enumerate_video_devices() {
            assert!(p.starts_with("/dev/video"), "unexpected path: {p}");
        }
    }

    #[test]
    fn probe_missing_camera_returns_none() {
        assert!(probe_camera("/dev/this-does-not-exist").is_none());
    }

    #[test]
    fn parse_normal_temperature() {
        assert!((parse_temperature("42500\n").unwrap() - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_negative_temperature() {
        assert!((parse_temperature("-5000\n").unwrap() - (-5.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_empty_rejected() {
        let err = parse_temperature("").unwrap_err();
        assert!(matches!(err, TempReadError::Parse(_)));
    }

    #[test]
    fn parse_garbage_rejected() {
        let err = parse_temperature("garbage\n").unwrap_err();
        assert!(matches!(err, TempReadError::Parse(_)));
    }

    #[test]
    fn read_missing_path() {
        assert!(read_temperature("/nonexistent/thermal").is_err());
    }

    #[test]
    fn system_sensor_name_is_system() {
        let cfg = crate::config::SystemSensorConfig::default();
        let sensor = SystemSensor::new(&cfg, 5.0);
        assert_eq!(sensor.name(), "system");
    }

    #[tokio::test]
    async fn system_sensor_writes_snapshot_to_context() {
        let cfg = crate::config::SystemSensorConfig {
            enabled: true,
            interval_s: Some(0.05),
        };
        let sensor = SystemSensor::new(&cfg, 1.0);
        let ctx = crate::context::Context::new(crate::config::tests::test_config());
        let cancel = CancellationToken::new();

        let ctx_clone = std::sync::Arc::clone(&ctx);
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            sensor.run(ctx_clone, cancel_clone).await;
        });

        // First sample primes sysinfo CPU usage by sleeping
        // MINIMUM_CPU_UPDATE_INTERVAL (200ms on Linux), so wait longer than that.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let stored = ctx.sensors.system.read().await;
        assert!(stored.is_some(), "sensor should have populated a snapshot");

        cancel.cancel();
        let _ = handle.await;
    }
}
