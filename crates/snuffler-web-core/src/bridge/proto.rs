//! The generated protobuf types, and decoding of the messages `bridge.js` returns.

pub use snuffler_web_proto::bridge::v1 as pb;

/// Decodes a protobuf message a DOM adapter returned (a file page, a cancel reply).
/// `adapter` names it in a typed decode failure.
#[cfg(target_arch = "wasm32")]
pub fn decode_message<M: snuffler_web_proto::prost::Message + Default>(
    bytes: &[u8],
    adapter: &'static str,
) -> Result<M, snuffler_web_proto::CallError> {
    M::decode(bytes).map_err(|source| snuffler_web_proto::CallError::Decode {
        method: adapter,
        source,
    })
}
