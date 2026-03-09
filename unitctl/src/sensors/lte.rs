use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::LteSensorConfig;
use crate::context::Context;

use super::Sensor;

/// Supported modem types for LTE telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModemType {
    Simcom7600,
    QuectelEm12,
    QuectelEm06E,
    QuectelEm06GL,
}

impl fmt::Display for ModemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModemType::Simcom7600 => write!(f, "SIMCOM_7600"),
            ModemType::QuectelEm12 => write!(f, "QUECTEL_EM12"),
            ModemType::QuectelEm06E => write!(f, "QUECTEL_EM06E"),
            ModemType::QuectelEm06GL => write!(f, "QUECTEL_EM06GL"),
        }
    }
}

/// Modem model string to ModemType mapping.
/// Order matters: more specific matches come first (e.g., "EM060K-GL" before "EM06").
const MODEM_IDENTIFIERS: &[(&str, ModemType)] = &[
    ("SIMCOM_SIM7600G-H", ModemType::Simcom7600),
    ("EM12", ModemType::QuectelEm12),
    ("EM060K-GL", ModemType::QuectelEm06GL),
    ("EM06", ModemType::QuectelEm06E),
];

/// Detect modem type from the model string reported by ModemManager.
///
/// Checks if the model string contains any known modem identifier.
/// More specific identifiers are checked first to avoid false matches.
pub fn detect_modem_type(model: &str) -> Option<ModemType> {
    for (identifier, modem_type) in MODEM_IDENTIFIERS {
        if model.contains(identifier) {
            return Some(*modem_type);
        }
    }
    None
}

/// Error type for modem operations.
#[derive(Debug, Clone)]
pub enum ModemError {
    /// D-Bus communication error.
    Dbus(String),
    /// AT command timed out.
    Timeout,
    /// No modem found on the system.
    NoModem,
    /// Modem model not recognized.
    UnsupportedModem(String),
}

impl fmt::Display for ModemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModemError::Dbus(msg) => write!(f, "D-Bus error: {}", msg),
            ModemError::Timeout => write!(f, "AT command timeout"),
            ModemError::NoModem => write!(f, "no modem found"),
            ModemError::UnsupportedModem(model) => {
                write!(f, "unsupported modem model: {}", model)
            }
        }
    }
}

impl std::error::Error for ModemError {}

/// LTE signal quality measurements.
#[derive(Default, Debug, Clone)]
pub struct LteSignalQuality {
    pub rsrq: i32,
    pub rsrp: i32,
    pub rssi: i32,
    pub rssnr: i32,
    pub earfcn: i32,
    pub tx_power: i32,
    pub pcid: i32,
}

/// A neighboring LTE cell.
#[derive(Debug, Clone)]
pub struct LteNeighborCell {
    pub pcid: i32,
    pub rsrp: i32,
    pub rsrq: i32,
    pub rssi: i32,
    pub rssnr: i32,
    pub earfcn: i32,
    pub last_seen: u64,
}

/// Current LTE sensor reading.
#[derive(Default, Debug, Clone)]
pub struct LteReading {
    pub signal: LteSignalQuality,
    pub neighbors: HashMap<i32, LteNeighborCell>,
}

/// Abstraction over modem D-Bus operations.
///
/// This trait allows testing modem-dependent logic without an actual
/// ModemManager D-Bus service. The real implementation uses the
/// `modemmanager` crate (behind the `lte` feature flag).
#[async_trait]
pub trait ModemAccess: Send + Sync {
    /// Get the modem model string.
    async fn model(&self) -> Result<String, ModemError>;

    /// Send an AT command and return the response string.
    async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError>;
}

/// Discover a modem via ModemManager and detect its type.
///
/// Returns the detected ModemType and modem accessor, or an error if
/// no supported modem is found.
pub async fn discover_modem(modem: &dyn ModemAccess) -> Result<ModemType, ModemError> {
    let model = modem.model().await?;
    debug!(model = %model, "modem model detected");

    match detect_modem_type(&model) {
        Some(modem_type) => {
            debug!(modem_type = %modem_type, "modem type identified");
            Ok(modem_type)
        }
        None => Err(ModemError::UnsupportedModem(model)),
    }
}

/// Send an AT command and return the response.
///
/// Wraps the ModemAccess command method with logging and error handling.
pub async fn send_at_command(
    modem: &dyn ModemAccess,
    cmd: &str,
    timeout_ms: u32,
) -> Result<String, ModemError> {
    debug!(cmd = %cmd, timeout_ms = timeout_ms, "sending AT command");
    let response = modem.command(cmd, timeout_ms).await?;
    debug!(cmd = %cmd, response_len = response.len(), "AT command response received");
    Ok(response)
}

// --- Real ModemManager D-Bus implementation ---

pub mod dbus {
    use super::*;
    use modemmanager::dbus::modem::ModemProxy;
    use zbus::Connection;

    /// Real modem accessor using ModemManager D-Bus service.
    pub struct DbusModemAccess {
        connection: Connection,
        modem_path: String,
    }

    impl DbusModemAccess {
        /// Connect to ModemManager and find the first available modem.
        pub async fn discover() -> Result<Self, ModemError> {
            let connection = Connection::system()
                .await
                .map_err(|e| ModemError::Dbus(format!("failed to connect to system bus: {}", e)))?;

            // Use ObjectManager to enumerate modems under /org/freedesktop/ModemManager1
            let proxy = zbus::fdo::ObjectManagerProxy::builder(&connection)
                .destination("org.freedesktop.ModemManager1")
                .map_err(|e| ModemError::Dbus(format!("failed to build proxy: {}", e)))?
                .path("/org/freedesktop/ModemManager1")
                .map_err(|e| ModemError::Dbus(format!("invalid path: {}", e)))?
                .build()
                .await
                .map_err(|e| ModemError::Dbus(format!("failed to create proxy: {}", e)))?;

            let objects = proxy
                .get_managed_objects()
                .await
                .map_err(|e| ModemError::Dbus(format!("failed to enumerate modems: {}", e)))?;

            // Find the first modem object path
            let modem_path = objects
                .keys()
                .find(|path| path.as_str().contains("/Modem/"))
                .ok_or(ModemError::NoModem)?
                .to_string();

            debug!(modem_path = %modem_path, "modem found via D-Bus");

            Ok(Self {
                connection,
                modem_path,
            })
        }

        async fn modem_proxy(&self) -> Result<ModemProxy<'_>, ModemError> {
            zbus::proxy::Builder::<'_, ModemProxy<'_>>::new(&self.connection)
                .destination("org.freedesktop.ModemManager1")
                .map_err(|e| ModemError::Dbus(format!("failed to set destination: {}", e)))?
                .path(self.modem_path.as_str())
                .map_err(|e| ModemError::Dbus(format!("invalid modem path: {}", e)))?
                .build()
                .await
                .map_err(|e| ModemError::Dbus(format!("failed to create modem proxy: {}", e)))
        }
    }

    #[async_trait]
    impl ModemAccess for DbusModemAccess {
        async fn model(&self) -> Result<String, ModemError> {
            let proxy = self.modem_proxy().await?;
            proxy
                .model()
                .await
                .map_err(|e| ModemError::Dbus(format!("failed to read model: {}", e)))
        }

        async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError> {
            let proxy = self.modem_proxy().await?;
            proxy.command(cmd, timeout_ms).await.map_err(|e| {
                let msg = e.to_string();
                if msg.contains("Timeout") || msg.contains("timeout") {
                    ModemError::Timeout
                } else {
                    ModemError::Dbus(format!("AT command failed: {}", e))
                }
            })
        }
    }
}

// --- AT Response Parsing ---

/// Parse a string field as i32, returning 0 for non-numeric values.
fn parse_i32_field(s: &str) -> i32 {
    s.trim().trim_matches('"').parse::<i32>().unwrap_or(0)
}

/// Parse SIMCOM_7600 AT+CPSI? response into signal quality.
///
/// Response format:
/// `+CPSI: mode,status,mcc_mnc,tac,s_cell_id,p_cell_id,band_name,earfcn,dlbw,ulbw,rsrq,rsrp,rssi,rssnr`
///
/// RSSI, RSRP, RSRQ values are divided by 10 (reported in tenths of dBm).
pub fn parse_simcom_cpsi(response: &str) -> Option<LteSignalQuality> {
    let after_colon = response.split(':').nth(1)?;
    let parts: Vec<&str> = after_colon.trim().split(',').collect();
    if parts.len() < 14 {
        return None;
    }
    Some(LteSignalQuality {
        pcid: parse_i32_field(parts[5]),
        earfcn: parse_i32_field(parts[7]),
        rsrq: parse_i32_field(parts[10]) / 10,
        rsrp: parse_i32_field(parts[11]) / 10,
        rssi: parse_i32_field(parts[12]) / 10,
        rssnr: parse_i32_field(parts[13]),
        tx_power: 0, // Not available from SIMCOM
    })
}

/// Parse Quectel EM12 AT+QENG="servingcell" response.
///
/// Response format (20 comma-separated fields):
/// `+QENG: "servingcell","state","mode","is_tdd",mcc,mnc,cell_id,pcid,earfcn,
///   freq_band,ul_bw,dl_bw,tac,rsrp,rsrq,rssi,rssnr,cqi,tx_power,srxlev`
pub fn parse_quectel_em12_serving(response: &str) -> Option<LteSignalQuality> {
    if !response.contains("servingcell") {
        return None;
    }
    let parts: Vec<&str> = response.split(',').collect();
    if parts.len() < 20 {
        return None;
    }
    Some(LteSignalQuality {
        pcid: parse_i32_field(parts[7]),
        earfcn: parse_i32_field(parts[8]),
        rsrp: parse_i32_field(parts[13]),
        rsrq: parse_i32_field(parts[14]),
        rssi: parse_i32_field(parts[15]),
        rssnr: parse_i32_field(parts[16]),
        tx_power: parse_i32_field(parts[18]),
    })
}

/// Parse Quectel EM06GL AT+QENG="servingcell" response.
///
/// Same as EM12 but without cqi field (19 fields total).
/// `+QENG: "servingcell","state","mode","is_tdd",mcc,mnc,cell_id,pcid,earfcn,
///   freq_band,ul_bw,dl_bw,tac,rsrp,rsrq,rssi,rssnr,tx_power,srxlev`
pub fn parse_quectel_em06gl_serving(response: &str) -> Option<LteSignalQuality> {
    if !response.contains("servingcell") {
        return None;
    }
    let parts: Vec<&str> = response.split(',').collect();
    if parts.len() < 19 {
        return None;
    }
    Some(LteSignalQuality {
        pcid: parse_i32_field(parts[7]),
        earfcn: parse_i32_field(parts[8]),
        rsrp: parse_i32_field(parts[13]),
        rsrq: parse_i32_field(parts[14]),
        rssi: parse_i32_field(parts[15]),
        rssnr: parse_i32_field(parts[16]),
        tx_power: parse_i32_field(parts[17]),
    })
}

/// Parse Quectel EM06E AT+QENG="servingcell" response.
///
/// Same as EM12 but without cqi and tx_power fields (18 fields total).
/// `+QENG: "servingcell","state","mode","is_tdd",mcc,mnc,cell_id,pcid,earfcn,
///   freq_band,ul_bw,dl_bw,tac,rsrp,rsrq,rssi,rssnr,srxlev`
pub fn parse_quectel_em06e_serving(response: &str) -> Option<LteSignalQuality> {
    if !response.contains("servingcell") {
        return None;
    }
    let parts: Vec<&str> = response.split(',').collect();
    if parts.len() < 18 {
        return None;
    }
    Some(LteSignalQuality {
        pcid: parse_i32_field(parts[7]),
        earfcn: parse_i32_field(parts[8]),
        rsrp: parse_i32_field(parts[13]),
        rsrq: parse_i32_field(parts[14]),
        rssi: parse_i32_field(parts[15]),
        rssnr: parse_i32_field(parts[16]),
        tx_power: 0, // Not available from EM06E
    })
}

/// Parse a Quectel neighbor cell response line.
///
/// Response format:
/// `+QENG: "neighbourcell intra",mode,earfcn,pcid,rsrq,rsrp,rssi,rssnr[,...]`
///
/// Returns None if the line doesn't contain enough fields.
/// The `last_seen` field is set to 0; the caller should set it to the current time.
pub fn parse_quectel_neighbor(line: &str) -> Option<LteNeighborCell> {
    if !line.contains("neighbourcell") {
        return None;
    }
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() < 8 {
        return None;
    }
    Some(LteNeighborCell {
        pcid: parse_i32_field(parts[3]),
        earfcn: parse_i32_field(parts[2]),
        rsrq: parse_i32_field(parts[4]),
        rsrp: parse_i32_field(parts[5]),
        rssi: parse_i32_field(parts[6]),
        rssnr: parse_i32_field(parts[7]),
        last_seen: 0, // Caller sets this
    })
}

/// Dispatch serving cell parsing based on modem type.
pub fn parse_serving_cell(modem_type: ModemType, response: &str) -> Option<LteSignalQuality> {
    match modem_type {
        ModemType::Simcom7600 => parse_simcom_cpsi(response),
        ModemType::QuectelEm12 => parse_quectel_em12_serving(response),
        ModemType::QuectelEm06GL => parse_quectel_em06gl_serving(response),
        ModemType::QuectelEm06E => parse_quectel_em06e_serving(response),
    }
}

/// Remove neighbor cells not seen for longer than `expiry_s` seconds.
pub fn expire_neighbors(neighbors: &mut HashMap<i32, LteNeighborCell>, expiry_s: f64, now: u64) {
    neighbors.retain(|_, cell| (now.saturating_sub(cell.last_seen) as f64) <= expiry_s);
}

/// Get current time as unix epoch seconds.
fn current_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns the AT command for serving cell query based on modem type.
fn serving_cell_command(modem_type: ModemType) -> &'static str {
    match modem_type {
        ModemType::Simcom7600 => "AT+CPSI?",
        ModemType::QuectelEm12 | ModemType::QuectelEm06E | ModemType::QuectelEm06GL => {
            "AT+QENG=\"servingcell\""
        }
    }
}

/// Returns true if this modem type supports neighbor cell queries.
fn supports_neighbor_query(modem_type: ModemType) -> bool {
    matches!(modem_type, ModemType::QuectelEm12)
}

// --- LteSensor ---

/// LTE telemetry sensor that discovers a modem via ModemManager,
/// polls signal quality via AT commands, and stores results in Context.
pub struct LteSensor {
    interval: Duration,
    neighbor_expiry_s: f64,
}

impl LteSensor {
    pub fn new(config: &LteSensorConfig, default_interval: f64) -> Self {
        let interval_s = config.interval_s.unwrap_or(default_interval);
        Self {
            interval: Duration::from_secs_f64(interval_s),
            neighbor_expiry_s: config.neighbor_expiry_s,
        }
    }

    /// Poll a modem for signal quality. Separated from `run` for testability.
    pub(crate) async fn poll_loop(
        &self,
        modem: &dyn ModemAccess,
        ctx: &Arc<Context>,
        cancel: &CancellationToken,
    ) {
        let modem_type = match discover_modem(modem).await {
            Ok(t) => {
                info!(modem_type = %t, "modem type identified, starting polling");
                t
            }
            Err(e) => {
                warn!(error = %e, "modem type detection failed");
                return;
            }
        };

        let mut neighbors: HashMap<i32, LteNeighborCell> = HashMap::new();
        let mut at_counter: u64 = 1; // Start at 1 so first query (counter=2) is serving cell
        let has_neighbors = supports_neighbor_query(modem_type);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(self.interval) => {}
            }

            at_counter += 1;

            // For EM12: alternate between serving cell and neighbor cell queries
            let is_neighbor_query = has_neighbors && !at_counter.is_multiple_of(2);

            if is_neighbor_query {
                match send_at_command(modem, "AT+QENG=\"neighbourcell\"", 3000).await {
                    Ok(resp) => {
                        let now = current_time_secs();
                        let serving_pcid = ctx
                            .sensors
                            .lte
                            .read()
                            .await
                            .as_ref()
                            .map(|r| r.signal.pcid)
                            .unwrap_or(-1);

                        for line in resp.lines() {
                            if let Some(mut cell) = parse_quectel_neighbor(line) {
                                // Skip the serving cell
                                if cell.pcid != serving_pcid {
                                    cell.last_seen = now;
                                    neighbors.insert(cell.pcid, cell);
                                }
                            }
                        }
                        expire_neighbors(&mut neighbors, self.neighbor_expiry_s, now);

                        // Update neighbors in context
                        if let Some(ref mut reading) = *ctx.sensors.lte.write().await {
                            reading.neighbors = neighbors.clone();
                        }
                        debug!(neighbor_count = neighbors.len(), "neighbor cells updated");
                    }
                    Err(e) => {
                        warn!(error = %e, "neighbor cell query failed");
                    }
                }
            } else {
                // Serving cell query
                let cmd = serving_cell_command(modem_type);
                match send_at_command(modem, cmd, 3000).await {
                    Ok(resp) => {
                        if let Some(signal) = parse_serving_cell(modem_type, &resp) {
                            debug!(
                                rsrp = signal.rsrp,
                                rsrq = signal.rsrq,
                                rssi = signal.rssi,
                                pcid = signal.pcid,
                                "serving cell updated"
                            );
                            let reading = LteReading {
                                signal,
                                neighbors: neighbors.clone(),
                            };
                            *ctx.sensors.lte.write().await = Some(reading);
                        } else {
                            debug!(response = %resp, "failed to parse serving cell response");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "serving cell query failed");
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Sensor for LteSensor {
    fn name(&self) -> &str {
        "lte"
    }

    async fn run(&self, _ctx: Arc<Context>, cancel: CancellationToken) {
        loop {
            info!("attempting modem discovery via D-Bus");
            match dbus::DbusModemAccess::discover().await {
                Ok(modem) => {
                    self.poll_loop(&modem, &_ctx, &cancel).await;
                    if cancel.is_cancelled() {
                        return;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "modem discovery failed");
                }
            }

            // Retry delay on failure
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // --- Mock ModemAccess for testing ---

    struct MockModem {
        model_response: Result<String, ModemError>,
        command_responses: Mutex<Vec<Result<String, ModemError>>>,
    }

    impl MockModem {
        fn with_model(model: &str) -> Self {
            Self {
                model_response: Ok(model.to_string()),
                command_responses: Mutex::new(Vec::new()),
            }
        }

        fn with_model_error(err: ModemError) -> Self {
            Self {
                model_response: Err(err),
                command_responses: Mutex::new(Vec::new()),
            }
        }

        fn with_command_response(self, response: Result<String, ModemError>) -> Self {
            self.command_responses.lock().unwrap().push(response);
            self
        }
    }

    #[async_trait]
    impl ModemAccess for MockModem {
        async fn model(&self) -> Result<String, ModemError> {
            match &self.model_response {
                Ok(m) => Ok(m.clone()),
                Err(e) => Err(e.clone()),
            }
        }

        async fn command(&self, _cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
            let mut responses = self.command_responses.lock().unwrap();
            if responses.is_empty() {
                return Err(ModemError::Dbus("no mock response configured".to_string()));
            }
            responses.remove(0)
        }
    }

    // --- ModemType detection tests ---

    #[test]
    fn test_detect_simcom_7600() {
        assert_eq!(
            detect_modem_type("SIMCOM_SIM7600G-H"),
            Some(ModemType::Simcom7600)
        );
    }

    #[test]
    fn test_detect_quectel_em12() {
        assert_eq!(detect_modem_type("EM12"), Some(ModemType::QuectelEm12));
    }

    #[test]
    fn test_detect_quectel_em06e() {
        assert_eq!(detect_modem_type("EM06"), Some(ModemType::QuectelEm06E));
    }

    #[test]
    fn test_detect_quectel_em06gl() {
        assert_eq!(
            detect_modem_type("EM060K-GL"),
            Some(ModemType::QuectelEm06GL)
        );
    }

    #[test]
    fn test_detect_unknown_model() {
        assert_eq!(detect_modem_type("UNKNOWN_MODEM_XYZ"), None);
    }

    #[test]
    fn test_detect_empty_model() {
        assert_eq!(detect_modem_type(""), None);
    }

    #[test]
    fn test_detect_em06gl_before_em06e() {
        // "EM060K-GL" contains "EM06" but should match EM06GL first
        // because MODEM_IDENTIFIERS checks EM060K-GL before EM06
        assert_eq!(
            detect_modem_type("EM060K-GL"),
            Some(ModemType::QuectelEm06GL)
        );
    }

    #[test]
    fn test_detect_model_with_extra_text() {
        // ModemManager might return model with extra whitespace or prefix
        assert_eq!(
            detect_modem_type("Quectel EM12-G"),
            Some(ModemType::QuectelEm12)
        );
    }

    #[test]
    fn test_detect_partial_match_simcom() {
        assert_eq!(
            detect_modem_type("Some prefix SIMCOM_SIM7600G-H Rev1.0"),
            Some(ModemType::Simcom7600)
        );
    }

    #[test]
    fn test_modem_type_display() {
        assert_eq!(format!("{}", ModemType::Simcom7600), "SIMCOM_7600");
        assert_eq!(format!("{}", ModemType::QuectelEm12), "QUECTEL_EM12");
        assert_eq!(format!("{}", ModemType::QuectelEm06E), "QUECTEL_EM06E");
        assert_eq!(format!("{}", ModemType::QuectelEm06GL), "QUECTEL_EM06GL");
    }

    // --- discover_modem tests ---

    #[tokio::test]
    async fn test_discover_modem_simcom() {
        let mock = MockModem::with_model("SIMCOM_SIM7600G-H");
        let result = discover_modem(&mock).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ModemType::Simcom7600);
    }

    #[tokio::test]
    async fn test_discover_modem_quectel_em12() {
        let mock = MockModem::with_model("EM12");
        let result = discover_modem(&mock).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ModemType::QuectelEm12);
    }

    #[tokio::test]
    async fn test_discover_modem_unsupported() {
        let mock = MockModem::with_model("UNKNOWN_MODEM");
        let result = discover_modem(&mock).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ModemError::UnsupportedModem(model) => assert_eq!(model, "UNKNOWN_MODEM"),
            e => panic!("expected UnsupportedModem, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_discover_modem_dbus_error() {
        let mock = MockModem::with_model_error(ModemError::Dbus("connection refused".into()));
        let result = discover_modem(&mock).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ModemError::Dbus(_) => {}
            e => panic!("expected Dbus error, got: {:?}", e),
        }
    }

    // --- AT command execution tests ---

    #[tokio::test]
    async fn test_at_command_success() {
        let mock = MockModem::with_model("EM12")
            .with_command_response(Ok("+QENG: \"servingcell\",\"NOCONN\"".to_string()));
        let result = send_at_command(&mock, "AT+QENG=\"servingcell\"", 3000).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "+QENG: \"servingcell\",\"NOCONN\"");
    }

    #[tokio::test]
    async fn test_at_command_timeout() {
        let mock = MockModem::with_model("EM12").with_command_response(Err(ModemError::Timeout));
        let result = send_at_command(&mock, "AT+QENG=\"servingcell\"", 3000).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ModemError::Timeout => {}
            e => panic!("expected Timeout, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_at_command_dbus_error() {
        let mock = MockModem::with_model("EM12")
            .with_command_response(Err(ModemError::Dbus("method call failed".into())));
        let result = send_at_command(&mock, "AT+CPSI?", 3000).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ModemError::Dbus(msg) => assert!(msg.contains("method call failed")),
            e => panic!("expected Dbus error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_at_command_empty_response() {
        let mock = MockModem::with_model("EM12").with_command_response(Ok(String::new()));
        let result = send_at_command(&mock, "AT", 3000).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");
    }

    // --- ModemError display tests ---

    #[test]
    fn test_modem_error_display() {
        assert_eq!(
            format!("{}", ModemError::Dbus("test".into())),
            "D-Bus error: test"
        );
        assert_eq!(format!("{}", ModemError::Timeout), "AT command timeout");
        assert_eq!(format!("{}", ModemError::NoModem), "no modem found");
        assert_eq!(
            format!("{}", ModemError::UnsupportedModem("FOO".into())),
            "unsupported modem model: FOO"
        );
    }

    // --- SIMCOM_7600 AT+CPSI? parsing tests ---

    #[test]
    fn test_parse_simcom_cpsi_valid() {
        // Real-world format: +CPSI: LTE,Online,460-11,0x1234,12345678,311,EUTRAN-BAND3,1850,5,5,-115,-830,-530,18
        let response =
            "+CPSI: LTE,Online,460-11,0x1234,12345678,311,EUTRAN-BAND3,1850,5,5,-115,-830,-530,18";
        let signal = parse_simcom_cpsi(response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.earfcn, 1850);
        assert_eq!(signal.rsrq, -11); // -115 / 10
        assert_eq!(signal.rsrp, -83); // -830 / 10
        assert_eq!(signal.rssi, -53); // -530 / 10
        assert_eq!(signal.rssnr, 18);
        assert_eq!(signal.tx_power, 0); // Not available from SIMCOM
    }

    #[test]
    fn test_parse_simcom_cpsi_partial_data() {
        // Some fields may be non-numeric (e.g., "-" or empty)
        let response = "+CPSI: LTE,Online,460-11,0x1234,12345678,311,EUTRAN-BAND3,1850,5,5,-,-,-,0";
        let signal = parse_simcom_cpsi(response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.earfcn, 1850);
        assert_eq!(signal.rsrq, 0); // "-" parses to 0
        assert_eq!(signal.rsrp, 0);
        assert_eq!(signal.rssi, 0);
        assert_eq!(signal.rssnr, 0);
    }

    #[test]
    fn test_parse_simcom_cpsi_too_few_fields() {
        let response = "+CPSI: LTE,Online,460-11";
        assert!(parse_simcom_cpsi(response).is_none());
    }

    #[test]
    fn test_parse_simcom_cpsi_no_colon() {
        let response = "GARBAGE DATA";
        assert!(parse_simcom_cpsi(response).is_none());
    }

    #[test]
    fn test_parse_simcom_cpsi_empty() {
        assert!(parse_simcom_cpsi("").is_none());
    }

    // --- QUECTEL_EM12 serving cell parsing tests ---

    #[test]
    fn test_parse_em12_serving_valid() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,4,-32768,30";
        let signal = parse_quectel_em12_serving(response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.earfcn, 5110);
        assert_eq!(signal.rsrp, -83);
        assert_eq!(signal.rsrq, -7);
        assert_eq!(signal.rssi, -53);
        assert_eq!(signal.rssnr, 18);
        assert_eq!(signal.tx_power, -32768);
    }

    #[test]
    fn test_parse_em12_serving_connected() {
        let response = "+QENG: \"servingcell\",\"CONN\",\"LTE\",\"FDD\",310,410,1A2D001,100,66986,66,3,3,0001,-95,-12,-62,10,15,200,25";
        let signal = parse_quectel_em12_serving(response).unwrap();
        assert_eq!(signal.pcid, 100);
        assert_eq!(signal.earfcn, 66986);
        assert_eq!(signal.rsrp, -95);
        assert_eq!(signal.rsrq, -12);
        assert_eq!(signal.rssi, -62);
        assert_eq!(signal.rssnr, 10);
        assert_eq!(signal.tx_power, 200);
    }

    #[test]
    fn test_parse_em12_serving_too_few_fields() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\"";
        assert!(parse_quectel_em12_serving(response).is_none());
    }

    #[test]
    fn test_parse_em12_serving_not_serving() {
        // A neighbourcell response should not parse as serving
        let response = "+QENG: \"neighbourcell intra\",\"LTE\",5110,311,-7,-83,-53,18";
        assert!(parse_quectel_em12_serving(response).is_none());
    }

    // --- QUECTEL_EM12 neighbor cell parsing tests ---

    #[test]
    fn test_parse_quectel_neighbor_valid() {
        let response =
            "+QENG: \"neighbourcell intra\",\"LTE\",5110,312,-8,-85,-55,15,0,0,0,0,-,-,-,-";
        let cell = parse_quectel_neighbor(response).unwrap();
        assert_eq!(cell.pcid, 312);
        assert_eq!(cell.earfcn, 5110);
        assert_eq!(cell.rsrq, -8);
        assert_eq!(cell.rsrp, -85);
        assert_eq!(cell.rssi, -55);
        assert_eq!(cell.rssnr, 15);
        assert_eq!(cell.last_seen, 0); // Caller should set this
    }

    #[test]
    fn test_parse_quectel_neighbor_too_few_fields() {
        let response = "+QENG: \"neighbourcell intra\",\"LTE\",5110";
        assert!(parse_quectel_neighbor(response).is_none());
    }

    #[test]
    fn test_parse_quectel_neighbor_not_neighbor() {
        // A servingcell response should not parse as neighbor
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,4,-32768,30";
        assert!(parse_quectel_neighbor(response).is_none());
    }

    #[test]
    fn test_parse_quectel_neighbor_multiple_lines() {
        let response = "+QENG: \"neighbourcell intra\",\"LTE\",5110,312,-8,-85,-55,15\n+QENG: \"neighbourcell intra\",\"LTE\",5110,313,-10,-90,-60,12";
        let cells: Vec<_> = response
            .lines()
            .filter_map(parse_quectel_neighbor)
            .collect();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].pcid, 312);
        assert_eq!(cells[1].pcid, 313);
    }

    // --- Neighbor cell expiry tests ---

    #[test]
    fn test_expire_neighbors_removes_old() {
        let mut neighbors = HashMap::new();
        neighbors.insert(
            100,
            LteNeighborCell {
                pcid: 100,
                rsrp: -85,
                rsrq: -8,
                rssi: -55,
                rssnr: 15,
                earfcn: 5110,
                last_seen: 1000,
            },
        );
        neighbors.insert(
            200,
            LteNeighborCell {
                pcid: 200,
                rsrp: -90,
                rsrq: -10,
                rssi: -60,
                rssnr: 12,
                earfcn: 5110,
                last_seen: 1025,
            },
        );

        // At time 1035, with 30s expiry: cell 100 (last_seen=1000) is 35s old -> expired
        // Cell 200 (last_seen=1025) is 10s old -> kept
        expire_neighbors(&mut neighbors, 30.0, 1035);
        assert_eq!(neighbors.len(), 1);
        assert!(neighbors.contains_key(&200));
        assert!(!neighbors.contains_key(&100));
    }

    #[test]
    fn test_expire_neighbors_keeps_recent() {
        let mut neighbors = HashMap::new();
        neighbors.insert(
            100,
            LteNeighborCell {
                pcid: 100,
                rsrp: -85,
                rsrq: -8,
                rssi: -55,
                rssnr: 15,
                earfcn: 5110,
                last_seen: 1000,
            },
        );

        // At time 1010, with 30s expiry: cell is only 10s old -> kept
        expire_neighbors(&mut neighbors, 30.0, 1010);
        assert_eq!(neighbors.len(), 1);
    }

    #[test]
    fn test_expire_neighbors_boundary() {
        let mut neighbors = HashMap::new();
        neighbors.insert(
            100,
            LteNeighborCell {
                pcid: 100,
                rsrp: -85,
                rsrq: -8,
                rssi: -55,
                rssnr: 15,
                earfcn: 5110,
                last_seen: 1000,
            },
        );

        // Exactly at expiry boundary: 30s old with 30s expiry -> kept (<=)
        expire_neighbors(&mut neighbors, 30.0, 1030);
        assert_eq!(neighbors.len(), 1);

        // 31s old -> expired
        expire_neighbors(&mut neighbors, 30.0, 1031);
        assert_eq!(neighbors.len(), 0);
    }

    #[test]
    fn test_expire_neighbors_empty() {
        let mut neighbors: HashMap<i32, LteNeighborCell> = HashMap::new();
        expire_neighbors(&mut neighbors, 30.0, 1000);
        assert!(neighbors.is_empty());
    }

    // --- QUECTEL_EM06E parsing tests ---

    #[test]
    fn test_parse_em06e_serving_valid() {
        // EM06E: 18 fields, no cqi/tx_power
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,30";
        let signal = parse_quectel_em06e_serving(response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.earfcn, 5110);
        assert_eq!(signal.rsrp, -83);
        assert_eq!(signal.rsrq, -7);
        assert_eq!(signal.rssi, -53);
        assert_eq!(signal.rssnr, 18);
        assert_eq!(signal.tx_power, 0); // Not available from EM06E
    }

    #[test]
    fn test_parse_em06e_serving_too_few_fields() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\"";
        assert!(parse_quectel_em06e_serving(response).is_none());
    }

    // --- QUECTEL_EM06GL parsing tests ---

    #[test]
    fn test_parse_em06gl_serving_valid() {
        // EM06GL: 19 fields, no cqi but has tx_power
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,200,30";
        let signal = parse_quectel_em06gl_serving(response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.earfcn, 5110);
        assert_eq!(signal.rsrp, -83);
        assert_eq!(signal.rsrq, -7);
        assert_eq!(signal.rssi, -53);
        assert_eq!(signal.rssnr, 18);
        assert_eq!(signal.tx_power, 200);
    }

    #[test]
    fn test_parse_em06gl_serving_too_few_fields() {
        let response = "+QENG: \"servingcell\",\"NOCONN\"";
        assert!(parse_quectel_em06gl_serving(response).is_none());
    }

    // --- parse_serving_cell dispatch tests ---

    #[test]
    fn test_parse_serving_cell_dispatches_simcom() {
        let response =
            "+CPSI: LTE,Online,460-11,0x1234,12345678,311,EUTRAN-BAND3,1850,5,5,-115,-830,-530,18";
        let signal = parse_serving_cell(ModemType::Simcom7600, response).unwrap();
        assert_eq!(signal.pcid, 311);
    }

    #[test]
    fn test_parse_serving_cell_dispatches_em12() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,4,-32768,30";
        let signal = parse_serving_cell(ModemType::QuectelEm12, response).unwrap();
        assert_eq!(signal.pcid, 311);
    }

    #[test]
    fn test_parse_serving_cell_dispatches_em06gl() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,200,30";
        let signal = parse_serving_cell(ModemType::QuectelEm06GL, response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.tx_power, 200);
    }

    #[test]
    fn test_parse_serving_cell_dispatches_em06e() {
        let response = "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,30";
        let signal = parse_serving_cell(ModemType::QuectelEm06E, response).unwrap();
        assert_eq!(signal.pcid, 311);
        assert_eq!(signal.tx_power, 0);
    }

    // --- Helper function tests ---

    #[test]
    fn test_parse_i32_field_valid() {
        assert_eq!(parse_i32_field("-83"), -83);
        assert_eq!(parse_i32_field("5110"), 5110);
        assert_eq!(parse_i32_field("0"), 0);
        assert_eq!(parse_i32_field(" -32768 "), -32768);
    }

    #[test]
    fn test_parse_i32_field_invalid() {
        assert_eq!(parse_i32_field("-"), 0);
        assert_eq!(parse_i32_field(""), 0);
        assert_eq!(parse_i32_field("abc"), 0);
        assert_eq!(parse_i32_field("00A2"), 0); // hex not supported
    }

    #[test]
    fn test_parse_i32_field_quoted() {
        // Some fields come with quotes from AT responses
        assert_eq!(parse_i32_field("\"311\""), 311);
        assert_eq!(parse_i32_field("\"LTE\""), 0);
    }

    #[test]
    fn test_serving_cell_command() {
        assert_eq!(serving_cell_command(ModemType::Simcom7600), "AT+CPSI?");
        assert_eq!(
            serving_cell_command(ModemType::QuectelEm12),
            "AT+QENG=\"servingcell\""
        );
        assert_eq!(
            serving_cell_command(ModemType::QuectelEm06E),
            "AT+QENG=\"servingcell\""
        );
        assert_eq!(
            serving_cell_command(ModemType::QuectelEm06GL),
            "AT+QENG=\"servingcell\""
        );
    }

    #[test]
    fn test_supports_neighbor_query() {
        assert!(supports_neighbor_query(ModemType::QuectelEm12));
        assert!(!supports_neighbor_query(ModemType::Simcom7600));
        assert!(!supports_neighbor_query(ModemType::QuectelEm06E));
        assert!(!supports_neighbor_query(ModemType::QuectelEm06GL));
    }

    // --- LteSensor construction tests ---

    #[test]
    fn test_lte_sensor_new_with_defaults() {
        let config = LteSensorConfig {
            enabled: true,
            interval_s: None,
            neighbor_expiry_s: 30.0,
        };
        let sensor = LteSensor::new(&config, 1.0);
        assert_eq!(sensor.interval, Duration::from_secs_f64(1.0));
        assert_eq!(sensor.neighbor_expiry_s, 30.0);
    }

    #[test]
    fn test_lte_sensor_new_with_override() {
        let config = LteSensorConfig {
            enabled: true,
            interval_s: Some(2.0),
            neighbor_expiry_s: 60.0,
        };
        let sensor = LteSensor::new(&config, 1.0);
        assert_eq!(sensor.interval, Duration::from_secs_f64(2.0));
        assert_eq!(sensor.neighbor_expiry_s, 60.0);
    }

    #[test]
    fn test_lte_sensor_name() {
        let config = LteSensorConfig::default();
        let sensor = LteSensor::new(&config, 1.0);
        assert_eq!(sensor.name(), "lte");
    }

    // --- LteSensor poll_loop tests ---

    #[tokio::test]
    async fn test_poll_loop_serving_cell_update() {
        let config = crate::config::tests::test_config();
        let ctx = crate::context::Context::new(config);
        let cancel = CancellationToken::new();

        // EM12 alternates: first query (counter=2, even) is serving, second (counter=3, odd) is neighbor.
        // Provide serving cell response first, then neighbor response.
        let mock = MockModem::with_model("EM12")
            .with_command_response(Ok(
                "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",310,260,4E00001,311,5110,10,5,5,00A2,-83,-7,-53,18,4,-32768,30".to_string(),
            ))
            .with_command_response(Ok(
                "+QENG: \"neighbourcell intra\",\"LTE\",5110,999,-8,-85,-55,15".to_string(),
            ));

        let sensor = LteSensor::new(&LteSensorConfig::default(), 0.01);

        let cancel_clone = cancel.clone();
        let ctx_clone = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            sensor.poll_loop(&mock, &ctx_clone, &cancel_clone).await;
        });

        // Wait for at least two poll cycles (neighbor + serving)
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        let _ = handle.await;

        let reading = ctx.sensors.lte.read().await;
        assert!(reading.is_some());
        let reading = reading.as_ref().unwrap();
        assert_eq!(reading.signal.pcid, 311);
        assert_eq!(reading.signal.earfcn, 5110);
        assert_eq!(reading.signal.rsrp, -83);
        // Neighbor cell 999 should be tracked (different from serving pcid 311)
        assert!(reading.neighbors.contains_key(&999));
    }

    #[tokio::test]
    async fn test_poll_loop_unsupported_modem_exits() {
        let config = crate::config::tests::test_config();
        let ctx = crate::context::Context::new(config);
        let cancel = CancellationToken::new();

        let mock = MockModem::with_model("UNKNOWN_MODEM");

        let sensor = LteSensor::new(&LteSensorConfig::default(), 0.01);

        // poll_loop should return quickly when modem type is unsupported
        sensor.poll_loop(&mock, &ctx, &cancel).await;

        let reading = ctx.sensors.lte.read().await;
        assert!(reading.is_none());
    }
}
