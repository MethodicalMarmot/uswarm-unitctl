use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let schema_dir = Path::new(&manifest_dir).join("assets/schema");

    // Ensure the schema output directories exist (needed on first build
    // before the generate-schema binary has been compiled).
    let dirs = [schema_dir.join("telemetry"), schema_dir.join("command")];
    for dir in &dirs {
        fs::create_dir_all(dir).expect("failed to create schema directory");
    }

    // Try to run the previously-built generate-schema binary.
    // On the first build the binary won't exist yet; schemas will be
    // generated on the next build or by running:
    //   cargo run --bin generate-schema
    if let Some(bin) = find_generate_schema_binary() {
        let status = Command::new(&bin).arg(&schema_dir).status();
        match status {
            Ok(s) if s.success() => {
                eprintln!("build.rs: schemas generated via {}", bin.display());
            }
            Ok(s) => {
                eprintln!(
                    "build.rs: generate-schema exited with {}, schemas may be stale",
                    s
                );
            }
            Err(e) => {
                eprintln!(
                    "build.rs: failed to run generate-schema ({}), schemas may be stale",
                    e
                );
            }
        }
    } else {
        eprintln!(
            "build.rs: generate-schema binary not found (first build?), \
             run `cargo run --bin generate-schema` to generate schemas"
        );
    }

    // Rebuild when message definitions change
    println!("cargo:rerun-if-changed=src/messages/telemetry.rs");
    println!("cargo:rerun-if-changed=src/messages/commands.rs");
    println!("cargo:rerun-if-changed=src/bin/generate_schema.rs");
}

/// Locate the generate-schema binary from a previous build.
///
/// Derives the target profile directory from OUT_DIR, which has the form:
///   <target_dir>/<profile>/build/<pkg>-<hash>/out
fn find_generate_schema_binary() -> Option<PathBuf> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").ok()?);
    // Walk up: out -> <hash> -> build -> <profile>
    let profile_dir = out_dir.parent()?.parent()?.parent()?;
    let bin = profile_dir.join("generate-schema");
    if bin.exists() {
        Some(bin)
    } else {
        None
    }
}
