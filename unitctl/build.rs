use std::fs;
use std::path::Path;

fn main() {
    // Ensure the schema output directory exists.
    // Actual schema generation happens via `messages::schema::generate_all_schemas()`
    // which is called from tests and can be invoked programmatically.
    // build.rs cannot use crate types directly (it runs before compilation),
    // so we only set up the directory structure here.
    let schema_dir = Path::new("assets/schema");
    let dirs = [
        schema_dir.join("telemetry"),
        schema_dir.join("command"),
    ];

    for dir in &dirs {
        fs::create_dir_all(dir).expect("failed to create schema directory");
    }

    // Rebuild when message definitions change
    println!("cargo:rerun-if-changed=src/messages/telemetry.rs");
    println!("cargo:rerun-if-changed=src/messages/commands.rs");
}
