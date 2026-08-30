//! Compiles the bridge protobuf schema with prost, and generates a typed client
//! for `BridgeService` via a custom `ServiceGenerator` (the same prost-build hook
//! tonic-build uses). The client is transport-agnostic: it encodes the request,
//! calls a `BridgeTransport` (implemented by the frontend over the wasm/worker
//! bridge), and unwraps the `BridgeResult` envelope into the typed response — so
//! the (method, request type, response type) binding is checked by the compiler
//! and can never drift from the .proto.

use std::fmt::Write as _;
use std::path::Path;

use prost::Message as _;

#[derive(Clone, PartialEq, prost::Message)]
struct DescriptorSet {
    #[prost(message, repeated, tag = "1")]
    file: Vec<FileDescriptor>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct FileDescriptor {
    #[prost(message, repeated, tag = "6")]
    service: Vec<ServiceDescriptor>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ServiceDescriptor {
    #[prost(string, optional, tag = "1")]
    name: Option<String>,
    #[prost(message, repeated, tag = "2")]
    method: Vec<MethodDescriptor>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct MethodDescriptor {
    #[prost(string, optional, tag = "1")]
    name: Option<String>,
    #[prost(message, optional, tag = "4")]
    options: Option<MethodOptions>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct MethodOptions {
    #[prost(message, optional, tag = "51000")]
    activity: Option<ActivityOption>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ActivityOption {
    #[prost(string, tag = "1")]
    label: String,
    #[prost(int32, tag = "2")]
    lane: i32,
    #[prost(int32, tag = "3")]
    policy: i32,
    #[prost(int32, tag = "4")]
    effect: i32,
    #[prost(bool, tag = "5")]
    background: bool,
    #[prost(int32, tag = "6")]
    cancel: i32,
    #[prost(bool, tag = "7")]
    supersedes: bool,
    #[prost(bool, tag = "8")]
    marker_output: bool,
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

fn invalid_data(message: impl Into<String>) -> Box<dyn std::error::Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
}

fn enum_value(
    value: i32,
    values: &[&str],
    field: &str,
    method: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let value = usize::try_from(value)
        .ok()
        .and_then(|index| values.get(index))
        .filter(|value| **value != "Unspecified")
        .ok_or_else(|| {
            invalid_data(format!(
                "BridgeService.{method}: activity {field} is UNSPECIFIED"
            ))
        })?;
    Ok((*value).to_string())
}

fn activity_methods(
    descriptors: &DescriptorSet,
) -> Result<Vec<(&str, &ActivityOption)>, Box<dyn std::error::Error>> {
    let service = descriptors
        .file
        .iter()
        .flat_map(|file| &file.service)
        .find(|service| service.name.as_deref() == Some("BridgeService"))
        .ok_or_else(|| invalid_data("BridgeService descriptor is missing"))?;
    let mut methods = Vec::new();
    for method in &service.method {
        let name = method
            .name
            .as_deref()
            .ok_or_else(|| invalid_data("BridgeService method name is missing"))?;
        let activity = method
            .options
            .as_ref()
            .and_then(|options| options.activity.as_ref())
            .ok_or_else(|| {
                invalid_data(format!(
                    "BridgeService.{name}: missing required activity option"
                ))
            })?;
        if activity.label.is_empty() {
            return Err(invalid_data(format!(
                "BridgeService.{name}: activity label is empty"
            )));
        }
        methods.push((name, activity));
    }
    Ok(methods)
}

/// Executor-owned operations with no rpc: they never cross the worker, so their
/// metadata is declared here rather than in the .proto. Shared by the enum
/// listing and the match arms so the two cannot drift.
const LOCAL_OPERATIONS: [(&str, &str, &str, bool); 7] = [
    ("ReloadSnufflings", "Reloading snufflings", "Job", true),
    ("ExportSvg", "Exporting SVG", "Local", false),
    ("ExportPng", "Exporting PNG", "Local", false),
    ("MarkersToFile", "Serializing markers", "Local", false),
    ("SaveMarkers", "Saving markers", "Local", false),
    ("SaveWaveforms", "Saving waveforms", "Local", false),
    ("WatchPoll", "Checking watched files", "Local", false),
];

fn activity_prelude(methods: &[(&str, &ActivityOption)]) -> String {
    let mut out = String::new();
    out.push_str(
        r"#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane { Job, View, Local }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Policy { Queue, Dedup, LatestWins }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelCap { NotCancellable, Cancellable }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActivityMeta {
    pub label: &'static str,
    pub lane: Lane,
    pub policy: Policy,
    pub background: bool,
    pub cancel: CancelCap,
    pub supersedes: bool,
    pub marker_output: bool,
    pub exclusive_both_lanes: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
",
    );
    for (name, _) in methods {
        let _ = writeln!(out, "    {name},");
    }
    for (name, _, _, _) in LOCAL_OPERATIONS {
        let _ = writeln!(out, "    {name},");
    }
    out.push_str("}\nimpl Operation {\n    pub const fn activity(self) -> ActivityMeta {\n        match self {\n");
    out
}

fn write_rpc_activity(
    out: &mut String,
    methods: &[(&str, &ActivityOption)],
) -> Result<(), Box<dyn std::error::Error>> {
    for (name, activity) in methods {
        let lane = enum_value(activity.lane, &["Unspecified", "Job", "View"], "lane", name)?;
        let policy = enum_value(
            activity.policy,
            &["Unspecified", "Queue", "Dedup", "LatestWins"],
            "policy",
            name,
        )?;
        // Effect is consumed only by the Python dispatcher; it is still
        // validated here so an rpc cannot ship with it unspecified.
        enum_value(
            activity.effect,
            &["Unspecified", "Read", "Mutate"],
            "effect",
            name,
        )?;
        let cancel = enum_value(
            activity.cancel,
            &["Unspecified", "NotCancellable", "Cancellable"],
            "cancel",
            name,
        )?;
        let _ = writeln!(
            out,
            "            Self::{name} => ActivityMeta {{ label: {}, lane: Lane::{lane}, policy: Policy::{policy}, background: {}, cancel: CancelCap::{cancel}, supersedes: {}, marker_output: {}, exclusive_both_lanes: false }},",
            rust_string(&activity.label),
            activity.background,
            activity.supersedes,
            activity.marker_output,
        );
    }
    Ok(())
}

fn finish_activity(mut out: String) -> String {
    for (name, label, lane, exclusive_both_lanes) in LOCAL_OPERATIONS {
        let background = name == "WatchPoll";
        let _ = writeln!(
            out,
            "            Self::{name} => ActivityMeta {{ label: {}, lane: Lane::{lane}, policy: Policy::Queue, background: {background}, cancel: CancelCap::NotCancellable, supersedes: false, marker_output: false, exclusive_both_lanes: {exclusive_both_lanes} }},",
            rust_string(label),
        );
    }
    out.push_str("        }\n    }\n}\n");
    out
}

fn generate_activity(
    descriptor_path: &Path,
    output_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(descriptor_path)?;
    let descriptors = DescriptorSet::decode(bytes.as_slice())?;
    let methods = activity_methods(&descriptors)?;
    let mut output = activity_prelude(&methods);
    write_rpc_activity(&mut output, &methods)?;
    std::fs::write(output_path, finish_activity(output))?;
    Ok(())
}

/// The success message type each `BridgeResult` oneof variant carries. The rpc's
/// response type is matched against this to pick the variant to unwrap.
fn variant_for(output: &str) -> Option<&'static str> {
    match output {
        "SceneMeta" => Some("Scene"),
        "ViewMeta" => Some("View"),
        "FigureResult" => Some("Figure"),
        "SnufflingResult" => Some("Snuffling"),
        "MapData" => Some("MapData"),
        "StationDistanceList" => Some("StationDistances"),
        "ActiveEventResult" => Some("ActiveEvent"),
        "CommandResult" => Some("Command"),
        "SnufMarkerList" => Some("Markers"),
        "StringValue" => Some("Text"),
        "ExampleList" => Some("Examples"),
        "BundleResult" => Some("Bundle"),
        "FileBlob" => Some("Blob"),
        "LoadBytesResult" => Some("LoadBytes"),
        "IngestMiniSeedResult" => Some("Ingest"),
        _ => None,
    }
}

/// Last path segment of a prost type path (`a.b.C` / `a::b::C` -> `C`).
fn last_segment(ty: &str) -> &str {
    ty.rsplit([':', '.']).next().unwrap_or(ty)
}

#[derive(Default)]
struct BridgeClientGen;

fn generate_client_method(method: &prost_build::Method, service: &str, buf: &mut String) {
    let req = last_segment(&method.input_type).to_string();
    let out = last_segment(&method.output_type).to_string();
    let Some(variant) = variant_for(&out) else {
        let _ = writeln!(
            buf,
            "compile_error!(\"BridgeService response {out} has no BridgeResult variant\");"
        );
        return;
    };
    let wire = format!("/{service}/{}", method.proto_name);
    let (params, encode_expr) = if req == "Empty" {
        (String::new(), "encode_msg(&Empty::default())".to_string())
    } else {
        (
            format!(", request: {req}"),
            "encode_msg(&request)".to_string(),
        )
    };
    let operation = &method.proto_name;
    if out == "ViewMeta" {
        let _ = write!(
            buf,
            r#"
    pub async fn {name}(&self{params}) -> Result<({out}, Vec<f32>), crate::CallError> {{
        let (bytes, ys) = self
            .transport
            .unary_with_buffer(Operation::{operation}, "{wire}", {encode_expr})
            .await?;
        match bridge_ok(&bytes, "{wire}")? {{
            bridge_result::Result::{variant}(m) => Ok((m, ys)),
            _ => Err(crate::CallError::UnexpectedPayload {{ method: "{wire}" }}),
        }}
    }}
"#,
            name = method.name,
        );
    } else {
        let _ = write!(
            buf,
            r#"
    pub async fn {name}(&self{params}) -> Result<{out}, crate::CallError> {{
        let bytes = self.transport.unary(Operation::{operation}, "{wire}", {encode_expr}).await?;
        match bridge_ok(&bytes, "{wire}")? {{
            bridge_result::Result::{variant}(m) => Ok(m),
            _ => Err(crate::CallError::UnexpectedPayload {{ method: "{wire}" }}),
        }}
    }}
"#,
            name = method.name,
        );
    }
}

impl prost_build::ServiceGenerator for BridgeClientGen {
    fn generate(&mut self, service: prost_build::Service, buf: &mut String) {
        // The canonical gRPC method id: `/package.Service/Method`. The generated
        // Python dispatch derives the same string from the descriptor, so the two
        // generated ends agree by construction; the JS worker just relays it.
        let svc = format!("{}.{}", service.package, service.proto_name);
        // Shared transport + envelope-unwrap helpers, emitted once.
        buf.push_str(
            r#"
/// Transport for the generated `BridgeServiceClient`: marshals an encoded
/// request across the bridge boundary and returns the encoded `BridgeResult`
/// (plus the out-of-band `ys` values for the two buffer-carrying methods). The
/// frontend implements this over the wasm worker; native builds never call it.
///
/// `method` is `&'static str` because every method id is a compile-time constant
/// generated from the .proto — nothing can name an rpc that is not in the schema.
pub trait BridgeTransport {
    #[expect(async_fn_in_trait, reason = "the generated client is internal to this workspace")]
    async fn unary(
        &self,
        operation: Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<Vec<u8>, crate::TransportError>;
    #[expect(async_fn_in_trait, reason = "the generated client is internal to this workspace")]
    async fn unary_with_buffer(
        &self,
        operation: Operation,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<(Vec<u8>, Vec<f32>), crate::TransportError>;
}

fn encode_msg<M: ::prost::Message>(m: &M) -> Vec<u8> {
    let mut buf = Vec::with_capacity(::prost::Message::encoded_len(m));
    let _ = ::prost::Message::encode(m, &mut buf);
    buf
}

// Decode the BridgeResult envelope: error variant -> CallError::Remote, success
// variant -> the contained oneof value. Python never raises across the boundary,
// so this is the single place a remote error becomes an Err in Rust.
fn bridge_ok(
    bytes: &[u8],
    method: &'static str,
) -> Result<bridge_result::Result, crate::CallError> {
    let envelope = <BridgeResult as ::prost::Message>::decode(bytes)
        .map_err(|source| crate::CallError::Decode { method, source })?;
    match envelope.result {
        Some(bridge_result::Result::Error(e)) => Err(crate::RemoteError::from(e).into()),
        Some(bridge_result::Result::Cancelled(_)) => Err(crate::CallError::Cancelled { method }),
        Some(bridge_result::Result::Stale(stale)) => Err(crate::CallError::Stale {
            method,
            expected: stale.expected,
            actual: stale.actual,
        }),
        Some(other) => Ok(other),
        None => Err(crate::CallError::Empty { method }),
    }
}

/// Typed client for `BridgeService`, generated from the .proto. One method per
/// rpc: encode the request, call the transport, unwrap the matching response
/// variant. Generated — do not hand-edit; add/changes rpcs in the .proto.
pub struct BridgeServiceClient<T> {
    transport: T,
}

impl<T: BridgeTransport> BridgeServiceClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
"#,
        );

        for method in &service.methods {
            generate_client_method(method, &svc, buf);
        }

        buf.push_str("}\n");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for file in [
        "common.proto",
        "scene.proto",
        "view.proto",
        "snuffling.proto",
        "figure.proto",
        "data.proto",
        "activity.proto",
        "bridge.proto",
    ] {
        println!("cargo:rerun-if-changed=../../proto/snuffler/bridge/v1/{file}");
    }
    let output_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| invalid_data("Cargo did not set OUT_DIR for the build script"))?;
    let descriptor_path = Path::new(&output_dir).join("bridge-descriptors.bin");
    let activity_path = Path::new(&output_dir).join("bridge-activity.rs");
    prost_build::Config::new()
        .file_descriptor_set_path(&descriptor_path)
        .service_generator(Box::new(BridgeClientGen))
        .compile_protos(
            &[
                "../../proto/snuffler/bridge/v1/common.proto",
                "../../proto/snuffler/bridge/v1/scene.proto",
                "../../proto/snuffler/bridge/v1/view.proto",
                "../../proto/snuffler/bridge/v1/snuffling.proto",
                "../../proto/snuffler/bridge/v1/figure.proto",
                "../../proto/snuffler/bridge/v1/data.proto",
                "../../proto/snuffler/bridge/v1/activity.proto",
                "../../proto/snuffler/bridge/v1/bridge.proto",
            ],
            &["../../proto"],
        )?;
    generate_activity(&descriptor_path, &activity_path)?;
    Ok(())
}
