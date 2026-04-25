# SystemTelemetry — Design

**Status:** Draft
**Date:** 2026-04-25
**Replaces:** `CpuTempTelemetry` and `CpuTempSensor`

## Goal

Replace the narrow `CpuTempTelemetry` (CPU temperature only) with a broader `SystemTelemetry` payload that captures host-level health on each tick. One sensor task replaces the existing `CpuTempSensor` and produces one MQTT telemetry message.

## Payload

```rust
pub struct SystemTelemetry {
    pub cpu_temperature_c: Option<f64>,   // None when sysfs read fails
    pub cpu_usage_percent: f32,           // aggregate across all cores, 0..100
    pub ram: RamUsage,
    pub disks: Vec<DiskUsage>,
    pub load_avg: LoadAverage,            // 1/5/15-min
    pub uptime_s: u64,
    pub network_interfaces: Vec<NetworkInterfaceTelemetry>,
    pub cameras: Vec<CameraInfo>,
}

pub struct RamUsage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

pub struct DiskUsage {
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

pub struct NetworkInterfaceTelemetry {
    pub name: String,
    pub ipv4: Vec<String>,         // multiple addresses possible per iface
    pub rx_bps: u64,               // bits per second since previous tick
    pub tx_bps: u64,
}

pub struct CameraInfo {
    pub device: String,            // e.g. "/dev/video0"
    pub name: Option<String>,      // friendly name from V4L2 capability struct
    pub driver: Option<String>,
    pub formats: Vec<CameraFormat>,
}

pub struct CameraFormat {
    pub fourcc: String,            // e.g. "YUYV", "MJPG", "H264"
    pub width: u32,
    pub height: u32,
}
```

`TelemetryData::CpuTemp` is removed; `TelemetryData::System(SystemTelemetry)` is added.

## Sensor

`SystemSensor` lives at `src/sensors/system.rs`, implements `Sensor`, runs at the configured interval.

Per-tick work:
1. `sysinfo::System::refresh_cpu_usage()` + `refresh_memory()` + `refresh_load_average()`.
2. `sysinfo::Disks::refresh_list()` for disks.
3. `sysinfo::Networks::refresh()` for per-interface byte counters; compute `rx/tx_bps` from delta against the previously stored counters and the elapsed wall-clock time. First tick reports zeros.
4. Loopback interfaces are skipped.
5. IPv4 addresses come from `nix::ifaddrs::getifaddrs()` (already a dep, already used in `net.rs`) — keyed by interface name.
6. CPU temperature: read `/sys/class/thermal/thermal_zone0/temp` with the existing `parse_temperature` helper (moved from `cpu_temp.rs` into `system.rs`). On error, `cpu_temperature_c = None` and a warning is logged once per failure mode (no spam loop).
7. Cameras: enumerate `/dev/video*` via `glob`, then for each, open via the `v4l` crate and call `Device::query_caps()` + `Device::enum_formats()` + `Device::enum_framesizes()`. Probe failures (busy/permission/open error) drop the device from the list with a debug log; they don't fail the tick. Cameras are re-probed every tick for now (cheap; lets disconnects reflect promptly).

Sensor state held between ticks:
- `prev_net_bytes: HashMap<String, (u64, u64)>` — last (rx, tx) per iface.
- `prev_tick_at: Option<Instant>` — for elapsed-time bandwidth math.
- `sysinfo::System` instance held across ticks (required for delta-based CPU usage to be meaningful).

Stored in `Context.sensors.system: RwLock<Option<SystemTelemetry>>`. The old `cpu_temp` field is removed.

## Config

```toml
[sensors]
default_interval_s = 5.0   # bumped from 1.0 — system sensor is heavier than cpu_temp was

[sensors.system]
enabled = true
# interval_s omitted → falls back to default_interval_s (5s)
```

`SystemSensorConfig { enabled: bool, interval_s: Option<f64> }` replaces `CpuTempSensorConfig`. All references to `cpu_temp` in `config.rs`, `SensorsConfig`, `SensorManager`, the example config, and tests are renamed to `system`. Validation mirrors what `cpu_temp.interval_s` had (finite, positive).

`sensors.default_interval_s` is bumped from `1.0` to `5.0` in `config.toml.example` and the test fixtures. Ping and LTE keep their existing per-sensor overrides if any; sensors that previously relied on the 1s default will now run at 5s unless they set `interval_s` explicitly. Per-sensor overrides remain the escape hatch.

## MQTT

`TelemetryPublisher` publishes the new variant to `{prefix}/nodes/{nodeId}/telemetry/system`. The topic suffix `cpu_temp` goes away. JSON shape uses the existing `TelemetryEnvelope { ts, data: TelemetryData::System(...) }` pattern.

## MAVLink

No new MAVLink reporting is added. CPU temp was MQTT-only; the new metrics are MQTT-only as well. `telemetry_reporter.rs` is unchanged.

## Schema

After implementation, run `cargo run --bin generate-schema` (or `make schema`) to regenerate `assets/schema/telemetry/envelope.json` against the new `TelemetryData` shape. Verify the diff includes `System` and removes `CpuTemp`.

## Dependencies (add to `Cargo.toml`)

- `sysinfo = "0.32"` (or current) — system metrics.
- `v4l = "0.14"` (or current) — V4L2 capability probe for cameras.
- `glob = "0.3"` — `/dev/video*` enumeration.

## Testing

Unit-testable parts (no real hardware):
- `parse_temperature` (moved verbatim from `cpu_temp.rs`).
- Bandwidth calculation: pure function `compute_bps(prev, curr, elapsed) -> (rx_bps, tx_bps)` with edge cases (counter wrap → treat as zero, elapsed == 0 → zero, first tick → zero).
- Loopback filter: `is_loopback(name) -> bool`.
- Round-trip serde for `TelemetryEnvelope` carrying `TelemetryData::System` (mirrors existing `round_trip_cpu_temp_telemetry` test).
- `SensorManager` selection: `system` enabled/disabled, mirroring existing `cpu_temp` cases.
- Schema generation test still passes (`assets/schema/telemetry/envelope.json` regenerates).

Hardware-dependent parts (CPU usage, disk list, camera enumeration) are exercised by an `#[ignore]`'d integration test that just runs the sensor for one tick on the dev machine.

## Migration / out of scope

- No backwards-compat shim. `cpu_temp` is removed from config, sensor list, telemetry enum, and MQTT topic. Existing deployments must update `config.toml` to rename `[sensors.cpu_temp]` → `[sensors.system]` and (optionally) review `sensors.default_interval_s`, which now defaults to 5s.
- Camera streaming config (`[camera]`) is untouched — that's the active output stream, not enumeration.
- No alerting / thresholds. This is pure observability.
- Schema update is a build-time artifact; consumers that pin to the old schema must be updated separately.

## File-level changes

- `src/sensors/system.rs` — new file, replaces `cpu_temp.rs`.
- `src/sensors/cpu_temp.rs` — deleted.
- `src/sensors/mod.rs` — swap module + `SensorValues.cpu_temp` → `system`; swap `CpuTempSensor` wiring → `SystemSensor`.
- `src/messages/telemetry.rs` — remove `CpuTempTelemetry` and `TelemetryData::CpuTemp`; add `SystemTelemetry`, sub-types, and `TelemetryData::System`.
- `src/config.rs` — rename `CpuTempSensorConfig` → `SystemSensorConfig`, `cpu_temp` field → `system`, update tests + example TOML.
- `src/services/mqtt/telemetry.rs` — change topic suffix and read site.
- `src/context.rs` — `SensorValues.cpu_temp` → `system`.
- `config.toml.example` — rename section.
- `Cargo.toml` — add `sysinfo`, `v4l`, `glob`.
- `assets/schema/telemetry/envelope.json` — regenerated.
- `CLAUDE.md` — update sensor + types section to reflect `SystemSensor` / `SystemTelemetry`.
