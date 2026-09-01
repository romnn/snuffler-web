//! Compiles the bridge protobuf schema with prost, and generates a typed client
//! for `BridgeService` via a custom `ServiceGenerator` (the same prost-build hook
//! tonic-build uses). The client is transport-agnostic: it encodes the request,
//! calls a `BridgeTransport` (implemented by the frontend over the wasm/worker
//! bridge), and unwraps the `BridgeResult` envelope into the typed response — so
//! the (method, request type, response type) binding is checked by the compiler
//! and can never drift from the .proto.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use prost_reflect::{DescriptorPool, DynamicMessage, Kind, ReflectMessage as _};

struct RpcActivity {
    name: String,
    label: String,
    lane: String,
    policy: String,
    secondary_feedback: bool,
    cancellation: String,
    supersedes: bool,
    may_update_markers: bool,
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

fn invalid_data(message: impl Into<String>) -> Box<dyn std::error::Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
}

fn upper_camel_to_upper_snake(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            output.push('_');
        }
        output.push(character.to_ascii_uppercase());
    }
    output
}

fn rust_variant_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for word in name.split('_').filter(|word| !word.is_empty()) {
        let mut characters = word.chars();
        if let Some(first) = characters.next() {
            output.push(first.to_ascii_uppercase());
            output.extend(characters.map(|character| character.to_ascii_lowercase()));
        }
    }
    output
}

fn activity_enum_variant(
    activity: &DynamicMessage,
    field: &str,
    method: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let descriptor = activity
        .descriptor()
        .get_field_by_name(field)
        .ok_or_else(|| invalid_data(format!("Activity.{field} descriptor is missing")))?;
    let Kind::Enum(enum_descriptor) = descriptor.kind() else {
        return Err(invalid_data(format!("Activity.{field} is not an enum")));
    };
    let value = activity
        .get_field(&descriptor)
        .as_enum_number()
        .ok_or_else(|| invalid_data(format!("Activity.{field} is not an enum value")))?;
    let name = enum_descriptor
        .get_value(value)
        .ok_or_else(|| {
            invalid_data(format!(
                "BridgeService.{method}: activity {field} has unknown {} value {value}",
                enum_descriptor.name()
            ))
        })?
        .name()
        .to_owned();
    let prefix = format!("{}_", upper_camel_to_upper_snake(enum_descriptor.name()));
    let name = name.strip_prefix(&prefix).ok_or_else(|| {
        invalid_data(format!(
            "protobuf enum value {name} does not start with {prefix}"
        ))
    })?;
    if name == "UNSPECIFIED" {
        return Err(invalid_data(format!(
            "BridgeService.{method}: activity {field} is UNSPECIFIED"
        )));
    }
    Ok(rust_variant_name(name))
}

fn activity_string(
    activity: &DynamicMessage,
    field: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    activity
        .get_field_by_name(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| invalid_data(format!("Activity.{field} is not a string")))
}

fn activity_bool(
    activity: &DynamicMessage,
    field: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    activity
        .get_field_by_name(field)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| invalid_data(format!("Activity.{field} is not a bool")))
}

fn activity_methods(
    descriptors: &DescriptorPool,
) -> Result<Vec<RpcActivity>, Box<dyn std::error::Error>> {
    let service = descriptors
        .get_service_by_name("snuffler.bridge.v1.BridgeService")
        .ok_or_else(|| invalid_data("BridgeService descriptor is missing"))?;
    let activity_extension = descriptors
        .get_extension_by_name("snuffler.bridge.v1.activity")
        .ok_or_else(|| invalid_data("activity method-option descriptor is missing"))?;
    let mut methods = Vec::new();
    for method in service.methods() {
        let name = method.name();
        let options = method.options();
        if !options.has_extension(&activity_extension) {
            return Err(invalid_data(format!(
                "BridgeService.{name}: missing required activity option"
            )));
        }
        let activity_value = options.get_extension(&activity_extension);
        let activity = activity_value.as_message().ok_or_else(|| {
            invalid_data(format!(
                "BridgeService.{name}: activity option is not a message"
            ))
        })?;
        let label = activity_string(activity, "label")?;
        if label.is_empty() {
            return Err(invalid_data(format!(
                "BridgeService.{name}: activity label is empty"
            )));
        }
        let lane = activity_enum_variant(activity, "lane", name)?;
        let policy = activity_enum_variant(activity, "policy", name)?;
        // Session access is consumed only by the Python dispatcher. Validate it
        // here too so no generated endpoint accepts an unspecified contract.
        let _session_access = activity_enum_variant(activity, "session_access", name)?;
        let cancellation = activity_enum_variant(activity, "cancellation", name)?;
        methods.push(RpcActivity {
            name: name.to_owned(),
            label,
            lane,
            policy,
            secondary_feedback: activity_bool(activity, "secondary_feedback")?,
            cancellation,
            supersedes: activity_bool(activity, "supersedes")?,
            may_update_markers: activity_bool(activity, "may_update_markers")?,
        });
    }
    Ok(methods)
}

struct LocalOperationMetadata {
    name: &'static str,
    label: &'static str,
    lane: &'static str,
    policy: &'static str,
    secondary_feedback: bool,
    cancellation: &'static str,
    may_update_markers: bool,
    exclusive_across_lanes: bool,
}

/// Executor-owned operations with no rpc: they never cross the worker, so their
/// metadata is declared here rather than in the .proto. Shared by the enum
/// listing and the match arms so the two cannot drift.
const LOCAL_OPERATIONS: [LocalOperationMetadata; 8] = [
    LocalOperationMetadata {
        name: "ReloadSnufflings",
        label: "Reloading snufflings",
        lane: "Job",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "NotSupported",
        may_update_markers: false,
        exclusive_across_lanes: true,
    },
    LocalOperationMetadata {
        name: "OpenSelectedFiles",
        label: "Opening files",
        lane: "Job",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "Supported",
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "OpenDirectory",
        label: "Opening directory",
        lane: "Job",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "Supported",
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "ReloadOpenFiles",
        label: "Reloading files",
        lane: "Job",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "Supported",
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "PollWatchedFiles",
        label: "Checking watched files",
        lane: "Job",
        policy: "Dedup",
        secondary_feedback: true,
        cancellation: "Supported",
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "ExportSvg",
        label: "Exporting SVG",
        lane: "Local",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "NotSupported",
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "ExportPng",
        label: "Exporting PNG",
        lane: "Local",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "NotSupported",
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
    LocalOperationMetadata {
        name: "SerializeMarkers",
        label: "Serializing markers",
        lane: "Local",
        policy: "Queue",
        secondary_feedback: false,
        cancellation: "NotSupported",
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
];

fn activity_source(methods: &[RpcActivity]) -> String {
    let mut out = String::new();
    out.push_str(
        r"#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane { Job, View, Local }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Policy { Queue, Dedup, LatestWins }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancellationCapability { NotSupported, Supported }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActivityMeta {
    pub label: &'static str,
    pub lane: Lane,
    pub policy: Policy,
    pub secondary_feedback: bool,
    pub cancellation: CancellationCapability,
    pub supersedes: bool,
    pub may_update_markers: bool,
    pub exclusive_across_lanes: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
",
    );
    for activity in methods {
        let _ = writeln!(out, "    {},", activity.name);
    }
    for operation in &LOCAL_OPERATIONS {
        let _ = writeln!(out, "    {},", operation.name);
    }
    out.push_str("}\nimpl Operation {\n    pub const fn activity(self) -> ActivityMeta {\n        match self {\n");
    for activity in methods {
        let _ = writeln!(
            out,
            "            Self::{} => ActivityMeta {{ label: {}, lane: Lane::{}, policy: Policy::{}, secondary_feedback: {}, cancellation: CancellationCapability::{}, supersedes: {}, may_update_markers: {}, exclusive_across_lanes: false }},",
            activity.name,
            rust_string(&activity.label),
            activity.lane,
            activity.policy,
            activity.secondary_feedback,
            activity.cancellation,
            activity.supersedes,
            activity.may_update_markers,
        );
    }
    for operation in LOCAL_OPERATIONS {
        let _ = writeln!(
            out,
            "            Self::{} => ActivityMeta {{ label: {}, lane: Lane::{}, policy: Policy::{}, secondary_feedback: {}, cancellation: CancellationCapability::{}, supersedes: false, may_update_markers: {}, exclusive_across_lanes: {} }},",
            operation.name,
            rust_string(operation.label),
            operation.lane,
            operation.policy,
            operation.secondary_feedback,
            operation.cancellation,
            operation.may_update_markers,
            operation.exclusive_across_lanes,
        );
    }
    out.push_str("        }\n    }\n}\n");
    out
}

fn generate_activity(
    descriptors: &DescriptorPool,
    output_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let methods = activity_methods(descriptors)?;
    std::fs::write(output_path, activity_source(&methods))?;
    Ok(())
}

/// Last path segment of a prost type path (`a.b.C` / `a::b::C` -> `C`).
fn last_segment(ty: &str) -> &str {
    ty.rsplit([':', '.']).next().unwrap_or(ty)
}

fn bridge_result_variants(
    descriptors: &DescriptorPool,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let result = descriptors
        .get_message_by_name("snuffler.bridge.v1.BridgeResult")
        .ok_or_else(|| invalid_data("BridgeResult descriptor is missing"))?;
    let mut variants = HashMap::new();
    for field in result.fields() {
        let field_name = field.name();
        let Kind::Message(response_message) = field.kind() else {
            return Err(invalid_data(format!(
                "BridgeResult.{field_name} does not carry a message"
            )));
        };
        let response = response_message.name().to_owned();
        let variant = rust_variant_name(field_name);
        if variants.insert(response.clone(), variant).is_some() {
            return Err(invalid_data(format!(
                "BridgeResult carries {response} more than once"
            )));
        }
    }
    Ok(variants)
}

struct BridgeClientGen {
    response_variants: HashMap<String, String>,
}

fn generate_client_method(
    method: &prost_build::Method,
    service: &str,
    response_variants: &HashMap<String, String>,
    buf: &mut String,
) {
    let req = last_segment(&method.input_type).to_string();
    let out = last_segment(&method.output_type).to_string();
    let Some(variant) = response_variants.get(&out) else {
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
            expected_generation: stale.expected_generation,
            actual_generation: stale.actual_generation,
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
            generate_client_method(method, &svc, &self.response_variants, buf);
        }

        buf.push_str("}\n");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const PROTO_ROOT: &str = "../../proto";
    const PROTO_FILES: [&str; 8] = [
        "../../proto/snuffler/bridge/v1/common.proto",
        "../../proto/snuffler/bridge/v1/scene.proto",
        "../../proto/snuffler/bridge/v1/view.proto",
        "../../proto/snuffler/bridge/v1/snuffling.proto",
        "../../proto/snuffler/bridge/v1/figure.proto",
        "../../proto/snuffler/bridge/v1/data.proto",
        "../../proto/snuffler/bridge/v1/activity.proto",
        "../../proto/snuffler/bridge/v1/bridge.proto",
    ];
    for file in PROTO_FILES {
        println!("cargo:rerun-if-changed={file}");
    }
    let output_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| invalid_data("Cargo did not set OUT_DIR for the build script"))?;
    let descriptor_path = Path::new(&output_dir).join("bridge-descriptors.bin");
    let activity_path = Path::new(&output_dir).join("bridge-activity.rs");
    let mut config = prost_build::Config::new();
    config.file_descriptor_set_path(&descriptor_path);
    let descriptor_set = config.load_fds(&PROTO_FILES, &[PROTO_ROOT])?;
    let descriptor_bytes = std::fs::read(&descriptor_path)?;
    let descriptors = DescriptorPool::decode(descriptor_bytes.as_slice())?;
    let response_variants = bridge_result_variants(&descriptors)?;
    config.service_generator(Box::new(BridgeClientGen { response_variants }));
    config.compile_fds(descriptor_set)?;
    generate_activity(&descriptors, &activity_path)?;
    Ok(())
}
