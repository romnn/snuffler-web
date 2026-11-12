//! The `web/bridge.js` imports and the helpers that read the values they return.

use super::*;

// The module path escapes the crate (`/` roots it at the crate directory,
// `../..` walks up to the repo), so wasm-bindgen cannot embed the file as a
// snippet: the emitted import resolves to `web/gen/bridge.js` under the site
// root, and the trunk post-build hook must stage the file there
// (`stage_bridge_module` in scripts/trunk-post-build.sh).
#[wasm_bindgen(module = "/../../web/gen/bridge.js")]
extern "C" {
    // The single transport seam for every unary proto RPC:
    // (method, request bytes) -> { result: BridgeResult bytes, ys: Uint8Array }.
    // bridge.js forwards it to the Pyodide worker, which runs it through
    // `snuffler_bridge.service.call` and ships the out-of-band ys buffer alongside the proto.
    // The generated typed client rides on it through `WorkerTransport`.
    #[wasm_bindgen(js_name = call, catch)]
    pub(super) async fn js_call(
        method: &str,
        request: Vec<u8>,
        job_id: u64,
    ) -> Result<JsValue, JsValue>;

    // Runtime lifecycle and worker events
    #[wasm_bindgen(js_name = takeOperationEvents)]
    pub(super) fn js_take_operation_events() -> js_sys::Array;

    #[wasm_bindgen(js_name = seisbenchDevice)]
    pub(super) fn js_seisbench_device() -> JsValue;

    #[wasm_bindgen(js_name = runtimeInfo)]
    pub(super) fn js_runtime_info() -> JsValue;

    #[wasm_bindgen(js_name = computeBudget)]
    pub(super) fn js_compute_budget() -> Vec<u8>;

    #[wasm_bindgen(js_name = currentRuntimeEpoch)]
    pub(super) fn js_current_runtime_epoch() -> u32;

    #[wasm_bindgen(js_name = cancelJob, catch)]
    pub(super) fn js_cancel_job(request: Vec<u8>) -> Result<Vec<u8>, JsValue>;

    #[wasm_bindgen(js_name = restartRuntime)]
    pub(super) fn js_restart_runtime(reason: &str) -> u32;

    // DOM-only calls keep bespoke externs: they drive browser APIs (file
    // pickers, downloads, the File System Access API, tile fetch+decode) that
    // can't be expressed as a uniform request->response RPC.
    #[wasm_bindgen(js_name = pickCakeModel, catch)]
    pub(super) async fn js_pick_cake_model() -> Result<JsValue, JsValue>;

    #[cfg(feature = "gf")]
    #[wasm_bindgen(js_name = addGfStore, catch)]
    pub(super) async fn js_add_gf_store(job_id: u64) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = fetchTiles, catch)]
    pub(super) async fn js_fetch_tiles(
        urls: js_sys::Array,
        on_tile: js_sys::Function,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = reloadSnufflings, catch)]
    pub(super) async fn js_reload_snufflings(job_id: u64) -> Result<JsValue, JsValue>;

    // Picked and watched files, read page by page
    #[wasm_bindgen(js_name = openFiles, catch)]
    pub(super) async fn js_open_files() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = openDirectory, catch)]
    pub(super) async fn js_open_directory() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = reloadFiles, catch)]
    pub(super) async fn js_reload_files() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = readFileLoadPage, catch)]
    pub(super) async fn js_read_file_load_page(request: Vec<u8>) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = cancelFileLoad)]
    pub(super) fn js_cancel_file_load(request: Vec<u8>);

    #[wasm_bindgen(js_name = acknowledgeSources)]
    pub(super) fn js_acknowledge_sources(bytes: Vec<u8>);

    #[wasm_bindgen(js_name = clearOpenSources)]
    pub(super) fn js_clear_open_sources();

    // Metadata pickers
    #[wasm_bindgen(js_name = loadMarkers, catch)]
    pub(super) async fn js_load_markers() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadStationXml, catch)]
    pub(super) async fn js_load_station_xml() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadStations, catch)]
    pub(super) async fn js_load_stations() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = loadEvents, catch)]
    pub(super) async fn js_load_events() -> Result<JsValue, JsValue>;

    // Downloads
    #[wasm_bindgen(js_name = downloadFile, catch)]
    pub(super) async fn js_download_file(
        bytes: Vec<u8>,
        filename: String,
        mime: String,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = savePng, catch)]
    pub(super) async fn js_save_png(bytes: Vec<u8>, filename: String) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = saveSvg, catch)]
    pub(super) async fn js_save_svg(svg: String, filename: String) -> Result<JsValue, JsValue>;
}

/// Renders a thrown JS value as text, for an error that cannot hold a `JsValue`.
pub(super) fn js_value_to_string(value: JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}

/// Copies a `Uint8Array` out of the JS heap.
pub(super) fn uint8_array_bytes(
    value: JsValue,
    field: &'static str,
) -> Result<Vec<u8>, MarshalError> {
    value
        .dyn_into::<js_sys::Uint8Array>()
        .map_err(|_| MarshalError::NotBytes { field })
        .map(|array| array.to_vec())
}

/// Reads a `Uint8Array` field of a JS object.
pub(super) fn object_bytes_field(
    object: &JsValue,
    field: &'static str,
) -> Result<Vec<u8>, MarshalError> {
    let value = js_sys::Reflect::get(object, &JsValue::from_str(field))
        .map_err(|_| MarshalError::Missing { field })?;
    uint8_array_bytes(value, field)
}

/// Reads a `Uint8Array` field of a JS object as little-endian `f32` samples.
pub(super) fn object_f32_field(
    object: &JsValue,
    field: &'static str,
) -> Result<Vec<f32>, MarshalError> {
    let value = js_sys::Reflect::get(object, &JsValue::from_str(field))
        .map_err(|_| MarshalError::Missing { field })?;
    let bytes = value
        .dyn_into::<js_sys::Uint8Array>()
        .map_err(|_| MarshalError::NotBytes { field })?;
    if bytes.byte_length() % 4 != 0 {
        return Err(MarshalError::UnalignedSamples {
            field,
            len: bytes.byte_length(),
        });
    }
    let sample_count = bytes.byte_length() / 4;
    Ok(js_sys::Float32Array::new_with_byte_offset_and_length(
        &bytes.buffer(),
        bytes.byte_offset(),
        sample_count,
    )
    .to_vec())
}
