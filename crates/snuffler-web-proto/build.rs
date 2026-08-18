//! Compiles the bridge protobuf schema with prost, and generates a typed client
//! for `BridgeService` via a custom `ServiceGenerator` (the same prost-build hook
//! tonic-build uses). The client is transport-agnostic: it encodes the request,
//! calls a `BridgeTransport` (implemented by the frontend over the wasm/worker
//! bridge), and unwraps the `BridgeResult` envelope into the typed response — so
//! the (method, request type, response type) binding is checked by the compiler
//! and can never drift from the .proto.

use std::fmt::Write as _;

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
        _ => None,
    }
}

/// Last path segment of a prost type path (`a.b.C` / `a::b::C` -> `C`).
fn last_segment(ty: &str) -> &str {
    ty.rsplit([':', '.']).next().unwrap_or(ty)
}

#[derive(Default)]
struct BridgeClientGen;

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
    #[allow(async_fn_in_trait, reason = "the generated client is internal to this workspace")]
    async fn unary(
        &self,
        method: &'static str,
        request: Vec<u8>,
    ) -> Result<Vec<u8>, crate::TransportError>;
    #[allow(async_fn_in_trait, reason = "the generated client is internal to this workspace")]
    async fn unary_with_buffer(
        &self,
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

        for m in &service.methods {
            let req = last_segment(&m.input_type).to_string();
            let out = last_segment(&m.output_type).to_string();
            let Some(variant) = variant_for(&out) else {
                let _ = writeln!(
                    buf,
                    "compile_error!(\"BridgeService response {out} has no BridgeResult variant\");"
                );
                continue;
            };
            let wire = format!("/{svc}/{}", m.proto_name);
            let buffer = out == "ViewMeta";

            // Empty-request methods take no parameter (the worker stages any
            // out-of-band input); all others take the typed request message.
            let (params, encode_expr) = if req == "Empty" {
                (String::new(), "encode_msg(&Empty::default())".to_string())
            } else {
                (
                    format!(", request: {req}"),
                    "encode_msg(&request)".to_string(),
                )
            };

            if buffer {
                let _ = write!(
                    buf,
                    r#"
    pub async fn {name}(&self{params}) -> Result<({out}, Vec<f32>), crate::CallError> {{
        let (bytes, ys) = self
            .transport
            .unary_with_buffer("{wire}", {encode_expr})
            .await?;
        match bridge_ok(&bytes, "{wire}")? {{
            bridge_result::Result::{variant}(m) => Ok((m, ys)),
            _ => Err(crate::CallError::UnexpectedPayload {{ method: "{wire}" }}),
        }}
    }}
"#,
                    name = m.name,
                );
            } else {
                let _ = write!(
                    buf,
                    r#"
    pub async fn {name}(&self{params}) -> Result<{out}, crate::CallError> {{
        let bytes = self.transport.unary("{wire}", {encode_expr}).await?;
        match bridge_ok(&bytes, "{wire}")? {{
            bridge_result::Result::{variant}(m) => Ok(m),
            _ => Err(crate::CallError::UnexpectedPayload {{ method: "{wire}" }}),
        }}
    }}
"#,
                    name = m.name,
                );
            }
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
        "bridge.proto",
    ] {
        println!("cargo:rerun-if-changed=../../proto/snuffler/bridge/v1/{file}");
    }
    prost_build::Config::new()
        .service_generator(Box::new(BridgeClientGen))
        .compile_protos(
            &[
                "../../proto/snuffler/bridge/v1/common.proto",
                "../../proto/snuffler/bridge/v1/scene.proto",
                "../../proto/snuffler/bridge/v1/view.proto",
                "../../proto/snuffler/bridge/v1/snuffling.proto",
                "../../proto/snuffler/bridge/v1/figure.proto",
                "../../proto/snuffler/bridge/v1/data.proto",
                "../../proto/snuffler/bridge/v1/bridge.proto",
            ],
            &["../../proto"],
        )?;
    Ok(())
}
