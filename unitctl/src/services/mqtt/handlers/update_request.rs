use async_trait::async_trait;
use tracing::info;

use crate::services::mqtt::commands::{
    CommandEnvelope, CommandError, CommandHandler, CommandResult,
};

/// Handler for `update_request` commands.
///
/// Receives an update request payload, logs it, and acknowledges receipt.
/// This is a placeholder implementation — the real update mechanism (e.g.,
/// downloading and applying a firmware/software update) is not yet implemented.
pub struct UpdateRequestHandler;

impl UpdateRequestHandler {
    pub const NAME: &str = "update_request";

    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CommandHandler for UpdateRequestHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        let version = envelope
            .payload
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let url = envelope
            .payload
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("not provided");

        info!(
            uuid = %envelope.uuid,
            version = %version,
            url = %url,
            "Update request received (placeholder)"
        );

        Ok(CommandResult {
            extra: serde_json::json!({
                "message": "update request acknowledged (placeholder)",
                "version": version,
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
            uuid: "test-uuid-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload,
        }
    }

    #[tokio::test]
    async fn test_update_request_handler_with_version() {
        let handler = UpdateRequestHandler::new();
        let envelope = make_envelope(serde_json::json!({
            "version": "2.1.0",
            "url": "https://updates.example.com/v2.1.0"
        }));

        let result = handler.handle(&envelope).await.unwrap();

        assert_eq!(result.extra["version"], "2.1.0");
    }

    #[tokio::test]
    async fn test_update_request_handler_empty_payload() {
        let handler = UpdateRequestHandler::new();
        let envelope = make_envelope(serde_json::json!({}));

        let result = handler.handle(&envelope).await.unwrap();

        assert_eq!(result.extra["version"], "unknown");
    }

    #[tokio::test]
    async fn test_update_request_handler_missing_url() {
        let handler = UpdateRequestHandler::new();
        let envelope = make_envelope(serde_json::json!({
            "version": "1.0.0"
        }));

        let result = handler.handle(&envelope).await.unwrap();

        assert_eq!(result.extra["version"], "1.0.0");
    }
}
