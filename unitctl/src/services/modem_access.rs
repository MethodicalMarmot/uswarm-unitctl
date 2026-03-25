use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

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
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModemError {
    /// D-Bus communication error.
    #[error("D-Bus error: {0}")]
    Dbus(String),
    /// AT command timed out.
    #[error("AT command timeout")]
    Timeout,
    /// No modem found on the system.
    #[error("no modem found")]
    NoModem,
    /// Modem model not recognized.
    #[error("unsupported modem model: {0}")]
    UnsupportedModem(String),
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

    /// Read SIM IMSI via AT+CIMI.
    async fn imsi(&self) -> Result<String, ModemError> {
        let resp = self.command("AT+CIMI", 3000).await?;
        Ok(resp.trim().to_string())
    }

    /// Query network registration status via AT+CREG? or AT+CEREG?.
    async fn registration_status(&self) -> Result<NetworkRegistration, ModemError> {
        // Try EPS (4G) registration first
        if let Ok(resp) = self.command("AT+CEREG?", 3000).await {
            if let Some(status) = parse_registration_response(&resp) {
                return Ok(status);
            }
        }
        // Fall back to circuit-switched registration
        let resp = self.command("AT+CREG?", 3000).await?;
        parse_registration_response(&resp)
            .ok_or_else(|| ModemError::Dbus("failed to parse registration status".into()))
    }
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

/// Network registration status returned by the modem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRegistration {
    NotRegistered,
    RegisteredHome,
    Searching,
    Denied,
    Unknown,
    RegisteredRoaming,
}

impl NetworkRegistration {
    /// Parse a numeric registration status code (from AT+CREG? or AT+CEREG?).
    fn from_code(code: u8) -> Self {
        match code {
            0 => NetworkRegistration::NotRegistered,
            1 => NetworkRegistration::RegisteredHome,
            2 => NetworkRegistration::Searching,
            3 => NetworkRegistration::Denied,
            5 => NetworkRegistration::RegisteredRoaming,
            _ => NetworkRegistration::Unknown,
        }
    }
}

/// Request submitted to the modem worker queue.
enum ModemRequest {
    Model {
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    Command {
        cmd: String,
        timeout_ms: u32,
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    /// Read SIM IMSI via AT+CIMI.
    Imsi {
        reply: oneshot::Sender<Result<String, ModemError>>,
    },
    /// Query network registration status via AT+CREG? or AT+CEREG?.
    RegistrationStatus {
        reply: oneshot::Sender<Result<NetworkRegistration, ModemError>>,
    },
}

/// Queue-based modem access proxy. Implements ModemAccess.
/// Safe to share across threads — requests are serialized by the worker.
#[derive(Clone)]
pub struct ModemAccessService {
    tx: mpsc::Sender<ModemRequest>,
}

/// Modem discovery retry interval.
const DISCOVERY_RETRY_SECS: u64 = 5;

/// Channel capacity for the request queue.
const REQUEST_QUEUE_CAPACITY: usize = 32;

impl ModemAccessService {
    /// Discover modem (with retry), spawn worker, return service handle.
    pub async fn start(cancel: &CancellationToken) -> Result<Arc<Self>, ModemError> {
        let modem = Self::discover_with_retry(cancel).await?;
        let (tx, rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            Self::worker_loop(rx, modem, cancel_clone).await;
        });

        Ok(Arc::new(Self { tx }))
    }

    /// Create a service wrapping an existing ModemAccess implementation.
    /// Useful for testing — skips D-Bus discovery.
    #[cfg(test)]
    pub fn with_backend(backend: Box<dyn ModemAccess>, cancel: &CancellationToken) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            Self::worker_loop(rx, backend, cancel_clone).await;
        });

        Arc::new(Self { tx })
    }

    /// Discover modem via D-Bus, retrying until success or cancellation.
    ///
    /// Retries both D-Bus enumeration failures and modem identification failures
    /// (e.g., transient D-Bus read errors or no supported modem found yet).
    /// On hosts with multiple modems, tries each one before retrying.
    async fn discover_with_retry(
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ModemAccess>, ModemError> {
        loop {
            let retry_reason = match dbus::DbusModemAccess::discover_all().await {
                Ok(modems) => {
                    let mut last_err = String::from("no modems found");
                    for modem in modems {
                        match discover_modem(&modem).await {
                            Ok(modem_type) => {
                                info!(modem_type = %modem_type, "modem discovered for service");
                                return Ok(Box::new(modem));
                            }
                            Err(e) => {
                                debug!(error = %e, "modem not suitable, trying next");
                                last_err = e.to_string();
                            }
                        }
                    }
                    last_err
                }
                Err(e) => e.to_string(),
            };

            warn!(reason = %retry_reason, "modem discovery failed, retrying");
            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(DISCOVERY_RETRY_SECS)) => {}
                _ = cancel.cancelled() => {
                    return Err(ModemError::Dbus("cancelled during discovery".into()));
                }
            }
        }
    }

    /// Worker loop: processes requests sequentially against the real modem backend.
    async fn worker_loop(
        mut rx: mpsc::Receiver<ModemRequest>,
        modem: Box<dyn ModemAccess>,
        cancel: CancellationToken,
    ) {
        loop {
            let request = tokio::select! {
                req = rx.recv() => {
                    match req {
                        Some(r) => r,
                        None => {
                            debug!("modem request channel closed, worker exiting");
                            return;
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    debug!("modem worker cancelled, exiting");
                    return;
                }
            };

            match request {
                ModemRequest::Model { reply } => {
                    let result = modem.model().await;
                    let _ = reply.send(result);
                }
                ModemRequest::Command {
                    cmd,
                    timeout_ms,
                    reply,
                } => {
                    let result = modem.command(&cmd, timeout_ms).await;
                    let _ = reply.send(result);
                }
                ModemRequest::Imsi { reply } => {
                    let result = modem.imsi().await;
                    let _ = reply.send(result);
                }
                ModemRequest::RegistrationStatus { reply } => {
                    let result = modem.registration_status().await;
                    let _ = reply.send(result);
                }
            }
        }
    }
}

/// Timeout for waiting on a queued modem request reply.
const REQUEST_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(30);

impl ModemAccessService {
    /// Send a request to the worker and await the reply with a timeout.
    async fn request<T>(
        &self,
        request: ModemRequest,
        rx: oneshot::Receiver<Result<T, ModemError>>,
    ) -> Result<T, ModemError> {
        self.tx
            .send(request)
            .await
            .map_err(|_| ModemError::Dbus("modem service shut down".into()))?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ModemError::Dbus("modem worker dropped reply".into())),
            Err(_) => Err(ModemError::Timeout),
        }
    }
}

#[async_trait]
impl ModemAccess for ModemAccessService {
    async fn model(&self) -> Result<String, ModemError> {
        let (tx, rx) = oneshot::channel();
        self.request(ModemRequest::Model { reply: tx }, rx).await
    }

    async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError> {
        let (tx, rx) = oneshot::channel();
        self.request(
            ModemRequest::Command {
                cmd: cmd.to_string(),
                timeout_ms,
                reply: tx,
            },
            rx,
        )
        .await
    }

    async fn imsi(&self) -> Result<String, ModemError> {
        let (tx, rx) = oneshot::channel();
        self.request(ModemRequest::Imsi { reply: tx }, rx).await
    }

    async fn registration_status(&self) -> Result<NetworkRegistration, ModemError> {
        let (tx, rx) = oneshot::channel();
        self.request(ModemRequest::RegistrationStatus { reply: tx }, rx)
            .await
    }
}

/// Parse AT+CREG? or AT+CEREG? response to extract registration status.
/// Expected format: `+CREG: <n>,<stat>` or `+CEREG: <n>,<stat>`
fn parse_registration_response(response: &str) -> Option<NetworkRegistration> {
    // Find the line containing +CREG or +CEREG
    for line in response.lines() {
        let line = line.trim();
        if line.starts_with("+CREG:") || line.starts_with("+CEREG:") {
            // Format: +CREG: <n>,<stat>[,...]
            let parts: Vec<&str> = line.split(':').nth(1)?.trim().split(',').collect();
            if parts.len() >= 2 {
                if let Ok(code) = parts[1].trim().parse::<u8>() {
                    return Some(NetworkRegistration::from_code(code));
                }
            }
        }
    }
    None
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
        /// Connect to ModemManager and return all available modems.
        ///
        /// Returns one `DbusModemAccess` per modem object path found in
        /// ModemManager. Returns `NoModem` error if no modems are present.
        pub async fn discover_all() -> Result<Vec<Self>, ModemError> {
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

            let modem_paths: Vec<String> = objects
                .keys()
                .filter(|path| path.as_str().contains("/Modem/"))
                .map(|p| p.to_string())
                .collect();

            if modem_paths.is_empty() {
                return Err(ModemError::NoModem);
            }

            let modems = modem_paths
                .into_iter()
                .map(|modem_path| {
                    debug!(modem_path = %modem_path, "modem found via D-Bus");
                    Self {
                        connection: connection.clone(),
                        modem_path,
                    }
                })
                .collect();

            Ok(modems)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // --- Mock ModemAccess for testing ---

    struct MockModem {
        model_response: Result<String, ModemError>,
        command_responses: Mutex<Vec<Result<String, ModemError>>>,
        /// Records the order of commands received (for serialization testing).
        command_log: Mutex<Vec<String>>,
    }

    impl MockModem {
        fn with_model(model: &str) -> Self {
            Self {
                model_response: Ok(model.to_string()),
                command_responses: Mutex::new(Vec::new()),
                command_log: Mutex::new(Vec::new()),
            }
        }

        fn with_model_error(err: ModemError) -> Self {
            Self {
                model_response: Err(err),
                command_responses: Mutex::new(Vec::new()),
                command_log: Mutex::new(Vec::new()),
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

        async fn command(&self, cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
            self.command_log.lock().unwrap().push(cmd.to_string());
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
        assert_eq!(
            detect_modem_type("EM060K-GL"),
            Some(ModemType::QuectelEm06GL)
        );
    }

    #[test]
    fn test_detect_model_with_extra_text() {
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

    // --- NetworkRegistration tests ---

    #[test]
    fn test_network_registration_from_code() {
        assert_eq!(
            NetworkRegistration::from_code(0),
            NetworkRegistration::NotRegistered
        );
        assert_eq!(
            NetworkRegistration::from_code(1),
            NetworkRegistration::RegisteredHome
        );
        assert_eq!(
            NetworkRegistration::from_code(2),
            NetworkRegistration::Searching
        );
        assert_eq!(
            NetworkRegistration::from_code(3),
            NetworkRegistration::Denied
        );
        assert_eq!(
            NetworkRegistration::from_code(4),
            NetworkRegistration::Unknown
        );
        assert_eq!(
            NetworkRegistration::from_code(5),
            NetworkRegistration::RegisteredRoaming
        );
        assert_eq!(
            NetworkRegistration::from_code(99),
            NetworkRegistration::Unknown
        );
    }

    // --- parse_registration_response tests ---

    #[test]
    fn test_parse_creg_registered_home() {
        let resp = "+CREG: 0,1";
        assert_eq!(
            parse_registration_response(resp),
            Some(NetworkRegistration::RegisteredHome)
        );
    }

    #[test]
    fn test_parse_cereg_registered_roaming() {
        let resp = "+CEREG: 0,5";
        assert_eq!(
            parse_registration_response(resp),
            Some(NetworkRegistration::RegisteredRoaming)
        );
    }

    #[test]
    fn test_parse_creg_with_extra_fields() {
        // Some modems return extra location fields: +CREG: <n>,<stat>,<lac>,<ci>
        let resp = "+CREG: 2,1,\"1234\",\"5678\"";
        assert_eq!(
            parse_registration_response(resp),
            Some(NetworkRegistration::RegisteredHome)
        );
    }

    #[test]
    fn test_parse_registration_no_match() {
        let resp = "OK";
        assert_eq!(parse_registration_response(resp), None);
    }

    #[test]
    fn test_parse_registration_searching() {
        let resp = "+CEREG: 0,2";
        assert_eq!(
            parse_registration_response(resp),
            Some(NetworkRegistration::Searching)
        );
    }

    // --- ModemAccessService tests ---

    #[tokio::test]
    async fn test_service_model_via_queue() {
        let cancel = CancellationToken::new();
        let mock = MockModem::with_model("EM12");
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        let result = service.model().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "EM12");

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_command_via_queue() {
        let cancel = CancellationToken::new();
        let mock =
            MockModem::with_model("EM12").with_command_response(Ok("+CSQ: 20,99".to_string()));
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        let result = service.command("AT+CSQ", 3000).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "+CSQ: 20,99");

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_imsi_via_queue() {
        let cancel = CancellationToken::new();
        let mock = MockModem::with_model("EM12")
            .with_command_response(Ok("310260123456789\r\n".to_string()));
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        let result = service.imsi().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "310260123456789");

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_registration_status_via_queue() {
        let cancel = CancellationToken::new();
        let mock =
            MockModem::with_model("EM12").with_command_response(Ok("+CEREG: 0,1".to_string()));
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        let result = service.registration_status().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), NetworkRegistration::RegisteredHome);

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_registration_falls_back_to_creg() {
        let cancel = CancellationToken::new();
        // First response (AT+CEREG?) fails, second (AT+CREG?) succeeds
        let mock = MockModem::with_model("EM12")
            .with_command_response(Err(ModemError::Dbus("not supported".into())))
            .with_command_response(Ok("+CREG: 0,5".to_string()));
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        let result = service.registration_status().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), NetworkRegistration::RegisteredRoaming);

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_concurrent_commands_serialized() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        let cancel = CancellationToken::new();
        // Prepare enough responses for 3 concurrent commands
        let mock = MockModem::with_model("EM12")
            .with_command_response(Ok("resp1".to_string()))
            .with_command_response(Ok("resp2".to_string()))
            .with_command_response(Ok("resp3".to_string()));

        let log = StdArc::new(Mutex::new(Vec::<String>::new()));
        let log_clone = log.clone();
        // Track max concurrency: if commands overlap, this will exceed 1
        let active = StdArc::new(AtomicUsize::new(0));
        let max_active = StdArc::new(AtomicUsize::new(0));
        let active_clone = active.clone();
        let max_active_clone = max_active.clone();

        // Use a wrapper mock that records order with delays to verify serialization
        struct OrderedMock {
            inner: MockModem,
            log: StdArc<Mutex<Vec<String>>>,
            active: StdArc<AtomicUsize>,
            max_active: StdArc<AtomicUsize>,
        }

        #[async_trait]
        impl ModemAccess for OrderedMock {
            async fn model(&self) -> Result<String, ModemError> {
                self.inner.model().await
            }
            async fn command(&self, cmd: &str, timeout_ms: u32) -> Result<String, ModemError> {
                let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(current, Ordering::SeqCst);
                self.log.lock().unwrap().push(cmd.to_string());
                // Delay to make concurrency observable if serialization is broken
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                self.inner.command(cmd, timeout_ms).await
            }
        }

        let ordered = OrderedMock {
            inner: mock,
            log: log_clone,
            active: active_clone,
            max_active: max_active_clone,
        };
        let service = ModemAccessService::with_backend(Box::new(ordered), &cancel);

        // Fire 3 commands concurrently
        let s1 = service.clone();
        let s2 = service.clone();
        let s3 = service.clone();

        let (r1, r2, r3) = tokio::join!(
            s1.command("CMD1", 3000),
            s2.command("CMD2", 3000),
            s3.command("CMD3", 3000),
        );

        // All should succeed
        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(r3.is_ok());

        // The log should have exactly 3 entries
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 3);

        // Verify serialization: max concurrency must never exceed 1
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "commands should be serialized (max 1 active at a time)"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_handles_caller_drop() {
        let cancel = CancellationToken::new();
        // Pre-load two command responses: one for the dropped request, one for verification
        let mock = MockModem::with_model("EM12")
            .with_command_response(Ok("OK".to_string()))
            .with_command_response(Ok("still alive".to_string()));
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        // Send a request but drop the receiver before the worker replies
        let (tx, rx) = oneshot::channel::<Result<String, ModemError>>();
        drop(rx); // Drop receiver immediately

        // The worker should handle the dropped oneshot gracefully (send returns Err but doesn't panic)
        let _ = service
            .tx
            .send(ModemRequest::Command {
                cmd: "AT".to_string(),
                timeout_ms: 3000,
                reply: tx,
            })
            .await;

        // Give the worker a moment to process the dropped request
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Verify the worker is still alive by sending a follow-up command
        let result = service.command("AT+CHECK", 3000).await;
        assert!(
            result.is_ok(),
            "worker should still be alive after handling dropped oneshot"
        );
        assert_eq!(result.unwrap(), "still alive");

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_service_worker_exits_on_cancel() {
        let cancel = CancellationToken::new();
        let mock = MockModem::with_model("EM12");
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        cancel.cancel();

        // Give the worker a moment to exit
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // After cancellation, sending a request should fail (channel closed)
        // or the reply will be dropped
        let result = service.model().await;
        // Worker has exited, so either the send fails or the reply channel is dropped
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_service_worker_exits_on_sender_drop() {
        let cancel = CancellationToken::new();
        let mock = MockModem::with_model("EM12");
        let service = ModemAccessService::with_backend(Box::new(mock), &cancel);

        // Drop all senders by dropping the service
        drop(service);

        // Give the worker a moment to notice the channel closure
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // No assertion needed — the test passes if the worker task doesn't hang
        cancel.cancel();
    }
}
