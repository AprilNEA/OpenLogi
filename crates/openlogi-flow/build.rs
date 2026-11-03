//! Generates the `openlogi.flow.v1` wire types from `proto/flow.v1.proto`.
//!
//! `protox` parses the schema in-process, so building the workspace needs no
//! system `protoc`. The descriptor set it writes carries imports and source
//! info, exactly what `buffa-build` asks `protoc` for, so the generated Rust
//! (including the doc comments taken from the schema) is the same.

use std::path::{Path, PathBuf};

const PROTO_ROOT: &str = "proto";
const SCHEMA: &str = "flow.v1.proto";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={PROTO_ROOT}");

    let mut compiler = protox::Compiler::new([PROTO_ROOT])?;
    compiler
        .include_imports(true)
        .include_source_info(true)
        .open_file(SCHEMA)?;

    let descriptor_set = PathBuf::from(std::env::var("OUT_DIR")?).join("flow.v1.binpb");
    write_if_changed(&descriptor_set, &compiler.encode_file_descriptor_set())?;

    buffa_build::Config::new()
        .descriptor_set(&descriptor_set)
        .files(&[SCHEMA])
        .include_file("_include.rs")
        .compile()?;
    Ok(())
}

/// Leaves an unchanged descriptor set untouched: `buffa-build` registers it
/// with `rerun-if-changed`, and rewriting it on every run would make the
/// script rerun on every build.
fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes)
}
