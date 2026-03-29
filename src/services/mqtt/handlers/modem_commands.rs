use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::context::Context;
use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, ModemCommandResult,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};

/// Default AT command timeout in milliseconds.
const DEFAULT_AT_TIMEOUT_MS: u32 = 5000;

/// Maximum allowed AT command timeout in milliseconds (30 seconds).
const MAX_AT_TIMEOUT_MS: u32 = 30_000;

/// AT command prefixes that are inherently read-only (any suffix is safe).
const SAFE_AT_PREFIXES: &[&str] = &[
    "AT+CSQ",  // Signal quality
    "AT+CIMI", // IMSI query
    "AT+CGSN", // IMEI query
    "AT+CGMM", // Model identification
    "AT+CGMR", // Firmware revision
    "ATI",     // Device info
];

/// AT command prefixes that have write forms — only query forms (?, =?) are allowed.
/// For example, AT+COPS? is safe but AT+COPS=1,2 would change the operator.
const QUERY_ONLY_AT_PREFIXES: &[&str] = &[
    "AT+COPS",    // Operator selection (write form sets operator)
    "AT+CEREG",   // EPS network registration (write form sets URC mode)
    "AT+CREG",    // Network registration (write form sets URC mode)
    "AT+CGREG",   // GPRS registration (write form sets URC mode)
    "AT+CGDCONT", // PDP context (write form modifies APN config)
    "AT+QENG",    // Engineering mode (write form enables/disables reporting)
    "AT+QNWINFO", // Network info (write form exists on some modems)
    "AT+QSPN",    // Service provider name (write form exists on some modems)
];

/// Check if an AT command is in the allowlist.
fn is_allowed_at_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();

    // Reject non-ASCII input to prevent Unicode look-alike bypasses
    // (e.g., fullwidth characters like ＡＴ＋ＣＳＱ).
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii() && !b.is_ascii_control())
    {
        return false;
    }

    let upper = trimmed.to_uppercase();

    // Reject semicolons to prevent AT command chaining
    // (e.g., AT+CSQ;AT+CFUN=0).
    if upper.contains(';') {
        return false;
    }

    // Inherently safe commands — prefix match is sufficient
    if SAFE_AT_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        return true;
    }

    // Commands with write forms — only allow query form (no args, ?, or =?)
    for prefix in QUERY_ONLY_AT_PREFIXES {
        if let Some(suffix) = upper.strip_prefix(prefix) {
            return suffix.is_empty() || suffix == "?" || suffix == "=?";
        }
    }

    false
}

/// Handler for `modem_commands` — routes AT commands through ModemAccess.
///
/// Expects payload:
/// ```json
/// {
///     "command": "AT+CSQ",
///     "timeout_ms": 5000  // optional, defaults to 5000
/// }
/// ```
pub struct ModemCommandsHandler {
    ctx: Arc<Context>,
}

impl ModemCommandsHandler {
    pub const NAME: &str = "modem_commands";
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl CommandHandler for ModemCommandsHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        let typed_payload = match &envelope.payload {
            CommandPayload::ModemCommands(p) => p,
            _ => {
                return Err(CommandError::new("expected ModemCommands payload"));
            }
        };

        let at_command = &typed_payload.command;

        if !is_allowed_at_command(at_command) {
            warn!(
                uuid = %envelope.uuid,
                command = %at_command,
                "AT command rejected: not in allowlist"
            );
            return Err(CommandError::new(format!(
                "AT command '{}' is not allowed",
                at_command
            )));
        }

        let timeout_ms = typed_payload
            .timeout_ms
            .unwrap_or(DEFAULT_AT_TIMEOUT_MS)
            .min(MAX_AT_TIMEOUT_MS);

        info!(
            uuid = %envelope.uuid,
            command = %at_command,
            timeout_ms = timeout_ms,
            "Executing modem AT command"
        );

        let modem = self
            .ctx
            .get_modem()
            .await
            .ok_or_else(|| CommandError::new("modem not available"))?;

        match modem.command(at_command, timeout_ms).await {
            Ok(response) => {
                info!(
                    uuid = %envelope.uuid,
                    command = %at_command,
                    "AT command succeeded"
                );
                let result = ModemCommandResult {
                    command: at_command.clone(),
                    response,
                };
                Ok(CommandResult {
                    data: CommandResultData::ModemCommands(result),
                })
            }
            Err(e) => {
                warn!(
                    uuid = %envelope.uuid,
                    command = %at_command,
                    error = %e,
                    "AT command failed"
                );
                Err(CommandError::new(format!(
                    "AT command '{}' failed: {}",
                    at_command, e
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::messages::commands::ModemCommandPayload;
    use crate::services::modem_access::{ModemAccess, ModemError};
    use chrono::Utc;

    struct MockModem {
        response: Result<String, ModemError>,
    }

    #[async_trait]
    impl ModemAccess for MockModem {
        async fn model(&self) -> Result<String, ModemError> {
            Ok("MOCK_MODEM".to_string())
        }

        async fn command(&self, _cmd: &str, _timeout_ms: u32) -> Result<String, ModemError> {
            self.response.clone()
        }
    }

    fn make_envelope(command: &str, timeout_ms: Option<u32>) -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-modem".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::ModemCommands(ModemCommandPayload {
                command: command.to_string(),
                timeout_ms,
            }),
        }
    }

    fn extract_result(result: &CommandResult) -> &ModemCommandResult {
        match &result.data {
            CommandResultData::ModemCommands(r) => r,
            _ => panic!("expected ModemCommands result"),
        }
    }

    #[tokio::test]
    async fn test_modem_command_success() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Ok("+CSQ: 15,99".to_string()),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("AT+CSQ", None);

        let result = handler.handle(&envelope).await.unwrap();
        let typed = extract_result(&result);
        assert_eq!(typed.command, "AT+CSQ");
        assert_eq!(typed.response, "+CSQ: 15,99");
    }

    #[tokio::test]
    async fn test_modem_command_with_custom_timeout() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Ok("OK".to_string()),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("ATI", Some(10000));

        let result = handler.handle(&envelope).await.unwrap();
        extract_result(&result); // just verify it's the right variant
    }

    #[tokio::test]
    async fn test_modem_command_wrong_payload_variant() {
        let ctx = Context::new(test_config());
        let handler = ModemCommandsHandler::new(ctx);

        let envelope = CommandEnvelope {
            uuid: "test-uuid-modem".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::GetConfig(crate::messages::commands::GetConfigPayload {}),
        };

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("expected ModemCommands payload"));
    }

    #[tokio::test]
    async fn test_modem_command_no_modem_available() {
        let ctx = Context::new(test_config());
        // Don't set modem — it stays None

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("AT+CSQ", None);

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("modem not available"));
    }

    #[tokio::test]
    async fn test_modem_command_at_error() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Err(ModemError::Timeout),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("AT+CSQ", None);

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("AT command"));
        assert!(err.message.contains("failed"));
    }

    #[test]
    fn test_allowed_at_commands() {
        // Safe prefixes — any suffix OK
        assert!(is_allowed_at_command("AT+CSQ"));
        assert!(is_allowed_at_command("ATI"));
        assert!(is_allowed_at_command("AT+CIMI"));
        assert!(is_allowed_at_command("at+csq")); // case insensitive

        // Query-only commands — only ?, =?, or bare form allowed
        assert!(is_allowed_at_command("AT+COPS?"));
        assert!(is_allowed_at_command("AT+COPS=?"));
        assert!(is_allowed_at_command("AT+COPS"));
        assert!(is_allowed_at_command("AT+CEREG?"));
        assert!(is_allowed_at_command("AT+CGDCONT?"));
        assert!(is_allowed_at_command("AT+CGDCONT=?"));
        assert!(is_allowed_at_command("AT+QENG?"));
        assert!(is_allowed_at_command("AT+QENG=?"));
        assert!(is_allowed_at_command("AT+QNWINFO?"));
        assert!(is_allowed_at_command("AT+QSPN?"));
    }

    #[test]
    fn test_blocked_at_commands() {
        assert!(!is_allowed_at_command("AT+CFUN=0")); // disable radio
        assert!(!is_allowed_at_command("AT+QPOWD=0")); // power off modem
        assert!(!is_allowed_at_command("ATD12345")); // dial
        assert!(!is_allowed_at_command("AT&F")); // factory reset
        assert!(!is_allowed_at_command("AT+CLCK")); // facility lock
        assert!(!is_allowed_at_command("")); // empty

        // Write forms of query-only commands must be blocked
        assert!(!is_allowed_at_command("AT+COPS=1,2,\"name\"")); // set operator
        assert!(!is_allowed_at_command("AT+CGDCONT=1,\"IP\",\"apn\"")); // modify PDP
        assert!(!is_allowed_at_command("AT+CEREG=2")); // set URC mode
        assert!(!is_allowed_at_command("AT+CREG=1")); // set URC mode
        assert!(!is_allowed_at_command("AT+CGREG=2")); // set URC mode
        assert!(!is_allowed_at_command("AT+QENG=\"servingcell\"")); // write form
        assert!(!is_allowed_at_command("AT+QENG=0")); // enable/disable reporting

        // Semicolon command chaining must be blocked
        assert!(!is_allowed_at_command("AT+CSQ;AT+CFUN=0"));
        assert!(!is_allowed_at_command("AT+CIMI;ATD12345"));
    }

    #[tokio::test]
    async fn test_modem_command_blocked_command() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Ok("OK".to_string()),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("AT+CFUN=0", None);

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("not allowed"));
    }

    #[tokio::test]
    async fn test_modem_command_result_is_typed() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Ok("+CSQ: 15,99".to_string()),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("AT+CSQ", None);

        let result = handler.handle(&envelope).await.unwrap();
        let typed = extract_result(&result);
        assert_eq!(typed.command, "AT+CSQ");
        assert_eq!(typed.response, "+CSQ: 15,99");
    }

    #[tokio::test]
    async fn test_modem_command_dbus_error() {
        let ctx = Context::new(test_config());
        let modem = Arc::new(MockModem {
            response: Err(ModemError::Dbus("connection refused".to_string())),
        });
        ctx.set_modem(modem).await;

        let handler = ModemCommandsHandler::new(ctx);
        let envelope = make_envelope("ATI", None);

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("failed"));
    }
}
