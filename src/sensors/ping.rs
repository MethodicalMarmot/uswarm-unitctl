use std::sync::Arc;

use async_trait::async_trait;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::PingSensorConfig;
use crate::context::Context;
use crate::messages::telemetry::PingTelemetry;

use super::Sensor;

/// Ping sensor that spawns a persistent `ping` subprocess, periodically
/// sends SIGQUIT to get intermediate statistics, and parses the output
/// to extract latency and packet loss.
pub struct PingSensor {
    host: String,
    interface: String,
    interval: Duration,
}

impl PingSensor {
    pub fn new(config: &PingSensorConfig, default_interval: f64) -> Self {
        let interval_s = config.interval_s.unwrap_or(default_interval);
        Self {
            host: config.host.clone(),
            interface: config.interface.clone(),
            interval: Duration::from_secs_f64(interval_s),
        }
    }
}

#[async_trait]
impl Sensor for PingSensor {
    fn name(&self) -> &str {
        "ping"
    }

    async fn run(&self, ctx: Arc<Context>, cancel: CancellationToken) {
        loop {
            info!(host = %self.host, "starting ping subprocess");

            match run_ping_subprocess(&self.host, &self.interface, self.interval, &ctx, &cancel)
                .await
            {
                PingExit::Cancelled => {
                    info!("ping sensor cancelled");
                    return;
                }
                PingExit::ProcessExited(code) => {
                    warn!(exit_code = ?code, "ping process exited, restarting");
                }
                PingExit::SpawnError(e) => {
                    error!(error = %e, "failed to spawn ping process, retrying");
                }
            }

            // Brief delay before restart to avoid tight restart loops
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

enum PingExit {
    Cancelled,
    ProcessExited(Option<i32>),
    SpawnError(std::io::Error),
}

/// Spawn ping subprocess and run the SIGQUIT/parse loop.
async fn run_ping_subprocess(
    host: &str,
    interface: &str,
    interval: Duration,
    ctx: &Arc<Context>,
    cancel: &CancellationToken,
) -> PingExit {
    let mut cmd = Command::new("ping");

    if !interface.is_empty() {
        cmd.arg("-I").arg(interface);
    }
    cmd.arg("-q").arg("-i0.2").arg(host);
    cmd.kill_on_drop(true);

    // Ping writes SIGQUIT stats to stderr
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return PingExit::SpawnError(e),
    };

    let pid = match child.id() {
        Some(id) => Pid::from_raw(id as i32),
        None => {
            error!("ping process has no pid");
            return PingExit::ProcessExited(None);
        }
    };

    let stderr = child.stderr.take().expect("stderr was piped");
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    let re = build_ping_regex();
    let mut prev_sent: u64 = 0;
    let mut prev_rcvd: u64 = 0;

    // Spawn a task that sends SIGQUIT at the configured interval
    let sigquit_cancel = cancel.child_token();
    let sigquit_handle = tokio::spawn({
        let cancel = sigquit_cancel.clone();
        async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {
                        if let Err(e) = signal::kill(pid, Signal::SIGQUIT) {
                            debug!(error = %e, "failed to send SIGQUIT to ping");
                            break;
                        }
                    }
                }
            }
        }
    });

    let mut stderr_open = true;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Kill the ping process
                let _ = child.kill().await;
                sigquit_cancel.cancel();
                let _ = sigquit_handle.await;
                return PingExit::Cancelled;
            }
            status = child.wait() => {
                sigquit_cancel.cancel();
                let _ = sigquit_handle.await;
                // Mark as unreachable on process exit
                let reading = PingTelemetry {
                    reachable: false,
                    latency_ms: 0.0,
                    loss_percent: 100,
                };
                *ctx.sensors.ping.write().await = Some(reading);
                return PingExit::ProcessExited(status.ok().and_then(|s| s.code()));
            }
            line_result = lines.next_line(), if stderr_open => {
                match line_result {
                    Ok(Some(line)) => {
                        if let Some(stats) = parse_ping_line(&re, &line) {
                            let reading = compute_reading(&stats, &mut prev_sent, &mut prev_rcvd);
                            debug!(
                                reachable = reading.reachable,
                                latency_ms = reading.latency_ms,
                                loss_percent = reading.loss_percent,
                                "ping reading updated"
                            );
                            *ctx.sensors.ping.write().await = Some(reading);
                        }
                    }
                    Ok(None) => {
                        // EOF on stderr — stop polling, wait for process exit
                        stderr_open = false;
                    }
                    Err(e) => {
                        warn!(error = %e, "error reading ping stderr");
                    }
                }
            }
        }
    }
}

/// Intermediate parsed stats from a ping SIGQUIT output line.
#[derive(Debug, Clone)]
pub struct PingStats {
    pub packets_rcvd: u64,
    pub packets_sent: u64,
    pub latency_ms: Option<f64>,
}

/// Build the regex for parsing ping SIGQUIT output.
/// Format: "5/10 packets, 50% loss, min/avg/ewma/max = 10.5/25.3/20.1/45.2 ms"
pub fn build_ping_regex() -> Regex {
    Regex::new(
        r"(\d+)/(\d+) packets, \d+% loss(?:, min/avg/ewma/max = [\d.]+/[\d.]+/([\d.]+)/[\d.]+ ms)?",
    )
    .expect("ping regex is valid")
}

/// Parse a single line of ping stderr output. Returns parsed stats if the line
/// matches the expected SIGQUIT statistics format.
pub fn parse_ping_line(re: &Regex, line: &str) -> Option<PingStats> {
    let caps = re.captures(line)?;

    let packets_rcvd = caps.get(1)?.as_str().parse::<u64>().ok()?;
    let packets_sent = caps.get(2)?.as_str().parse::<u64>().ok()?;
    let latency_ms = caps.get(3).and_then(|m| m.as_str().parse::<f64>().ok());

    Some(PingStats {
        packets_rcvd,
        packets_sent,
        latency_ms,
    })
}

/// Compute a PingTelemetry from parsed stats, using delta-based loss calculation.
///
/// Tracks previous sent/received counts to calculate loss over the most recent
/// interval rather than cumulative loss.
pub fn compute_reading(
    stats: &PingStats,
    prev_sent: &mut u64,
    prev_rcvd: &mut u64,
) -> PingTelemetry {
    let delta_sent = stats.packets_sent.saturating_sub(*prev_sent);
    let delta_rcvd = stats.packets_rcvd.saturating_sub(*prev_rcvd);

    *prev_sent = stats.packets_sent;
    *prev_rcvd = stats.packets_rcvd;

    let loss_percent = calculate_loss_percent(delta_sent, delta_rcvd);

    let reachable = stats.latency_ms.is_some() && loss_percent < 100;
    let latency_ms = stats.latency_ms.unwrap_or(0.0);

    PingTelemetry {
        reachable,
        latency_ms,
        loss_percent,
    }
}

/// Calculate loss percentage from delta sent/received counts.
///
/// Returns 0 if no packets were sent or if received >= sent.
/// Uses ceiling to round up (matching Python implementation).
pub fn calculate_loss_percent(delta_sent: u64, delta_rcvd: u64) -> u8 {
    if delta_sent == 0 || delta_rcvd >= delta_sent {
        return 0;
    }
    let loss = 100.0 - (delta_rcvd as f64 / delta_sent as f64) * 100.0;
    // Ceiling, clamped to u8 range
    let ceiled = loss.ceil() as u64;
    ceiled.min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re() -> Regex {
        build_ping_regex()
    }

    // --- Parsing tests ---

    #[test]
    fn test_parse_connected_with_latency() {
        let line = "5/5 packets, 0% loss, min/avg/ewma/max = 0.123/0.456/0.789/1.234 ms";
        let stats = parse_ping_line(&re(), line).unwrap();
        assert_eq!(stats.packets_rcvd, 5);
        assert_eq!(stats.packets_sent, 5);
        assert!((stats.latency_ms.unwrap() - 0.789).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_disconnected_no_latency() {
        // When all packets are lost, ping omits the timing section
        let line = "0/5 packets, 100% loss";
        let stats = parse_ping_line(&re(), line).unwrap();
        assert_eq!(stats.packets_rcvd, 0);
        assert_eq!(stats.packets_sent, 5);
        assert!(stats.latency_ms.is_none());
    }

    #[test]
    fn test_parse_partial_loss() {
        let line = "3/5 packets, 40% loss, min/avg/ewma/max = 1.0/2.0/1.5/3.0 ms";
        let stats = parse_ping_line(&re(), line).unwrap();
        assert_eq!(stats.packets_rcvd, 3);
        assert_eq!(stats.packets_sent, 5);
        assert!((stats.latency_ms.unwrap() - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_large_packet_counts() {
        let line = "12345/12350 packets, 0% loss, min/avg/ewma/max = 10.0/20.0/15.0/30.0 ms";
        let stats = parse_ping_line(&re(), line).unwrap();
        assert_eq!(stats.packets_rcvd, 12345);
        assert_eq!(stats.packets_sent, 12350);
        assert!((stats.latency_ms.unwrap() - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_non_matching_line() {
        let line = "PING 10.45.0.2 (10.45.0.2) 56(84) bytes of data.";
        assert!(parse_ping_line(&re(), line).is_none());
    }

    #[test]
    fn test_parse_empty_line() {
        assert!(parse_ping_line(&re(), "").is_none());
    }

    #[test]
    fn test_parse_garbage_input() {
        assert!(parse_ping_line(&re(), "random garbage text").is_none());
    }

    // --- Loss percentage calculation tests ---

    #[test]
    fn test_loss_zero_when_all_received() {
        assert_eq!(calculate_loss_percent(5, 5), 0);
    }

    #[test]
    fn test_loss_100_when_none_received() {
        assert_eq!(calculate_loss_percent(5, 0), 100);
    }

    #[test]
    fn test_loss_partial() {
        // 3 received of 5 sent = 40% loss
        assert_eq!(calculate_loss_percent(5, 3), 40);
    }

    #[test]
    fn test_loss_rounds_up() {
        // 2 received of 3 sent = 33.33...% loss -> ceil = 34
        assert_eq!(calculate_loss_percent(3, 2), 34);
    }

    #[test]
    fn test_loss_zero_when_no_packets_sent() {
        assert_eq!(calculate_loss_percent(0, 0), 0);
    }

    #[test]
    fn test_loss_zero_when_rcvd_exceeds_sent() {
        // Edge case: should not happen but handle gracefully
        assert_eq!(calculate_loss_percent(3, 5), 0);
    }

    #[test]
    fn test_loss_one_of_many() {
        // 99 received of 100 sent = 1% loss
        assert_eq!(calculate_loss_percent(100, 99), 1);
    }

    #[test]
    fn test_loss_ceil_small_fraction() {
        // 99 received of 200 sent = 50.5% loss -> ceil = 51
        // Actually 101/200 = 50.5% loss
        // Wait, 99/200 received = 49.5% received, so loss = 50.5 -> ceil = 51
        assert_eq!(calculate_loss_percent(200, 99), 51);
    }

    // --- compute_reading tests ---

    #[test]
    fn test_reading_connected() {
        let stats = PingStats {
            packets_rcvd: 5,
            packets_sent: 5,
            latency_ms: Some(10.5),
        };
        let mut prev_sent = 0;
        let mut prev_rcvd = 0;

        let reading = compute_reading(&stats, &mut prev_sent, &mut prev_rcvd);
        assert!(reading.reachable);
        assert!((reading.latency_ms - 10.5).abs() < f64::EPSILON);
        assert_eq!(reading.loss_percent, 0);
        assert_eq!(prev_sent, 5);
        assert_eq!(prev_rcvd, 5);
    }

    #[test]
    fn test_reading_disconnected() {
        let stats = PingStats {
            packets_rcvd: 0,
            packets_sent: 5,
            latency_ms: None,
        };
        let mut prev_sent = 0;
        let mut prev_rcvd = 0;

        let reading = compute_reading(&stats, &mut prev_sent, &mut prev_rcvd);
        assert!(!reading.reachable);
        assert!((reading.latency_ms - 0.0).abs() < f64::EPSILON);
        assert_eq!(reading.loss_percent, 100);
    }

    #[test]
    fn test_reading_delta_based_loss() {
        let mut prev_sent: u64 = 10;
        let mut prev_rcvd: u64 = 10;

        // Second reading: 15 sent, 13 received -> delta: 5 sent, 3 rcvd -> 40% loss
        let stats = PingStats {
            packets_rcvd: 13,
            packets_sent: 15,
            latency_ms: Some(20.0),
        };
        let reading = compute_reading(&stats, &mut prev_sent, &mut prev_rcvd);
        assert!(reading.reachable);
        assert_eq!(reading.loss_percent, 40);
        assert!((reading.latency_ms - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reading_full_loss_with_latency_from_before() {
        // Edge case: latency is None but loss < 100 shouldn't happen in practice,
        // but if it did, reachable should be false
        let stats = PingStats {
            packets_rcvd: 5,
            packets_sent: 5,
            latency_ms: None,
        };
        let mut prev_sent = 0;
        let mut prev_rcvd = 0;

        let reading = compute_reading(&stats, &mut prev_sent, &mut prev_rcvd);
        // No latency means not reachable, even if 0% loss
        assert!(!reading.reachable);
    }

    // --- PingSensor construction test ---

    #[test]
    fn test_ping_sensor_new_with_defaults() {
        let config = PingSensorConfig {
            enabled: true,
            interval_s: None,
            host: "10.45.0.2".to_string(),
            interface: String::new(),
        };
        let sensor = PingSensor::new(&config, 1.0);
        assert_eq!(sensor.host, "10.45.0.2");
        assert_eq!(sensor.interface, "");
        assert_eq!(sensor.interval, Duration::from_secs_f64(1.0));
    }

    #[test]
    fn test_ping_sensor_new_with_override() {
        let config = PingSensorConfig {
            enabled: true,
            interval_s: Some(0.5),
            host: "192.168.1.1".to_string(),
            interface: "eth0".to_string(),
        };
        let sensor = PingSensor::new(&config, 1.0);
        assert_eq!(sensor.host, "192.168.1.1");
        assert_eq!(sensor.interface, "eth0");
        assert_eq!(sensor.interval, Duration::from_secs_f64(0.5));
    }

    #[test]
    fn test_sensor_name() {
        let config = PingSensorConfig::default();
        let sensor = PingSensor::new(&config, 1.0);
        assert_eq!(sensor.name(), "ping");
    }
}
