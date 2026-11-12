//! Regression checks for the activity metadata generator used by the Rust build.

#[path = "../build/activity.rs"]
mod activity;

use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{self, OptionExt as _, ensure};
use prost_reflect::DescriptorPool;

#[test]
fn current_schema_has_valid_activity_metadata() -> eyre::Result<()> {
    let descriptors = DescriptorPool::decode(
        include_bytes!(concat!(env!("OUT_DIR"), "/bridge-descriptors.bin")).as_slice(),
    )?;
    let operations = activity::operations(&descriptors)?;
    ensure!(!activity::render(&operations).is_empty());
    Ok(())
}

#[test]
fn latest_wins_rejects_mutating_rpcs() -> eyre::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scratch = tempfile::tempdir()?;
    let output = scratch.path().join("activity.bin");
    let compiled = Command::new("protoc")
        .arg(format!("--proto_path={}", root.display()))
        .arg(format!("--proto_path={}", root.join("proto").display()))
        .arg("--include_imports")
        .arg(format!("--descriptor_set_out={}", output.display()))
        .arg(root.join("tests/fixtures/latest_wins_mutation.proto"))
        .output()?;
    ensure!(
        compiled.status.success(),
        "invalid-activity fixture did not compile: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = std::fs::read(output)?;
    let descriptors = DescriptorPool::decode(bytes.as_slice())?;
    let error = activity::operations(&descriptors)
        .err()
        .ok_or_eyre("the Rust generator accepted a mutating LATEST_WINS RPC")?;
    ensure!(
        error
            .to_string()
            .contains("LATEST_WINS requires SESSION_ACCESS_READ_ONLY")
    );
    Ok(())
}
