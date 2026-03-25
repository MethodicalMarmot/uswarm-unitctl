use async_trait::async_trait;
use tracing::info;

use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, ConfigUpdateResult,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};

/// Handler for `config_update` commands.
///
/// Receives a config payload and applies changes. Currently a placeholder
/// implementation that acknowledges receipt and logs the payload.
pub struct ConfigUpdateHandler;

impl ConfigUpdateHandler {
    pub const NAME: &str = "config_update";

    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CommandHandler for ConfigUpdateHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        info!(
            uuid = %envelope.uuid,
            "Config update requested (placeholder)"
        );

        let typed_payload = match &envelope.payload {
            CommandPayload::ConfigUpdate(p) => p,
            _ => {
                return Err(CommandError::new("expected ConfigUpdate payload"));
            }
        };

        // Placeholder: log what fields were requested to change.
        // Real implementation would validate and apply config changes.
        let fields: Vec<String> = typed_payload
            .payload
            .as_object()
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();

        info!(
            uuid = %envelope.uuid,
            fields = ?fields,
            "Config update fields received"
        );

        let result = ConfigUpdateResult {
            message: "config update acknowledged (placeholder)".to_string(),
            fields_received: fields,
        };

        Ok(CommandResult {
            data: CommandResultData::ConfigUpdate(result),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::commands::ConfigUpdatePayload;
    use chrono::Utc;

    fn make_envelope(payload: serde_json::Value) -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-config-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::ConfigUpdate(ConfigUpdatePayload { payload }),
        }
    }

    fn extract_result(result: &CommandResult) -> &ConfigUpdateResult {
        match &result.data {
            CommandResultData::ConfigUpdate(r) => r,
            _ => panic!("expected ConfigUpdate result"),
        }
    }

    #[tokio::test]
    async fn test_config_update_handler_success() {
        let handler = ConfigUpdateHandler::new();

        let envelope = make_envelope(serde_json::json!({
            "sensors": {
                "default_interval_s": 2.0
            }
        }));

        let result = handler.handle(&envelope).await.unwrap();
        assert_eq!(extract_result(&result).fields_received, vec!["sensors"]);
    }

    #[tokio::test]
    async fn test_config_update_handler_empty_payload() {
        let handler = ConfigUpdateHandler::new();

        let envelope = make_envelope(serde_json::json!({}));

        let result = handler.handle(&envelope).await.unwrap();
        assert!(extract_result(&result).fields_received.is_empty());
    }

    #[tokio::test]
    async fn test_config_update_handler_wrong_payload_variant() {
        let handler = ConfigUpdateHandler::new();

        let envelope = CommandEnvelope {
            uuid: "test-uuid-config-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::GetConfig(crate::messages::commands::GetConfigPayload {}),
        };

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("expected ConfigUpdate payload"));
    }

    #[tokio::test]
    async fn test_config_update_handler_result_is_typed() {
        let handler = ConfigUpdateHandler::new();

        let envelope = make_envelope(serde_json::json!({
            "sensors": { "ping": { "enabled": false } },
            "mqtt": { "telemetry_interval_s": 5.0 }
        }));

        let result = handler.handle(&envelope).await.unwrap();
        let typed = extract_result(&result);
        assert_eq!(typed.fields_received.len(), 2);
        assert!(typed.message.contains("placeholder"));
    }
}
