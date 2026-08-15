pub use snuffler_web_proto::bridge::v1 as pb;

// The decoded prost messages ARE the domain model, so there is no proto<->app
// conversion layer here — only: (1) encode a request message to bytes, and
// (2) unwrap the BridgeResult envelope (error variant -> CallError::Remote,
// success variant -> the contained pb message). The `ys` buffer rides
// out-of-band and is attached to ViewData here, never in the proto.
//
// These are the DOM-only twins of the generated client's envelope handling: a
// file picker or a directory read reaches pyrocko through bridge.js rather than
// through the typed client, but comes back in the same envelope and so fails in
// the same ways. `what` names the action for the error message; bridge.js has
// already mapped it onto a real rpc.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::pb;
    use snuffler_web_proto::prost::Message;
    use snuffler_web_proto::{CallError, RemoteError};

    pub fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
        let mut out = Vec::with_capacity(msg.encoded_len());
        if msg.encode(&mut out).is_err() {
            return Vec::new();
        }
        out
    }

    pub fn decode_message<M: Message + Default>(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<M, CallError> {
        M::decode(bytes).map_err(|source| CallError::Decode {
            method: what,
            source,
        })
    }

    // Every bridge response is a BridgeResult envelope (one success variant OR a
    // BridgeError). Decode it and either return the matched oneof variant or turn
    // the error variant into a typed `CallError` for the frontend. Python never
    // raises across the boundary, so this is the single place errors enter Rust.
    fn bridge_oneof(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<pb::bridge_result::Result, CallError> {
        let br = decode_message::<pb::BridgeResult>(bytes, what)?;
        match br.result {
            Some(pb::bridge_result::Result::Error(e)) => Err(RemoteError::from(e).into()),
            Some(other) => Ok(other),
            None => Err(CallError::Empty { method: what }),
        }
    }

    fn unexpected(what: &'static str) -> CallError {
        CallError::UnexpectedPayload { method: what }
    }

    pub fn scene_from_bytes(bytes: &[u8], what: &'static str) -> Result<pb::SceneMeta, CallError> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Scene(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    /// A file load that may have been a whole dataset.
    ///
    /// `openFiles` picks the rpc from the bytes it read — a zip is a bundle and
    /// everything else is a waveform file — so the response is one of two envelope
    /// variants. Both carry a scene; only a bundle also carries the station and
    /// event results, which the caller applies exactly as a metadata load would.
    pub fn scene_or_bundle_from_bytes(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<(pb::SceneMeta, Option<pb::SnufflingResult>), CallError> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Scene(scene) => Ok((scene, None)),
            pb::bridge_result::Result::Bundle(bundle) => {
                Ok((bundle.scene.unwrap_or_default(), bundle.info))
            }
            _ => Err(unexpected(what)),
        }
    }

    pub fn markers_from_bytes(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<pb::SnufMarkerList, CallError> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Markers(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn snuffling_from_bytes(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<pb::SnufflingResult, CallError> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Snuffling(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
