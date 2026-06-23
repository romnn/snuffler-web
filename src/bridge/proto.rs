pub use snuffler_proto::bridge::v1 as pb;

// The decoded prost messages ARE the domain model, so there is no proto<->app
// conversion layer here — only: (1) encode a request message to bytes, and
// (2) unwrap the BridgeResult envelope (error variant -> Err(String), success
// variant -> the contained pb message). The zero-copy `ys` buffer rides
// out-of-band and is attached to ViewData/SpectrumData here, never in the proto.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::pb;
    use snuffler_proto::prost::Message;

    use crate::bridge::{SpectrumData, ViewData};

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

    pub fn snuffling_from_bytes(bytes: &[u8], what: &str) -> Result<pb::SnufflingResult, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Snuffling(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn command_from_bytes(bytes: &[u8], what: &str) -> Result<pb::CommandResult, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Command(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn map_from_bytes(bytes: &[u8], what: &str) -> Result<pb::MapData, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::MapData(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn cake_plot_from_bytes(bytes: &[u8], what: &str) -> Result<pb::CakePlotResult, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::CakePlot(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn markers_from_bytes(bytes: &[u8], what: &str) -> Result<pb::SnufMarkerList, String> {
        match bridge_oneof(bytes, what)? {
            pb::bridge_result::Result::Markers(m) => Ok(m),
            _ => Err(unexpected(what)),
        }
    }

    pub fn view_data_from_parts(result_bytes: &[u8], ys_bytes: &[u8]) -> Result<ViewData, String> {
        match bridge_oneof(result_bytes, "view")? {
            pb::bridge_result::Result::View(m) => Ok(ViewData {
                meta: m,
                ys: f32_vec_from_le_bytes(ys_bytes, "view.ys")?,
            }),
            _ => Err(unexpected("view")),
        }
    }

    pub fn spectrum_data_from_parts(
        result_bytes: &[u8],
        ys_bytes: &[u8],
    ) -> Result<SpectrumData, String> {
        match bridge_oneof(result_bytes, "spectrum")? {
            pb::bridge_result::Result::Spectrum(m) => Ok(SpectrumData {
                meta: m,
                ys: f32_vec_from_le_bytes(ys_bytes, "spectrum.ys")?,
            }),
            _ => Err(unexpected("spectrum")),
        }
    }

    pub fn encode_string_request(value: String) -> Vec<u8> {
        encode_message(&pb::StringRequest { value })
    }

    fn f32_vec_from_le_bytes(bytes: &[u8], what: &str) -> Result<Vec<f32>, String> {
        let mut chunks = bytes.chunks_exact(4);
        let mut out = Vec::with_capacity(bytes.len() / 4);
        for chunk in &mut chunks {
            let arr: [u8; 4] = chunk
                .try_into()
                .map_err(|_| format!("{what}: invalid f32 chunk"))?;
            out.push(f32::from_le_bytes(arr));
        }
        if !chunks.remainder().is_empty() {
            return Err(format!("{what}: byte length is not divisible by four"));
        }
        Ok(out)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
