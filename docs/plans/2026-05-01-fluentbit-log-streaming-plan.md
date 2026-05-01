# Fluent Bit systemd log streaming — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream selected systemd journal entries from each unit to a central log server using Fluent Bit's `forward` output, configured by `unitctl` from `config.toml` at startup.

**Architecture:** New `[fluentbit]` config section drives a `Task`-implementing env writer (`FluentbitEnvWriter`) that emits a Fluent Bit YAML config. A new `fluentbit.service` runs `fluent-bit -c <config_path>`; a `fluentbit-watcher.{path,service}` pair restarts it whenever the YAML is regenerated — mirroring the camera/mavlink precedent. The three TLS cert paths move from `[mqtt]` to `[general]` as `Option<String>` so MQTT and Fluent Bit can share them; each consumer validates availability independently.

**Tech Stack:** Rust (tokio), serde+toml, schemars, tracing; Fluent Bit ≥ 2.x with YAML config; systemd path/service activation.

**Spec:** `docs/plans/2026-05-01-fluentbit-log-streaming-design.md`

**Plan location override:** Saved to `docs/plans/` per repo convention (CLAUDE.md), not `docs/superpowers/plans/`.

---

## File Structure

| Path | Action | Responsibility |
|---|---|---|
| `src/config.rs` | Modify | Add `Option<String>` cert fields on `GeneralConfig`; add `FluentbitConfig` struct + validation; remove cert fields from `MqttConfig`; update fixtures. |
| `src/services/mqtt/transport.rs` | Modify | `MqttTransport::new` takes `&Config`; new `TransportError::MissingTlsConfig { field }`. Reads cert paths from `general.*`. |
| `src/messages/commands.rs` | Modify | New `SafeGeneralConfig` mirroring `GeneralConfig` with cert redaction. `SafeMqttConfig` loses cert fields. `SafeConfig.fluentbit: FluentbitConfig`. |
| `src/services/mqtt/handlers/get_config.rs` | Modify | Update redaction tests to assert `general.*` redaction and `fluentbit` exposure. |
| `src/env/fluentbit_env.rs` | Create | `generate_fluentbit_config()` pure helper + `FluentbitEnvWriter` Task. |
| `src/env/mod.rs` | Modify | `pub mod fluentbit_env; pub use fluentbit_env::FluentbitEnvWriter;` |
| `src/main.rs` | Modify | Update `MqttTransport::new` call site (now takes `&ctx.config`); spawn `FluentbitEnvWriter`. |
| `services/fluentbit.service` | Create | Runs `fluent-bit -c /etc/fluent-bit.conf`, waits for the file. |
| `services/fluentbit-watcher.path` | Create | `PathModified=/etc/fluent-bit.conf`. |
| `services/fluentbit-watcher.service` | Create | `oneshot` calling `systemctl restart fluentbit`. |
| `scripts/install.sh` | Modify | Install `fluent-bit` apt package; link & enable fluentbit units; uninstall hook. |
| `config.toml.example` | Modify | Move cert paths to `[general]`; new `[fluentbit]` section. |

---

### Task 1: Add optional TLS cert paths to `GeneralConfig`

**Files:**
- Modify: `src/config.rs`

Adds three `Option<String>` cert fields to `GeneralConfig` (additive — `MqttConfig` keeps its fields for now so the build stays green). Adds `Config::validate()` checks: when `Some`, the value must be non-empty and contain no `\n` / `\r`.

- [x] **Step 1: Write failing test for parsing `general.*_cert_path`**

Add to `mod tests` in `src/config.rs` (place near the existing `test_general_interface_roundtrips`):

```rust
#[test]
fn test_general_cert_paths_parsed_when_present() {
    let toml_str = format!(
        "{}\nca_cert_path = \"/etc/unitctl/certs/ca.pem\"\n\
         client_cert_path = \"/etc/unitctl/certs/client.pem\"\n\
         client_key_path = \"/etc/unitctl/certs/client.key\"\n",
        // Replace the existing [general] block to include cert paths.
        FULL_TEST_CONFIG.replace(
            "env_dir = \"/var/run/unitctl\"",
            "env_dir = \"/var/run/unitctl\"\nca_cert_path = \"/etc/unitctl/certs/ca.pem\"\n\
             client_cert_path = \"/etc/unitctl/certs/client.pem\"\n\
             client_key_path = \"/etc/unitctl/certs/client.key\""
        )
    );
    let config: Config = toml::from_str(&toml_str).expect("parse with cert paths");
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
    let config = test_config();
    assert!(config.general.ca_cert_path.is_none());
    assert!(config.general.client_cert_path.is_none());
    assert!(config.general.client_key_path.is_none());
}
```

- [x] **Step 2: Run the new tests to confirm failure**

```bash
cargo test --lib config::tests::test_general_cert_paths -- --exact --nocapture
```
Expected: compile error — fields don't exist on `GeneralConfig`.

- [x] **Step 3: Add the fields to `GeneralConfig`**

Edit `src/config.rs` — replace the existing `GeneralConfig` definition with:

```rust
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
```

- [x] **Step 4: Run the parsing tests to confirm pass**

```bash
cargo test --lib config::tests::test_general_cert_paths -- --nocapture
```
Expected: PASS.

- [x] **Step 5: Write failing tests for cert-path validation**

Add to `mod tests` in `src/config.rs`:

```rust
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
```

- [x] **Step 6: Run the validation tests to confirm failure**

```bash
cargo test --lib config::tests::test_validate -- --nocapture 2>&1 | tail -20
```
Expected: the three "rejects" tests FAIL (validate currently accepts anything).

- [x] **Step 7: Add the validation rules**

In `src/config.rs`, in `Config::validate()`, after the existing `general.env_dir` newline check (immediately before the sensor interval block), insert:

```rust
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
```

- [x] **Step 8: Run all config tests**

```bash
cargo test --lib config::tests
```
Expected: all PASS.

- [x] **Step 9: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add optional TLS cert paths to [general]"
```

---

### Task 2: Move TLS cert paths from `[mqtt]` to `[general]`

**Files:**
- Modify: `src/config.rs`
- Modify: `src/services/mqtt/transport.rs`
- Modify: `src/messages/commands.rs`
- Modify: `src/services/mqtt/handlers/get_config.rs`
- Modify: `src/main.rs`
- Modify: `config.toml.example`

This is the breaking config change. After this commit, any `[mqtt].ca_cert_path` etc. is rejected; consumers read from `[general]`. `MqttTransport::new` gains a `MissingTlsConfig` error variant. `SafeConfig` gains `SafeGeneralConfig` and `SafeMqttConfig` loses cert fields.

- [x] **Step 1: Write failing test for `MqttTransport::new` rejecting missing cert paths**

In `src/services/mqtt/transport.rs`, replace the existing `test_new_with_invalid_cert_paths` body with:

```rust
    #[test]
    fn test_new_with_invalid_cert_paths() {
        use crate::config::tests::test_config;
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.general.ca_cert_path = Some("/nonexistent/ca.pem".to_string());
        config.general.client_cert_path = Some("/nonexistent/cert.pem".to_string());
        config.general.client_key_path = Some("/nonexistent/key.pem".to_string());

        let result = MqttTransport::new(&config, CancellationToken::new());
        assert!(matches!(result, Err(TransportError::Tls(_))));
    }

    #[test]
    fn test_new_returns_missing_tls_when_general_ca_cert_absent() {
        use crate::config::tests::test_config;
        let mut config = test_config();
        config.mqtt.enabled = true;
        config.general.ca_cert_path = None;
        config.general.client_cert_path = Some("/x".to_string());
        config.general.client_key_path = Some("/y".to_string());

        let err = MqttTransport::new(&config, CancellationToken::new()).unwrap_err();
        match err {
            TransportError::MissingTlsConfig { field } => {
                assert_eq!(field, "general.ca_cert_path");
            }
            other => panic!("expected MissingTlsConfig, got {other:?}"),
        }
    }
```

- [x] **Step 2: Run failing test**

```bash
cargo test --lib services::mqtt::transport 2>&1 | tail -20
```
Expected: compile errors (`general` field doesn't exist on `MqttConfig`; `MissingTlsConfig` variant doesn't exist; `MqttTransport::new` takes `&MqttConfig` not `&Config`).

- [x] **Step 3: Update `TransportError` and `MqttTransport::new` to take `&Config`**

In `src/services/mqtt/transport.rs`:

Replace the `TransportError` enum:
```rust
/// Errors from the MQTT transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("TLS error: {0}")]
    Tls(#[from] tls::TlsError),
    #[error("MQTT client error: {0}")]
    Client(#[from] rumqttc::ClientError),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("missing TLS config: {field}")]
    MissingTlsConfig { field: &'static str },
}
```

Replace the `use crate::config::MqttConfig;` line with:
```rust
use crate::config::Config;
```

Update `MqttTransport::new` signature and body (replace the existing `pub fn new(...)` block up through `let session_id = generate_session_id();`):

```rust
    pub fn new(config: &Config, cancel: CancellationToken) -> Result<Self, TransportError> {
        let mqtt = &config.mqtt;
        let general = &config.general;

        let ca = general
            .ca_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(TransportError::MissingTlsConfig {
                field: "general.ca_cert_path",
            })?;
        let cert = general
            .client_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(TransportError::MissingTlsConfig {
                field: "general.client_cert_path",
            })?;
        let key = general
            .client_key_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(TransportError::MissingTlsConfig {
                field: "general.client_key_path",
            })?;

        let tls_config = tls::load_tls_config(ca, cert, key)?;
        let node_id = tls::extract_node_id(cert)?;
        validate_topic_segment(&node_id, "node_id (certificate CN)")?;
        validate_topic_segment(&mqtt.env_prefix, "env_prefix")?;

        let session_id = generate_session_id();
```

Inside `new`, replace any subsequent reference to `config.host`/`config.port`/`config.env_prefix`/etc. with `mqtt.host`/`mqtt.port`/`mqtt.env_prefix`. (Scan the rest of the function and substitute `config.` with `mqtt.` where it referred to MQTT fields. Do not touch references to `config` if they no longer exist.)

- [x] **Step 4: Remove cert fields from `MqttConfig`**

In `src/config.rs`, replace `MqttConfig`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct MqttConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub env_prefix: String,
    pub telemetry_interval_s: f64,
}
```

Replace `MqttConfig::default()`:

```rust
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
```

In `Config::validate()`, delete the three `if self.mqtt.{ca_cert_path,client_cert_path,client_key_path}.is_empty()` checks (the corresponding new checks live in `MqttTransport::new`).

In the `mod tests` of `src/config.rs`, replace **all** occurrences of `mqtt.ca_cert_path` / `mqtt.client_cert_path` / `mqtt.client_key_path` setters and assertions:
- Tests like `test_mqtt_enabled_empty_ca_cert_rejected`, `test_mqtt_enabled_empty_client_cert_rejected`, `test_mqtt_enabled_empty_client_key_rejected`, `test_mqtt_disabled_skips_validation`, `test_mqtt_config_parsed`, `test_mqtt_config_default` — delete the assertions/setters that reference these fields. The three "empty cert path rejected" tests should be **deleted** (they belong to `MqttTransport::new` now and exist there).
- In `FULL_TEST_CONFIG` (line ~493) and any inline TOML strings inside tests, delete the three lines:
  ```
  ca_cert_path = "/etc/unitctl/certs/ca.pem"
  client_cert_path = "/etc/unitctl/certs/client.pem"
  client_key_path = "/etc/unitctl/certs/client.key"
  ```
  Add to the `[general]` block in `FULL_TEST_CONFIG`:
  ```
  ca_cert_path = "/etc/unitctl/certs/ca.pem"
  client_cert_path = "/etc/unitctl/certs/client.pem"
  client_key_path = "/etc/unitctl/certs/client.key"
  ```
- Same for the inline TOML string in `test_parse_full_config` and `test_sensor_config_full` and `test_mqtt_missing_section_fails`.
- Update `test_mqtt_config_parsed` body to remove the three `mqtt.*_cert_path` assertions and replace them with assertions on `config.general.{ca,client}_cert_path` / `config.general.client_key_path` matching the values in `FULL_TEST_CONFIG`.

- [x] **Step 5: Update `SafeConfig` to redact in `general` and drop from `mqtt`**

In `src/messages/commands.rs`, replace the `SafeConfig` and `SafeMqttConfig` definitions plus their `From` impls (the entire block from `pub struct SafeConfig {` through the end of `impl From<&MqttConfig> for SafeMqttConfig {`) with:

```rust
/// A sanitized view of the full `Config`, safe for MQTT exposure.
/// TLS certificate paths in `general` are replaced with `"***"`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeConfig {
    pub general: SafeGeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
    pub camera: CameraConfig,
    pub mqtt: SafeMqttConfig,
    pub fluentbit: crate::config::FluentbitConfig,
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
```

Note: this references `crate::config::FluentbitConfig`, which is added in Task 4. We add a temporary placeholder now and Task 4 fills it in. To keep this commit green, **do not** include `fluentbit` in `SafeConfig` yet — it's added in Task 5. Update the `SafeConfig` struct above by removing the `pub fluentbit: ...` line and the `fluentbit: config.fluentbit.clone(),` line in `From<&Config>`. (Task 5 puts them back.)

So the green-build version for this task is:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeConfig {
    pub general: SafeGeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
    pub camera: CameraConfig,
    pub mqtt: SafeMqttConfig,
}

impl From<&Config> for SafeConfig {
    fn from(config: &Config) -> Self {
        Self {
            general: SafeGeneralConfig::from(&config.general),
            mavlink: config.mavlink.clone(),
            sensors: config.sensors.clone(),
            camera: config.camera.clone(),
            mqtt: SafeMqttConfig::from(&config.mqtt),
        }
    }
}
```

Update the `use` line at the top of `src/messages/commands.rs` from
```rust
use crate::config::{
    CameraConfig, Config, GeneralConfig, MavlinkConfig, MqttConfig, SensorsConfig,
};
```
(it already imports what's needed — confirm `GeneralConfig` is in the list; if not, add it).

- [x] **Step 6: Update `SafeConfig` tests in `src/messages/commands.rs`**

Find and update the existing tests:

- Tests at lines ~248–250 referencing `safe.mqtt.ca_cert_path` etc. → change to `safe.general.ca_cert_path.as_deref(), Some("***")` (and same for `client_cert_path`, `client_key_path`).
- Test at line ~263 referencing `parsed.mqtt.ca_cert_path` → change to `parsed.general.ca_cert_path.as_deref(), Some("***")`.
- Test at line ~426 referencing `r.config.mqtt.ca_cert_path` → change to `r.config.general.ca_cert_path.as_deref(), Some("***")`.

For each test that exercises redaction, the assertion shape becomes:
```rust
assert_eq!(safe.general.ca_cert_path.as_deref(), Some("***"));
assert_eq!(safe.general.client_cert_path.as_deref(), Some("***"));
assert_eq!(safe.general.client_key_path.as_deref(), Some("***"));
```

Add a new test that confirms `None` cert paths stay `None`:
```rust
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
```

The ones in `src/services/mqtt/handlers/get_config.rs` lines 105–119 — change to assert against `general`:
```rust
assert_eq!(config.general.ca_cert_path.as_deref(), Some("***"));
assert_eq!(config.general.client_cert_path.as_deref(), Some("***"));
assert_eq!(config.general.client_key_path.as_deref(), Some("***"));
```

The pre-test fixture must populate `general.*_cert_path` with `Some(...)` so the redaction has something to redact. Update the relevant test setup or use `test_config()` after we make that fixture include `Some(...)` paths. Simpler: keep the fixture cert-less, and only assert `None`-passthrough; for the existing redaction test, mutate `cfg.general.ca_cert_path = Some("/x".to_string());` etc. before invoking the handler. Concrete edit: in `test_get_config_handler_redacts_cert_paths` and `test_get_config_handler_uses_safe_config`, prepend:
```rust
let mut cfg = test_config();
cfg.general.ca_cert_path = Some("/etc/unitctl/certs/ca.pem".to_string());
cfg.general.client_cert_path = Some("/etc/unitctl/certs/client.pem".to_string());
cfg.general.client_key_path = Some("/etc/unitctl/certs/client.key".to_string());
let ctx = Context::new(cfg);
```
(replacing the existing `let ctx = Context::new(test_config());` line) and update the assertions to read `config.general.ca_cert_path.as_deref()` etc.

- [x] **Step 7: Update `MqttTransport` test fixtures referencing the old fields**

In `src/services/mqtt/transport.rs`, around line 467–482, the previous `MqttConfig { ca_cert_path, ... }` literal was rewritten in Step 1; confirm it's gone. Search for any remaining `MqttConfig {` literal that names cert fields and remove those fields:

```bash
grep -n "MqttConfig {" src/services/mqtt/transport.rs
```

For every match, remove the three cert lines from the literal.

- [x] **Step 8: Update `main.rs` `MqttTransport::new` call site**

In `src/main.rs`, replace
```rust
match MqttTransport::new(&ctx.config.mqtt, cancel.clone()) {
```
with
```rust
match MqttTransport::new(&ctx.config, cancel.clone()) {
```

- [x] **Step 9: Update `config.toml.example`**

In `config.toml.example`, **add** these lines to the `[general]` block (between `env_dir` and the next blank line):
```toml
# Optional mutual-TLS material consumed by [mqtt] and [fluentbit].
# Each consumer validates availability independently.
ca_cert_path = "/etc/unitctl/certs/ca.pem"
client_cert_path = "/etc/unitctl/certs/client.pem"
client_key_path = "/etc/unitctl/certs/client.key"
```
**Delete** these three lines from the `[mqtt]` block:
```toml
ca_cert_path = "/etc/unitctl/certs/ca.pem"
client_cert_path = "/etc/unitctl/certs/client.pem"
client_key_path = "/etc/unitctl/certs/client.key"
```

- [x] **Step 10: Build + test**

```bash
cargo build --release
cargo test --lib
```
Expected: PASS.

- [x] **Step 11: Commit**

```bash
git add src/config.rs src/services/mqtt/transport.rs src/messages/commands.rs \
        src/services/mqtt/handlers/get_config.rs src/main.rs config.toml.example
git commit -m "refactor(config): move TLS cert paths from [mqtt] to [general]"
```

---

### Task 3: Add `FluentbitConfig` to `Config`

**Files:**
- Modify: `src/config.rs`

Adds the new section, field-by-field. Pure data + parse tests; no validation logic yet.

- [x] **Step 1: Write failing test for parsing `[fluentbit]`**

In `mod tests` of `src/config.rs`:

```rust
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
```

- [x] **Step 2: Run failing tests**

```bash
cargo test --lib config::tests::test_fluentbit -- --nocapture
```
Expected: compile error — `Config` has no `fluentbit` field.

- [x] **Step 3: Add `FluentbitConfig` struct and field**

Append to `src/config.rs` next to `MqttConfig`:

```rust
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
```

Add `pub fluentbit: FluentbitConfig,` to the `Config` struct (after `pub mqtt: MqttConfig,`).

- [x] **Step 4: Update `FULL_TEST_CONFIG`**

In `src/config.rs`, append to `FULL_TEST_CONFIG`:

```rust
[fluentbit]
enabled = false
host = "logs.example.com"
port = 24224
tls = true
tls_verify = true
config_path = "/etc/fluent-bit.conf"
# systemd_filter placeholder
```

(Note: the `# systemd_filter placeholder` comment is the marker the second test replaces.)

Also append the same block to every other inline TOML string in tests that uses a full config (search: `\[mqtt\]` inside tests, and append `[fluentbit] ...` directly after the mqtt block). Specifically: the inline TOMLs in `test_parse_full_config`, `test_sensor_config_full`, and any other test that constructs a complete Config from a TOML string.

- [x] **Step 5: Run the parsing tests to confirm pass**

```bash
cargo test --lib config::tests
```
Expected: PASS.

- [x] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add [fluentbit] section"
```

---

### Task 4: Validate `[fluentbit]` in `Config::validate()`

**Files:**
- Modify: `src/config.rs`

Adds validation: when `fluentbit.enabled`, `host` non-empty + not `-`-prefixed, `port != 0`, `config_path` non-empty + no newlines, each `systemd_filter` entry has exactly one `=`, no newlines, key matches `[A-Z_][A-Z0-9_]*`.

- [x] **Step 1: Write failing tests**

In `mod tests` of `src/config.rs`:

```rust
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
```

- [x] **Step 2: Run failing tests**

```bash
cargo test --lib config::tests::test_fluentbit_enabled -- --nocapture
```
Expected: most "rejected" tests FAIL.

- [x] **Step 3: Add validation block in `Config::validate()`**

In `src/config.rs`, immediately before the final `Ok(())` of `Config::validate()`, insert:

```rust
        // Validate fluentbit config (only when enabled)
        if self.fluentbit.enabled {
            let f = &self.fluentbit;
            if f.host.is_empty() || f.host.starts_with('-') {
                return Err(ConfigError::Validation(
                    "fluentbit.host must be a non-empty value that does not start with '-'"
                        .to_string(),
                ));
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
```

- [x] **Step 4: Run all config tests**

```bash
cargo test --lib config::tests
```
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): validate [fluentbit] section"
```

---

### Task 5: Add `fluentbit` to `SafeConfig`

**Files:**
- Modify: `src/messages/commands.rs`
- Modify: `src/services/mqtt/handlers/get_config.rs`

Now that `FluentbitConfig` exists, surface it in `SafeConfig` (no redaction needed — no secrets in the section).

- [x] **Step 1: Write failing test**

In `src/messages/commands.rs` `mod tests`:

```rust
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
```

- [x] **Step 2: Run failing test**

```bash
cargo test --lib messages::commands::tests::test_safe_config_includes_fluentbit -- --nocapture
```
Expected: compile error — `SafeConfig` has no `fluentbit` field.

- [x] **Step 3: Add `fluentbit` to `SafeConfig`**

In `src/messages/commands.rs`, update the `use` line to:

```rust
use crate::config::{
    CameraConfig, Config, FluentbitConfig, GeneralConfig, MavlinkConfig, MqttConfig, SensorsConfig,
};
```

Update `SafeConfig` to add the field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeConfig {
    pub general: SafeGeneralConfig,
    pub mavlink: MavlinkConfig,
    pub sensors: SensorsConfig,
    pub camera: CameraConfig,
    pub mqtt: SafeMqttConfig,
    pub fluentbit: FluentbitConfig,
}
```

Update `From<&Config> for SafeConfig`:

```rust
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
```

- [x] **Step 4: Run tests**

```bash
cargo test --lib messages::commands::tests
cargo test --lib services::mqtt::handlers::get_config::tests
```
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/messages/commands.rs src/services/mqtt/handlers/get_config.rs
git commit -m "feat(safe-config): expose [fluentbit] in SafeConfig"
```

---

### Task 6: `generate_fluentbit_config()` pure helper

**Files:**
- Create: `src/env/fluentbit_env.rs`
- Modify: `src/env/mod.rs`

Pure function rendering YAML from `&Config`. Caller-side enabled-check; this function only runs when `fluentbit.enabled = true`.

- [x] **Step 1: Create the file with stub + failing test**

Create `src/env/fluentbit_env.rs`:

```rust
use std::sync::Arc;

use tracing::{error, info};

use crate::config::Config;
use crate::context::Context;
use crate::Task;

/// Generate the Fluent Bit YAML config from `config`.
///
/// Caller must ensure `config.fluentbit.enabled == true`. When `fluentbit.tls`
/// is set, all three of `general.{ca,client_cert,client_key}_path` must be
/// `Some(non_empty)`; otherwise this returns `Err`.
pub fn generate_fluentbit_config(config: &Config) -> Result<String, FluentbitGenError> {
    let f = &config.fluentbit;
    let g = &config.general;

    let mut out = String::new();
    out.push_str("service:\n");
    out.push_str("  flush: 1\n");
    out.push_str("  log_level: info\n");
    out.push('\n');
    out.push_str("pipeline:\n");
    out.push_str("  inputs:\n");
    out.push_str("    - name: systemd\n");
    out.push_str("      tag: host.*\n");
    out.push_str("      read_from_tail: off\n");
    if let Some(filters) = &f.systemd_filter {
        if !filters.is_empty() {
            out.push_str("      systemd_filter:\n");
            for entry in filters {
                out.push_str("        - ");
                out.push_str(entry);
                out.push('\n');
            }
        }
    }
    out.push('\n');
    out.push_str("  outputs:\n");
    out.push_str("    - name: forward\n");
    out.push_str("      match: '*'\n");
    out.push_str(&format!("      host: {}\n", f.host));
    out.push_str(&format!("      port: {}\n", f.port));

    if f.tls {
        let ca = g
            .ca_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.ca_cert_path"))?;
        let cert = g
            .client_cert_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.client_cert_path"))?;
        let key = g
            .client_key_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(FluentbitGenError::MissingCert("general.client_key_path"))?;

        out.push_str("      tls: on\n");
        out.push_str(&format!(
            "      tls.verify: {}\n",
            if f.tls_verify { "on" } else { "off" }
        ));
        out.push_str(&format!("      tls.ca_file: {ca}\n"));
        out.push_str(&format!("      tls.crt_file: {cert}\n"));
        out.push_str(&format!("      tls.key_file: {key}\n"));
    }

    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum FluentbitGenError {
    #[error("missing TLS config: {0}")]
    MissingCert(&'static str),
}

pub struct FluentbitEnvWriter {
    ctx: Arc<Context>,
}

impl FluentbitEnvWriter {
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }
}

impl Task for FluentbitEnvWriter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        // Implementation in Task 7.
        let _ = (&self.ctx, error::<()>, info::<()>);
        vec![]
    }
}

fn error<T>() {}
fn info<T>() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;

    fn enabled_config() -> crate::config::Config {
        let mut cfg = test_config();
        cfg.fluentbit.enabled = true;
        cfg.general.ca_cert_path = Some("/etc/ca.pem".to_string());
        cfg.general.client_cert_path = Some("/etc/cert.pem".to_string());
        cfg.general.client_key_path = Some("/etc/key.pem".to_string());
        cfg
    }

    #[test]
    fn test_generate_includes_systemd_input_with_read_from_tail_off() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("- name: systemd"));
        assert!(yaml.contains("read_from_tail: off"));
        assert!(yaml.contains("tag: host.*"));
    }

    #[test]
    fn test_generate_includes_forward_output() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("- name: forward"));
        assert!(yaml.contains("host: logs.example.com"));
        assert!(yaml.contains("port: 24224"));
    }

    #[test]
    fn test_generate_omits_systemd_filter_when_none() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(!yaml.contains("systemd_filter:"));
    }

    #[test]
    fn test_generate_renders_systemd_filter_when_present() {
        let mut cfg = enabled_config();
        cfg.fluentbit.systemd_filter = Some(vec![
            "_SYSTEMD_UNIT=unitctl.service".to_string(),
            "PRIORITY=4".to_string(),
        ]);
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(yaml.contains("systemd_filter:\n        - _SYSTEMD_UNIT=unitctl.service\n        - PRIORITY=4\n"));
    }

    #[test]
    fn test_generate_with_tls_includes_cert_paths() {
        let yaml = generate_fluentbit_config(&enabled_config()).unwrap();
        assert!(yaml.contains("tls: on"));
        assert!(yaml.contains("tls.verify: on"));
        assert!(yaml.contains("tls.ca_file: /etc/ca.pem"));
        assert!(yaml.contains("tls.crt_file: /etc/cert.pem"));
        assert!(yaml.contains("tls.key_file: /etc/key.pem"));
    }

    #[test]
    fn test_generate_with_tls_verify_off() {
        let mut cfg = enabled_config();
        cfg.fluentbit.tls_verify = false;
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(yaml.contains("tls.verify: off"));
    }

    #[test]
    fn test_generate_without_tls_omits_tls_block() {
        let mut cfg = enabled_config();
        cfg.fluentbit.tls = false;
        // Cert paths can even be None now.
        cfg.general.ca_cert_path = None;
        let yaml = generate_fluentbit_config(&cfg).unwrap();
        assert!(!yaml.contains("tls:"));
        assert!(!yaml.contains("tls.verify"));
        assert!(!yaml.contains("tls.ca_file"));
    }

    #[test]
    fn test_generate_with_tls_missing_ca_path_errors() {
        let mut cfg = enabled_config();
        cfg.general.ca_cert_path = None;
        let err = generate_fluentbit_config(&cfg).unwrap_err();
        match err {
            FluentbitGenError::MissingCert(field) => {
                assert_eq!(field, "general.ca_cert_path");
            }
        }
    }

    #[test]
    fn test_generate_with_tls_empty_client_cert_errors() {
        let mut cfg = enabled_config();
        cfg.general.client_cert_path = Some(String::new());
        let err = generate_fluentbit_config(&cfg).unwrap_err();
        match err {
            FluentbitGenError::MissingCert(field) => {
                assert_eq!(field, "general.client_cert_path");
            }
        }
    }
}
```

(The `error::<()>` / `info::<()>` calls in the stub `Task::run` are placeholders silenced by the unused-result lint; they reference unused imports so they're removed in Task 7. Drop them with the unused functions in the next task.)

- [x] **Step 2: Wire up the module**

In `src/env/mod.rs`, replace the contents with:

```rust
pub mod camera_env;
pub mod fluentbit_env;
pub mod mavlink_env;

pub use camera_env::CameraEnvWriter;
pub use fluentbit_env::FluentbitEnvWriter;
pub use mavlink_env::MavlinkEnvWriter;
```

- [x] **Step 3: Run the new tests**

```bash
cargo test --lib env::fluentbit_env
```
Expected: PASS.

- [x] **Step 4: Commit**

```bash
git add src/env/fluentbit_env.rs src/env/mod.rs
git commit -m "feat(env): generate_fluentbit_config helper + module skeleton"
```

---

### Task 7: Implement `FluentbitEnvWriter::run`

**Files:**
- Modify: `src/env/fluentbit_env.rs`

Replaces the placeholder body with the real writer: skip when disabled, render via `generate_fluentbit_config`, write atomically (write to a temp file, then rename) into `config_path`.

- [x] **Step 1: Write failing integration tests**

Append to `mod tests` in `src/env/fluentbit_env.rs`:

```rust
    #[tokio::test]
    async fn test_writer_writes_file_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("- name: systemd"));
        assert!(written.contains("- name: forward"));
    }

    #[tokio::test]
    async fn test_writer_skips_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = test_config();
        cfg.fluentbit.enabled = false;
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(!path.exists(), "no file should be written when disabled");
    }

    #[tokio::test]
    async fn test_writer_skips_when_tls_required_but_cert_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.general.ca_cert_path = None;
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(!path.exists(), "no file should be written when cert missing");
    }

    #[tokio::test]
    async fn test_writer_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/a/b/fluent-bit.conf");

        let mut cfg = enabled_config();
        cfg.fluentbit.config_path = path.to_string_lossy().to_string();

        let ctx = Context::new(cfg);
        let writer = Arc::new(FluentbitEnvWriter::new(ctx));
        for h in writer.run() {
            h.await.unwrap();
        }
        assert!(path.exists());
    }
```

- [x] **Step 2: Run failing tests**

```bash
cargo test --lib env::fluentbit_env::tests::test_writer 2>&1 | tail -20
```
Expected: failures (no file written / panic).

- [x] **Step 3: Replace the stub `Task::run` with the real implementation**

In `src/env/fluentbit_env.rs`, replace the `impl Task for FluentbitEnvWriter` block plus the two stub functions (`fn error<T>()` and `fn info<T>()`) with:

```rust
impl Task for FluentbitEnvWriter {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let ctx = Arc::clone(&self.ctx);
        let handle = tokio::spawn(async move {
            let cfg = &ctx.config;
            if !cfg.fluentbit.enabled {
                info!("fluentbit disabled, skipping config write");
                return;
            }

            let path = cfg.fluentbit.config_path.clone();

            let content = match generate_fluentbit_config(cfg) {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "failed to generate fluentbit config");
                    return;
                }
            };

            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        error!(path = %path, error = %e, "failed to create parent directory for fluentbit config");
                        return;
                    }
                }
            }

            // Atomic write: write to a sibling tmp file, then rename.
            let tmp_path = format!("{path}.tmp");
            if let Err(e) = std::fs::write(&tmp_path, &content) {
                error!(path = %tmp_path, error = %e, "failed to write fluentbit tmp file");
                return;
            }
            match std::fs::rename(&tmp_path, &path) {
                Ok(()) => info!(path = %path, "fluentbit config written"),
                Err(e) => {
                    error!(path = %path, error = %e, "failed to rename fluentbit tmp file");
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
        });
        vec![handle]
    }
}
```

- [x] **Step 4: Run tests**

```bash
cargo test --lib env::fluentbit_env
```
Expected: PASS.

- [x] **Step 5: Lint**

```bash
cargo clippy -- -D warnings
```
Expected: no warnings.

- [x] **Step 6: Commit**

```bash
git add src/env/fluentbit_env.rs
git commit -m "feat(env): FluentbitEnvWriter writes Fluent Bit YAML at startup"
```

---

### Task 8: Wire `FluentbitEnvWriter` into `main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Update imports**

In `src/main.rs`, replace
```rust
use unitctl::env::{CameraEnvWriter, MavlinkEnvWriter};
```
with
```rust
use unitctl::env::{CameraEnvWriter, FluentbitEnvWriter, MavlinkEnvWriter};
```

- [ ] **Step 2: Spawn the writer**

In `src/main.rs`, immediately after the existing `let camera_env = ...; handles.extend(camera_env.run());` block, insert:

```rust
    let fluentbit_env = Arc::new(FluentbitEnvWriter::new(Arc::clone(&ctx)));
    handles.extend(fluentbit_env.run());
```

- [ ] **Step 3: Build**

```bash
cargo build --release
```
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): spawn FluentbitEnvWriter at startup"
```

---

### Task 9: systemd unit files

**Files:**
- Create: `services/fluentbit.service`
- Create: `services/fluentbit-watcher.path`
- Create: `services/fluentbit-watcher.service`

- [ ] **Step 1: Create `services/fluentbit.service`**

```ini
[Unit]
Description=Fluent Bit log forwarder
After=unitctl.service network-online.target
Wants=network-online.target
StartLimitBurst=5
StartLimitIntervalSec=60

[Service]
Type=exec
TimeoutStartSec=30s
ExecStartPre=/bin/sh -c 'until [ -f /etc/fluent-bit.conf ]; do sleep 0.1; done'
ExecStart=/opt/fluent-bit/bin/fluent-bit -c /etc/fluent-bit.conf
Restart=on-failure
RestartSec=1s
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Create `services/fluentbit-watcher.path`**

```ini
[Unit]
Description=Restart fluentbit on config changes
StartLimitIntervalSec=0

[Path]
PathModified=/etc/fluent-bit.conf

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 3: Create `services/fluentbit-watcher.service`**

```ini
[Unit]
Description=Restart fluentbit on config changes
After=network.target
StartLimitIntervalSec=10
StartLimitBurst=20

[Service]
Type=oneshot
ExecStart=/usr/bin/systemctl restart fluentbit

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 4: Verify file shapes**

```bash
ls -la services/fluentbit*
systemd-analyze verify services/fluentbit.service services/fluentbit-watcher.service services/fluentbit-watcher.path 2>&1 | head -30 || true
```
(`systemd-analyze verify` may emit warnings about absolute paths to linked binaries; ignore those.)

- [ ] **Step 5: Commit**

```bash
git add services/fluentbit.service services/fluentbit-watcher.path services/fluentbit-watcher.service
git commit -m "feat(systemd): fluentbit service + path watcher"
```

---

### Task 10: Update `scripts/install.sh`

**Files:**
- Modify: `scripts/install.sh`

Adds Fluent Bit installation (apt repo + package) and systemd unit linking.

- [ ] **Step 1: Add `curl` and `gnupg` to the package list**

In `scripts/install.sh`, replace the body of `install_packages()` with:

```bash
install_packages() {
  apt-get update
  apt-get install -y --no-install-recommends \
        systemd systemd-sysv iputils-ping \
        socat bash ca-certificates curl gnupg \
        gstreamer1.0-tools gstreamer1.0-plugins-base \
        gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
        gstreamer1.0-plugins-ugly gstreamer1.0-libav \
        gstreamer1.0-x \
        libssl3 libdbus-1-3 modemmanager dnsutils rsync

  # Fluent Bit official apt repository (Debian bookworm).
  install -d -m 0755 /usr/share/keyrings
  curl -fsSL https://packages.fluentbit.io/fluentbit.key \
    | gpg --dearmor --yes -o /usr/share/keyrings/fluentbit-keyring.gpg
  echo "deb [signed-by=/usr/share/keyrings/fluentbit-keyring.gpg] https://packages.fluentbit.io/debian/bookworm bookworm main" \
    > /etc/apt/sources.list.d/fluent-bit.list
  apt-get update
  apt-get install -y --no-install-recommends fluent-bit

  rm -rf /var/lib/apt/lists/*
}
```

- [ ] **Step 2: Add fluentbit setup to `install()`**

In `install()`, immediately after the camera services block (after the `systemctl-exists camera-watcher.service ...` block), insert:

```bash
  echo "Setting up fluentbit services..."
  systemctl-exists fluentbit.service || systemctl link ./services/fluentbit.service
  systemctl-exists fluentbit-watcher.service || {
    systemctl link ./services/fluentbit-watcher.service
    systemctl enable ./services/fluentbit-watcher.path
    maybe_start fluentbit-watcher.path
  }
```

- [ ] **Step 3: Add fluentbit teardown to `uninstall()`**

In `uninstall()`, replace the first composite `systemctl disable --now ...` line with:

```bash
    systemctl disable --now mavlink-watcher.path mavlink-restart.timer camera-watcher.path unitctl-watcher.path modem-restart.timer fluentbit-watcher.path || true
```

Add new lines after the `systemctl disable --now camera-watcher.service || true` line:

```bash
    systemctl disable --now fluentbit.service || true
    systemctl disable --now fluentbit-watcher.service || true
```

- [ ] **Step 4: Static-check the script**

```bash
bash -n scripts/install.sh
```
Expected: exit 0 (no syntax errors).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): install Fluent Bit and link fluentbit systemd units"
```

---

### Task 11: Update `config.toml.example` `[fluentbit]` block

**Files:**
- Modify: `config.toml.example`

Cert-path move was done in Task 2. Now add the `[fluentbit]` section.

- [ ] **Step 1: Append fluentbit section**

Append to `config.toml.example`:

```toml

# Fluent Bit log forwarder
# When enabled, unitctl writes a Fluent Bit YAML config to `config_path` at
# startup. The bundled `fluentbit.service` reads /etc/fluent-bit.conf, so set
# `config_path` to that path unless you also override the systemd unit. When
# `tls = true`, `general.{ca,client}_cert_path` and `general.client_key_path`
# must all be set; otherwise the writer skips and an error is logged.
[fluentbit]
enabled = false
host = "logs.example.com"
port = 24224
tls = true
tls_verify = true
config_path = "/etc/fluent-bit.conf"
# Optional list of journald field filters (KEY=VALUE), AND-ed by Fluent Bit.
# systemd_filter = ["_SYSTEMD_UNIT=unitctl.service", "_SYSTEMD_UNIT=mavlink.service"]
```

- [ ] **Step 2: Validate the example parses**

Add a quick smoke test in `src/config.rs` (`mod tests`):

```rust
#[test]
fn test_config_toml_example_parses() {
    let content = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml.example"),
    )
    .expect("read config.toml.example");
    let config: Config = toml::from_str(&content).expect("parse config.toml.example");
    config.validate().expect("validate config.toml.example");
}
```

(If a similar test already exists, skip this step.)

- [ ] **Step 3: Run the smoke test**

```bash
cargo test --lib config::tests::test_config_toml_example_parses
```
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add config.toml.example src/config.rs
git commit -m "docs(config): document [fluentbit] section in config.toml.example"
```

---

### Task 12: Final verification

**Files:**
- (no edits — read-only verification)

- [ ] **Step 1: Full test pass**

```bash
cargo test --all-targets
```
Expected: all PASS.

- [ ] **Step 2: Clippy clean**

```bash
cargo clippy --all-targets -- -D warnings
```
Expected: no warnings.

- [ ] **Step 3: Format check**

```bash
cargo fmt --check
```
Expected: no diffs.

- [ ] **Step 4: Schema regeneration**

```bash
cargo run --bin generate-schema
```
Inspect `assets/schema/` for the new `FluentbitConfig` schema; if the diff is large but expected, commit it:

```bash
git add assets/schema/
git diff --cached --stat
git commit -m "chore(schema): regenerate JSON schemas after [fluentbit] addition"
```

- [ ] **Step 5: Spot-check the install script in a container** (optional, recommended before merging)

```bash
docker run --rm -it -v "$PWD":/work debian:bookworm-slim bash -c \
  "apt-get update && apt-get install -y --no-install-recommends rsync sudo && cd /work && bash -n scripts/install.sh"
```
Expected: exit 0.

---

## Self-Review Notes

- **Spec coverage:**
  - `[fluentbit]` section + validation → Tasks 3 & 4.
  - `FluentbitEnvWriter` (skip-disabled, TLS-missing skip, parent-dir creation, atomic write) → Tasks 6 & 7.
  - YAML body shape (read_from_tail off, systemd_filter rendering, tls.* block, dotted keys) → Task 6.
  - Cert path move + Optional types + per-consumer validation → Tasks 1 & 2.
  - `MqttTransport::new` `MissingTlsConfig` error and `&Config` signature → Task 2.
  - `SafeConfig` redaction in `general` and exposure of `fluentbit` → Tasks 2 & 5.
  - systemd units and install/uninstall hooks → Tasks 9 & 10.
  - `config.toml.example` updates → Tasks 2 & 11.
- **Type consistency:** `FluentbitConfig`, `FluentbitGenError::MissingCert`, `TransportError::MissingTlsConfig { field: &'static str }`, `SafeGeneralConfig` are referenced consistently across tasks.
- **No placeholders.** Every code edit shows the actual replacement.
