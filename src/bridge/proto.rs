pub use snuffler_proto::bridge::v1 as pb;

// The decoded prost messages ARE the domain model, so there is no proto<->app
// conversion layer here — only: (1) encode a request message to bytes, and
// (2) unwrap the BridgeResult envelope (error variant -> Err(String), success
// variant -> the contained pb message). The `ys` buffer rides out-of-band and is
// attached to ViewData/SpectrumData here, never in the proto.
mod wasm {
    use super::pb;
    use snuffler_proto::prost::Message;

    pub fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
        let mut out = Vec::with_capacity(msg.encoded_len());
        if msg.encode(&mut out).is_err() {
            return Vec::new();
        }
        out
    }

    pub fn decode_message<M: Message + Default>(bytes: &[u8], what: &str) -> Result<M, String> {
        M::decode(bytes).map_err(|e| format!("{what}: {e}"))
    }

    // Every bridge response is a BridgeResult envelope (one success variant OR a
    // BridgeError). Decode it and either return the matched oneof variant or turn
    // the error variant into an `Err(String)` for the UI. Python never raises
    // across the boundary, so this is the single place errors enter Rust.
    fn bridge_oneof(bytes: &[u8], what: &str) -> Result<pb::bridge_result::Result, String> {
        let br = decode_message::<pb::BridgeResult>(bytes, what)?;
        match br.result {
            Some(pb::bridge_result::Result::Error(e)) => Err(if e.message.is_empty() {
                format!("{what}: {}", e.kind)
            } else {
                format!("{}: {}", e.kind, e.message)
            }),
            Some(other) => Ok(other),
            None => Err(format!("{what}: empty BridgeResult")),
        }
    }

    fn unexpected(what: &str) -> String {
        format!("{what}: unexpected BridgeResult variant")
    }

    pub fn scene_from_bytes(bytes: &[u8], what: &str) -> Result<pb::SceneMeta, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Scene(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn markers_from_bytes(bytes: &[u8], what: &str) -> Result<pb::SnufMarkerList, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Markers(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }
}

pub use wasm::*;
