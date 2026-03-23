use std::sync::Arc;

use async_trait::async_trait;
use tracing::info;

use crate::context::Context;
use crate::services::mqtt::commands::{
    CommandEnvelope, CommandError, CommandHandler, CommandResult,
};

/// Handler for `get_config` commands.
///
/// Reads the current configuration from Context and returns it as JSON.
pub struct GetConfigHandler {
    ctx: Arc<Context>,
}

impl GetConfigHandler {
    pub const NAME: &str = "get_config";
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }
}

/// Keys to redact from MQTT config before publishing (contain filesystem paths to secrets).
const REDACTED_MQTT_KEYS: &[&str] = &["ca_cert_path", "client_cert_path", "client_key_path"];

#[async_trait]
impl CommandHandler for GetConfigHandler {
    async fn handle(&self, envelope: &CommandEnvelope) -> Result<CommandResult, CommandError> {
        info!(uuid = %envelope.uuid, "Get config requested");

        let mut config_json = serde_json::to_value(&self.ctx.config)
            .map_err(|e| CommandError::new(format!("failed to serialize config: {e}")))?;

        // Redact sensitive certificate paths from the mqtt section
        if let Some(mqtt) = config_json.get_mut("mqtt").and_then(|v| v.as_object_mut()) {
            for key in REDACTED_MQTT_KEYS {
                if mqtt.contains_key(*key) {
                    mqtt.insert(
                        (*key).to_string(),
                        serde_json::Value::String("***".to_string()),
                    );
                }
            }
        }

        Ok(CommandResult {
            extra: serde_json::json!({
                "config": config_json,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::test_config;
    use chrono::Utc;

    fn make_envelope() -> CommandEnvelope {
        CommandEnvelope {
            uuid: "test-uuid-get-config".to_string(),
            issued_at: Utc::now(),
            ttl_sec: 300,
            payload: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn test_get_config_handler_returns_config() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let config = &result.extra["config"];
        assert!(config.is_object());
        // Verify key config sections are present
        assert!(config["general"].is_object());
        assert!(config["mavlink"].is_object());
        assert!(config["sensors"].is_object());
        assert!(config["camera"].is_object());
        assert!(config["mqtt"].is_object());
    }

    #[tokio::test]
    async fn test_get_config_handler_config_values() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let config = &result.extra["config"];

        // Verify some specific values from test_config
        assert_eq!(config["general"]["debug"], false);
        assert_eq!(config["mavlink"]["host"], "127.0.0.1");
        assert_eq!(config["mqtt"]["enabled"], false);
    }

    #[tokio::test]
    async fn test_get_config_handler_redacts_cert_paths() {
        let ctx = Context::new(test_config());
        let handler = GetConfigHandler::new(ctx);

        let result = handler.handle(&make_envelope()).await.unwrap();
        let mqtt = &result.extra["config"]["mqtt"];
        assert_eq!(mqtt["ca_cert_path"], "***");
        assert_eq!(mqtt["client_cert_path"], "***");
        assert_eq!(mqtt["client_key_path"], "***");
    }
}
