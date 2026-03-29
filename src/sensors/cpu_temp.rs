use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::config::CpuTempSensorConfig;
use crate::context::Context;
use crate::messages::telemetry::CpuTempTelemetry;

use super::Sensor;

const DEFAULT_THERMAL_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";

/// CPU temperature sensor that reads from sysfs thermal zone.
pub struct CpuTempSensor {
    interval: Duration,
    thermal_path: String,
}

impl CpuTempSensor {
    pub fn new(config: &CpuTempSensorConfig, default_interval: f64) -> Self {
        let interval_s = config.interval_s.unwrap_or(default_interval);
        Self {
            interval: Duration::from_secs_f64(interval_s),
            thermal_path: DEFAULT_THERMAL_PATH.to_string(),
        }
    }

    #[cfg(test)]
    fn with_path(config: &CpuTempSensorConfig, default_interval: f64, path: String) -> Self {
        let interval_s = config.interval_s.unwrap_or(default_interval);
        Self {
            interval: Duration::from_secs_f64(interval_s),
            thermal_path: path,
        }
    }
}

#[async_trait]
impl Sensor for CpuTempSensor {
    fn name(&self) -> &str {
        "cpu_temp"
    }

    async fn run(&self, ctx: Arc<Context>, cancel: CancellationToken) {
        loop {
            match read_temperature(&self.thermal_path) {
                Ok(temperature_c) => {
                    debug!(temperature_c, "cpu temperature reading");
                    *ctx.sensors.cpu_temp.write().await = Some(CpuTempTelemetry { temperature_c });
                }
                Err(e) => {
                    warn!(error = %e, path = %self.thermal_path, "failed to read cpu temperature");
                }
            }

            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(self.interval) => {}
            }
        }
    }
}

/// Read temperature from a sysfs thermal zone file.
/// The file contains millidegrees Celsius as an integer (e.g., "42500" = 42.5 C).
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- Temperature parsing tests ---

    #[test]
    fn test_parse_normal_temperature() {
        let result = parse_temperature("42500\n").unwrap();
        assert!((result - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_zero_temperature() {
        let result = parse_temperature("0\n").unwrap();
        assert!((result - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_high_temperature() {
        let result = parse_temperature("85000\n").unwrap();
        assert!((result - 85.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_negative_temperature() {
        let result = parse_temperature("-5000\n").unwrap();
        assert!((result - (-5.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_fractional_temperature() {
        // 47123 millidegrees = 47.123 degrees
        let result = parse_temperature("47123\n").unwrap();
        assert!((result - 47.123).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_no_trailing_newline() {
        let result = parse_temperature("42500").unwrap();
        assert!((result - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_extra_whitespace() {
        let result = parse_temperature("  42500  \n").unwrap();
        assert!((result - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_temperature("");
        assert!(result.is_err());
        match result.unwrap_err() {
            TempReadError::Parse(msg) => assert!(msg.contains("empty")),
            _ => panic!("expected Parse error"),
        }
    }

    #[test]
    fn test_parse_non_numeric() {
        let result = parse_temperature("not_a_number\n");
        assert!(result.is_err());
        match result.unwrap_err() {
            TempReadError::Parse(msg) => assert!(msg.contains("invalid integer")),
            _ => panic!("expected Parse error"),
        }
    }

    #[test]
    fn test_parse_float_input() {
        // sysfs should always be integer, but handle gracefully
        let result = parse_temperature("42.5\n");
        assert!(result.is_err());
    }

    // --- File read tests ---

    #[test]
    fn test_read_missing_file() {
        let result = read_temperature("/nonexistent/path/temp");
        assert!(result.is_err());
        match result.unwrap_err() {
            TempReadError::Io(_) => {}
            _ => panic!("expected Io error"),
        }
    }

    #[test]
    fn test_read_valid_file() {
        let dir = std::env::temp_dir().join("unitctl_cpu_temp_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("temp");
        std::fs::write(&path, "55000\n").unwrap();

        let result = read_temperature(path.to_str().unwrap()).unwrap();
        assert!((result - 55.0).abs() < f64::EPSILON);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_empty_file() {
        let dir = std::env::temp_dir().join("unitctl_cpu_temp_empty_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("temp");
        std::fs::write(&path, "").unwrap();

        let result = read_temperature(path.to_str().unwrap());
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_invalid_content() {
        let dir = std::env::temp_dir().join("unitctl_cpu_temp_invalid_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("temp");
        std::fs::write(&path, "garbage\n").unwrap();

        let result = read_temperature(path.to_str().unwrap());
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- CpuTempSensor construction tests ---

    #[test]
    fn test_cpu_temp_sensor_new_with_defaults() {
        let config = CpuTempSensorConfig::default();
        let sensor = CpuTempSensor::new(&config, 5.0);
        assert_eq!(sensor.name(), "cpu_temp");
        assert_eq!(sensor.interval, Duration::from_secs_f64(5.0));
        assert_eq!(sensor.thermal_path, DEFAULT_THERMAL_PATH);
    }

    #[test]
    fn test_cpu_temp_sensor_new_with_interval_override() {
        let config = CpuTempSensorConfig {
            enabled: true,
            interval_s: Some(2.0),
        };
        let sensor = CpuTempSensor::new(&config, 5.0);
        assert_eq!(sensor.interval, Duration::from_secs_f64(2.0));
    }

    #[test]
    fn test_cpu_temp_sensor_with_custom_path() {
        let config = CpuTempSensorConfig::default();
        let sensor = CpuTempSensor::with_path(&config, 1.0, "/custom/path".to_string());
        assert_eq!(sensor.thermal_path, "/custom/path");
    }

    // --- Sensor run integration test ---

    #[tokio::test]
    async fn test_cpu_temp_sensor_reads_from_file() {
        let dir = std::env::temp_dir().join("unitctl_cpu_temp_run_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("temp");
        std::fs::write(&path, "65000\n").unwrap();

        let config = CpuTempSensorConfig {
            enabled: true,
            interval_s: Some(0.1),
        };
        let sensor = CpuTempSensor::with_path(&config, 1.0, path.to_str().unwrap().to_string());

        let ctx = crate::context::Context::new(crate::config::tests::test_config());
        let cancel = CancellationToken::new();

        let ctx_clone = Arc::clone(&ctx);
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            sensor.run(ctx_clone, cancel_clone).await;
        });

        // Give the sensor time to read
        tokio::time::sleep(Duration::from_millis(200)).await;

        let reading = ctx.sensors.cpu_temp.read().await;
        assert!(reading.is_some());
        let reading = reading.as_ref().unwrap();
        assert!((reading.temperature_c - 65.0).abs() < f64::EPSILON);

        cancel.cancel();
        let _ = handle.await;

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn test_cpu_temp_sensor_handles_missing_file() {
        let config = CpuTempSensorConfig {
            enabled: true,
            interval_s: Some(0.1),
        };
        let sensor =
            CpuTempSensor::with_path(&config, 1.0, "/nonexistent/thermal/temp".to_string());

        let ctx = crate::context::Context::new(crate::config::tests::test_config());
        let cancel = CancellationToken::new();

        let ctx_clone = Arc::clone(&ctx);
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            sensor.run(ctx_clone, cancel_clone).await;
        });

        // Give it time to attempt a read
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Should still be None since file doesn't exist
        let reading = ctx.sensors.cpu_temp.read().await;
        assert!(reading.is_none());

        cancel.cancel();
        let _ = handle.await;
    }

    // --- TempReadError Display test ---

    #[test]
    fn test_temp_read_error_display() {
        let io_err = TempReadError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert!(io_err.to_string().contains("io error"));

        let parse_err = TempReadError::Parse("bad data".to_string());
        assert!(parse_err.to_string().contains("parse error"));
        assert!(parse_err.to_string().contains("bad data"));
    }
}
