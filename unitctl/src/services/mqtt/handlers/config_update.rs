use async_trait::async_trait;
use tracing::info;

use crate::services::mqtt::commands::{
    CommandEnvelope, CommandError, CommandHandler, CommandResult,
};

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

        // Placeholder: log what fields were requested to change.
        // Real implementation would validate and apply config changes.
        let fields: Vec<String> = envelope
            .payload
            .as_object()
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();

        info!(
            uuid = %envelope.uuid,
            fields = ?fields,
            "Config update fields received"
        );

        Ok(CommandResult {
            extra: serde_json::json!({
                "message": "config update acknowledged (placeholder)",
                "fields_received": fields,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_envelope(payload: serde_json::Value) -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-config-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload,
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
        assert_eq!(
            result.extra["fields_received"],
            serde_json::json!(["sensors"])
        );
    }

    #[tokio::test]
    async fn test_config_update_handler_empty_payload() {
        let handler = ConfigUpdateHandler::new();

        let envelope = make_envelope(serde_json::json!({}));

        let result = handler.handle(&envelope).await.unwrap();
        let fields = result.extra["fields_received"].as_array().unwrap();
        assert!(fields.is_empty());
    }

    #[tokio::test]
    async fn test_config_update_handler_non_object_payload() {
        let handler = ConfigUpdateHandler::new();

        let envelope = make_envelope(serde_json::json!("not an object"));

        let result = handler.handle(&envelope).await.unwrap();
        let fields = result.extra["fields_received"].as_array().unwrap();
        assert!(fields.is_empty());
    }
}
