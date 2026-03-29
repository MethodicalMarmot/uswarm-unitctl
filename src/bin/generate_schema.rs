use schemars::schema_for;
use std::fs;
use std::path::{Path, PathBuf};
use unitctl::messages::commands::{CommandEnvelope, CommandResultMsg, CommandStatus};
use unitctl::messages::status::NodeStatusEnvelope;
use unitctl::messages::telemetry::TelemetryEnvelope;

fn main() {
    let base_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/schema"));

    generate_all_schemas(&base_dir).expect("schema generation failed");
    println!("schemas written to {}", base_dir.display());
}

/// Generate all JSON Schema files and write them to `base_dir`.
///
/// Directory layout:
/// ```text
/// telemetry/envelope.json
/// command/envelope.json
/// command/status.json
/// command/result.json
/// status/envelope.json
/// ```
fn generate_all_schemas(base_dir: &Path) -> std::io::Result<()> {
    // Telemetry schemas
    let telemetry_dir = base_dir.join("telemetry");
    fs::create_dir_all(&telemetry_dir)?;

    write_schema::<TelemetryEnvelope>(&telemetry_dir.join("envelope.json"))?;

    // Shared command schemas
    let command_dir = base_dir.join("command");
    fs::create_dir_all(&command_dir)?;

    write_schema::<CommandEnvelope>(&command_dir.join("envelope.json"))?;
    write_schema::<CommandStatus>(&command_dir.join("status.json"))?;
    write_schema::<CommandResultMsg>(&command_dir.join("result.json"))?;

    // Status schemas
    let status_dir = base_dir.join("status");
    fs::create_dir_all(&status_dir)?;

    write_schema::<NodeStatusEnvelope>(&status_dir.join("envelope.json"))?;

    Ok(())
}

fn write_schema<T: schemars::JsonSchema>(path: &Path) -> std::io::Result<()> {
    let schema = schema_for!(T);
    let json = serde_json::to_string_pretty(&schema).map_err(std::io::Error::other)?;
    fs::write(path, json + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn schema_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/schema")
    }

    #[test]
    fn generate_and_write_schemas() {
        let dir = schema_dir();
        generate_all_schemas(&dir).expect("schema generation failed");

        // Verify telemetry schema exists
        assert!(dir.join("telemetry/envelope.json").exists());

        // Verify command schemas exist
        assert!(dir.join("command/envelope.json").exists());
        assert!(dir.join("command/status.json").exists());
        assert!(dir.join("command/result.json").exists());

        // Verify status schema exists
        assert!(dir.join("status/envelope.json").exists());
    }

    #[test]
    fn schemas_are_valid_json_schema() {
        let dir = schema_dir();
        generate_all_schemas(&dir).expect("schema generation failed");

        let schema_files = [
            "telemetry/envelope.json",
            "command/envelope.json",
            "command/status.json",
            "command/result.json",
            "status/envelope.json",
        ];

        for file in &schema_files {
            let path = dir.join(file);
            let content =
                fs::read_to_string(&path).unwrap_or_else(|_| panic!("failed to read {}", file));
            let value: serde_json::Value = serde_json::from_str(&content)
                .unwrap_or_else(|_| panic!("{} is not valid JSON", file));

            // Verify it looks like a JSON Schema (has $schema or type field)
            let obj = value.as_object().expect("schema should be an object");
            assert!(
                obj.contains_key("$schema")
                    || obj.contains_key("type")
                    || obj.contains_key("definitions"),
                "{} doesn't look like a JSON Schema",
                file
            );
        }
    }
}
