//! Compiles the bridge protobuf schema and generates the Rust code built on it.
//!
//! The build produces three files in `OUT_DIR`, all included by `src/lib.rs`:
//!
//! - `snuffler_web_proto.rs`: the prost messages, followed by the typed `BridgeServiceClient`
//!   that [`client::ClientGenerator`] emits through prost's service-generator hook.
//! - `bridge-activity.rs`: the `Operation` enum and its scheduling metadata, read from each
//!   rpc's `activity` method option by [`activity`].
//! - `bridge-descriptors.bin`: the raw descriptor set the two generators reflect over.
//!
//! Every binding between an rpc, its request, its response and its `BridgeResult` variant is
//! derived from the schema, so the generated code cannot drift from the `.proto` files.

mod activity;
mod client;

use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, OptionExt as _, WrapErr as _};
use proc_macro2::TokenStream;
use prost_reflect::DescriptorPool;

/// Directory that every `import` in the schema is resolved against.
const PROTO_ROOT: &str = "../../proto";

/// Schema files of the `snuffler_web_proto` package, relative to its directory.
const PROTO_FILES: [&str; 8] = [
    "common.proto",
    "scene.proto",
    "view.proto",
    "snuffling.proto",
    "figure.proto",
    "data.proto",
    "activity.proto",
    "bridge.proto",
];

fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let proto_files = PROTO_FILES.map(|file| format!("{PROTO_ROOT}/snuffler_web_proto/{file}"));
    for file in &proto_files {
        println!("cargo:rerun-if-changed={file}");
    }

    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").ok_or_eyre("cargo did not set OUT_DIR for the build script")?,
    );
    let descriptor_path = out_dir.join("bridge-descriptors.bin");

    // Parse the schema
    let mut config = prost_build::Config::new();
    config.file_descriptor_set_path(&descriptor_path);
    let descriptor_set = config
        .load_fds(&proto_files, &[PROTO_ROOT])
        .wrap_err("failed to parse the bridge schema")?;

    // Reflect over the raw descriptor bytes rather than `descriptor_set`.
    // prost drops unknown fields when decoding, and the custom `activity` method option is
    // one, so only the bytes protoc wrote still carry it.
    let descriptor_bytes = fs::read(&descriptor_path)
        .wrap_err_with(|| format!("failed to read {}", descriptor_path.display()))?;
    let descriptors = DescriptorPool::decode(descriptor_bytes.as_slice())
        .wrap_err("failed to decode the bridge descriptor set")?;

    // Messages and the typed client
    let response_variants = client::response_variants(&descriptors)?;
    config.service_generator(Box::new(client::ClientGenerator { response_variants }));
    config
        .compile_fds(descriptor_set)
        .wrap_err("failed to generate the bridge messages")?;

    // Operation table
    let operations = activity::operations(&descriptors)?;
    write_rust(
        &out_dir.join("bridge-activity.rs"),
        activity::render(&operations),
    )
}

/// Formats `tokens` as a Rust source file and writes it to `path`.
fn write_rust(path: &Path, tokens: TokenStream) -> eyre::Result<()> {
    let file: syn::File = syn::parse2(tokens)
        .wrap_err_with(|| format!("generated {} is not valid Rust", path.display()))?;
    fs::write(path, prettyplease::unparse(&file))
        .wrap_err_with(|| format!("failed to write {}", path.display()))
}
