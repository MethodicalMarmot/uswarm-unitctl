use std::sync::Arc;

use async_trait::async_trait;
use tracing::info;

use crate::context::Context;
use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, GetConfigResult, SafeConfig,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};

/// Handler for `get_config` commands.
///
/// Reads the current configuration from Context and returns it as JSON.
/// Uses `SafeConfig` to redact sensitive fields (TLS cert paths).
pub struct GetConfigHandler {
    ctx: Arc<Context>,
}

impl GetConfigHandler {
    pub const NAME: &str = "get_config";
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl CommandHandler for GetConfigHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        match &envelope.payload {
            CommandPayload::GetConfig(_) => {}
            _ => {
                return Err(CommandError::new("expected GetConfig payload"));
            }
        }

        info!(uuid = %envelope.uuid, "Get config requested");

        let result = GetConfigResult {
            config: SafeConfig::from(&self.ctx.config),
        };

        Ok(CommandResult {
            data: CommandResultData::GetConfig(Box::new(result)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use crate::messages::commands::{CommandEnvelope, CommandPayload, GetConfigPayload};
    use chrono::Utc;

    fn make_envelope() -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-get-config".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::GetConfig(GetConfigPayload {}),
        }
    }

    fn extract_config(result: &CommandResult) -> &GetConfigResult {
        match &result.data {
            CommandResultData::GetConfig(ref r) => r,
            _ => panic!("expected GetConfig result"),
        }
    }

    #[tokio::test]
    async fn test_get_config_handler_returns_config() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let config = &extract_config(&result).config;
        // Verify key config sections are present
        assert!(!config.general.debug); // test_config has debug=false
        assert!(!config.mavlink.host.is_empty());
        assert!(!config.mqtt.host.is_empty());
    }

    #[tokio::test]
    async fn test_get_config_handler_config_values() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let config = &extract_config(&result).config;

        assert!(!config.general.debug);
        assert_eq!(config.mavlink.host, "127.0.0.1");
        assert!(!config.mqtt.enabled);
    }

    #[tokio::test]
    async fn test_get_config_handler_redacts_cert_paths() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let general = &extract_config(&result).config.general;
        assert_eq!(general.ca_cert_path.as_deref(), Some("***"));
        assert_eq!(general.client_cert_path.as_deref(), Some("***"));
        assert_eq!(general.client_key_path.as_deref(), Some("***"));
    }

    #[tokio::test]
    async fn test_get_config_handler_uses_safe_config() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let config = &extract_config(&result).config;
        assert_eq!(config.general.ca_cert_path.as_deref(), Some("***"));
        assert_eq!(config.general.client_cert_path.as_deref(), Some("***"));
        assert_eq!(config.general.client_key_path.as_deref(), Some("***"));
    }

    #[tokio::test]
    async fn test_get_config_handler_wrong_payload_variant() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let envelope = CommandEnvelope {
            uuid: "test-uuid-get-config".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::ModemCommands(
                crate::messages::commands::ModemCommandPayload {
                    command: "ATI".to_string(),
                    timeout_ms: None,
                },
            ),
        };

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("expected GetConfig payload"));
    }
}
