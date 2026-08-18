pub use snuffler_web_proto::bridge::v1 as pb;

// The decoded prost messages ARE the domain model, so there is no proto<->app
// conversion layer here — only the DOM-only responses need their BridgeResult
// envelope unwrapped (error variant -> CallError::Remote, success variant ->
// the contained pb message).
// The `ys` buffer rides out-of-band and is attached to ViewData here, never in
// the proto.
//
// These are the DOM-only twins of the generated client's envelope handling: a
// file picker or a directory read reaches pyrocko through bridge.js rather than
// through the typed client, but comes back in the same envelope and so fails in
// the same ways.
// `what` names the action for the error message; bridge.js has already mapped it
// onto a real rpc.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::pb;
    use snuffler_web_proto::prost::Message;
    use snuffler_web_proto::{CallError, RemoteError};

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

    /// A file load that may have been a whole dataset.
    ///
    /// `openFiles` picks the rpc from the bytes it read — a zip is a bundle and
    /// everything else is a waveform file — so the response is one of two envelope
    /// variants.
    /// Both carry a scene.
    /// A bundle remains complete so its station/event result, requested view, and
    /// dataset note all reach the shared installer.
    pub enum DatasetResult {
        Waveform(pb::LoadBytesResult),
        Bundle(pb::BundleResult),
    }

    pub fn scene_or_bundle_from_bytes(
        bytes: &[u8],
        what: &'static str,
    ) -> Result<DatasetResult, CallError> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::LoadBytes(load) => Ok(DatasetResult::Waveform(load)),
            pb::bridge_result::Result::Bundle(bundle) => Ok(DatasetResult::Bundle(bundle)),
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
