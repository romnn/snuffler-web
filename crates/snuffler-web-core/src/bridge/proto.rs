pub use snuffler_web_proto::bridge::v1 as pb;

// DOM adapters return protobuf manifests, decoded here before Rust submits any Python
// work through the generated client.
// `what` names the adapter for typed decode failures.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use snuffler_web_proto::CallError;
    use snuffler_web_proto::prost::Message;

    pub fn decode_message<M: Message + Default>(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<M, CallError> {
        M::decode(bytes).map_err(|source| CallError::Decode {
            method: what,
            source,
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
