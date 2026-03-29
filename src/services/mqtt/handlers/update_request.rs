use async_trait::async_trait;
use tracing::info;

use crate::messages::commands::{
    CommandEnvelope, CommandPayload, CommandResultData, UpdateRequestResult,
};
use crate::services::mqtt::commands::{CommandError, CommandHandler, CommandResult};

/// Handler for `update_request` commands.
///
/// Receives an update request payload, logs it, and acknowledges receipt.
/// This is a placeholder implementation — the real update mechanism (e.g.,
/// downloading and applying a firmware/software update) is not yet implemented.
#[derive(Debug, Clone, Default)]
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
        let typed_payload = match &envelope.payload {
            CommandPayload::UpdateRequest(p) => p,
            _ => {
                return Err(CommandError::new("expected UpdateRequest payload"));
            }
        };

        info!(
            uuid = %envelope.uuid,
            version = %typed_payload.version,
            url = %typed_payload.url,
            "Update request received (placeholder)"
        );

        let result = UpdateRequestResult {
            message: "update request acknowledged (placeholder)".to_string(),
            version: typed_payload.version.clone(),
        };

        Ok(CommandResult {
            data: CommandResultData::UpdateRequest(result),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::commands::UpdateRequestPayload;
    use chrono::Utc;

    fn make_envelope(version: &str, url: &str) -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::UpdateRequest(UpdateRequestPayload {
                version: version.to_string(),
                url: url.to_string(),
            }),
        }
    }

    fn extract_result(result: &CommandResult) -> &UpdateRequestResult {
        match &result.data {
            CommandResultData::UpdateRequest(r) => r,
            _ => panic!("expected UpdateRequest result"),
        }
    }

    #[tokio::test]
    async fn test_update_request_handler_with_version() {
        let handler = UpdateRequestHandler::new();
        let envelope = make_envelope("2.1.0", "https://updates.example.com/v2.1.0");

        let result = handler.handle(&envelope).await.unwrap();
        assert_eq!(extract_result(&result).version, "2.1.0");
    }

    #[tokio::test]
    async fn test_update_request_handler_wrong_payload_variant() {
        let handler = UpdateRequestHandler::new();

        let envelope = CommandEnvelope {
            uuid: "test-uuid-update".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: CommandPayload::GetConfig(crate::messages::commands::GetConfigPayload {}),
        };

        let err = handler.handle(&envelope).await.unwrap_err();
        assert!(err.message.contains("expected UpdateRequest payload"));
    }

    #[tokio::test]
    async fn test_update_request_handler_result_is_typed() {
        let handler = UpdateRequestHandler::new();
        let envelope = make_envelope("3.0.0", "https://updates.example.com/v3.0.0");

        let result = handler.handle(&envelope).await.unwrap();
        let typed = extract_result(&result);
        assert_eq!(typed.version, "3.0.0");
        assert!(typed.message.contains("placeholder"));
    }
}
