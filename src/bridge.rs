//! The bridge between the egui frontend and pyrocko.
//!
//! On the web, this calls into `web/bridge.js`, which drives Pyodide
//! (`CPython` + numpy + scipy + pyrocko, all compiled to WASM) and returns
//! waveform data. On native targets it falls back to a self-contained
//! synthetic stub so the UI can be developed with `cargo run` (no browser).
//!
//! The data contract below (serde structs) is mirrored exactly by the Python
//! shim in `web/bridge.py` — that mirroring is what makes the glue
//! "type-checked" on both sides of the boundary.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

/// Metadata for the whole loaded set of traces (snuffler's "pile").
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SceneMeta {
    pub tmin: f64,
    pub tmax: f64,
    pub tracks: Vec<TraceMeta>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization wire contract mirroring bridge.py; not every field is read by the renderer"
)]
pub struct TraceMeta {
    pub net: String,
    pub sta: String,
    pub loc: String,
    pub cha: String,
    pub label: String,
    pub tmin: f64,
    pub tmax: f64,
    pub deltat: f64,
    pub nsamples: usize,
}

/// What the UI asks for: a time window, a target pixel width, and processing.
/// Mirrors snuffler's processing flags so the bridge can reproduce its display.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ViewParams {
    pub tmin: f64,
    pub tmax: f64,
    pub npx: u32,
    pub highpass: Option<f64>,
    pub lowpass: Option<f64>,
    pub sort: String,
    pub subsort: String,
    /// Rotation of horizontal components, degrees.
    pub rotate: f64,
    /// Deconvolve instrument response (inert without response metadata).
    pub remove_response: bool,
    /// "wiggle" or "waterfall" (spectrogram).
    pub mode: String,
    /// Remove the per-trace mean before display (snuffler default on).
    pub demean: bool,
    /// Join traces across small gaps before processing.
    pub degap: bool,
    /// Allow decimation for fast drawing (off = send raw, capped).
    pub allow_downsampling: bool,
    /// Brick-wall FFT filtering instead of Butterworth.
    pub fft_filtering: bool,
    /// Apply a bandpass as separate low- + highpass passes.
    pub bandpass_lphp: bool,
    /// Use station depth in distance sorting (inert without coords).
    pub distances_3d: bool,
    /// Amplitude-range statistic: "minmax" | "robust" | "mean2" | "mean4".
    pub scaling_base: String,
    /// Amplitude grouping: "individual" | "common" | "`per_station`" |
    /// "`per_station_location`" | "`per_component`" (pyrocko derives the per-track
    /// display scale from this).
    pub scaling_group: String,
    /// Waterfall: rectify (abs) before the spectrogram.
    pub wf_abs: bool,
    /// Waterfall: integrate (÷ω) before the spectrogram.
    pub wf_integrate: bool,
    /// Waterfall colour mapping: "log" | "sqrt" | "lin".
    pub wf_color_scale: String,
    /// First track for which sample payloads are requested.
    pub track_offset: usize,
    /// Number of tracks for which sample payloads are requested.
    pub track_count: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SnufflingRequestParams {
    #[serde(flatten)]
    pub view: ViewParams,
    pub snufflings: SnufflingParams,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SnufflingParams {
    pub spectrum: SpectrumSnufflingParams,
    pub stalta: StaltaSnufflingParams,
    pub polarization: PolarizationSnufflingParams,
    pub rms: RmsSnufflingParams,
    pub cake: CakeSnufflingParams,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SpectrumSnufflingParams {
    pub spec_smoothing: bool,
    pub spec_smoothing_bandwidth: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror snuffler's STA/LTA parameter names in the bridge.py wire contract"
)]
pub struct StaltaSnufflingParams {
    pub stalta_highpass: Option<f64>,
    pub stalta_lowpass: Option<f64>,
    pub stalta_swin: f64,
    pub stalta_ratio: f64,
    pub stalta_level: f64,
    pub stalta_show_level_traces: bool,
    pub stalta_variant: String,
    pub stalta_scalingmethod: String,
    pub stalta_apply_to_sum: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror snuffler's polarization parameter names in the bridge.py wire contract"
)]
pub struct PolarizationSnufflingParams {
    pub polarization_t_length: f64,
    pub polarization_ft_offset: f64,
    pub polarization_azimuth: f64,
    pub polarization_highpass: Option<f64>,
    pub polarization_lowpass: Option<f64>,
    pub polarization_dot_position: f64,
    pub polarization_rotate_to_rt: bool,
    pub polarization_show_eigensystem_2d: bool,
    pub polarization_show_eigensystem_3d: bool,
    pub polarization_fix_scale: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror snuffler's RMS parameter names in the bridge.py wire contract"
)]
pub struct RmsSnufflingParams {
    pub rms_highpass: Option<f64>,
    pub rms_lowpass: Option<f64>,
    pub rms_block: f64,
    pub rms_group: bool,
    pub rms_log: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "the cake_* names are the literal JSON keys bridge.py reads; renaming desyncs the wire contract"
)]
pub struct CakeSnufflingParams {
    /// Dropdown model name; bridge.py maps it to `/earthmodels/<name>.nd`.
    pub cake_model: String,
    /// Enabled phase names; a leading `~` is kept (it selects a raw `PhaseDef`).
    pub cake_phases: Vec<String>,
    pub cake_tshift: f64,
    pub cake_use_station_depth: bool,
    /// `"run"` | `"plot_model"` | `"plot_rays"`; set per-action by the app.
    pub cake_mode: String,
}

/// What comes back: per-track polylines + amplitude stats. The actual values
/// live in a single flat `ys` array; each track points into it via
/// `offset`/`count`. For wiggles, `dt > 0` means raw uniform samples and
/// `dt == 0` means interleaved explicit `(t_rel, y)` points.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ViewMeta {
    pub view_tmin: f64,
    pub view_tmax: f64,
    pub tracks: Vec<ViewTrack>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization wire contract; some NSLC/scale fields are kept for completeness"
)]
pub struct ViewTrack {
    pub net: String,
    pub sta: String,
    pub loc: String,
    pub cha: String,
    pub label: String,
    /// Time of the first drawn sample, relative to `view_tmin` (seconds).
    pub t0_rel: f64,
    /// Time between raw samples (seconds), or zero for explicit points.
    pub dt: f64,
    pub count: usize,
    /// Index of this track's first value in the flat `ys` array.
    pub offset: usize,
    pub ymin: f32,
    pub ymax: f32,
    pub ymean: f32,
    /// Display half-range (units→pixels) computed by pyrocko per the Scaling
    /// group+base, so the egui side does no scaling math of its own.
    #[serde(default)]
    pub yscale: f32,
    /// Spectrogram dims (0 ⇒ this is a plain wiggle). When `nf > 0` the `ys`
    /// slice holds `nf*nt` log-power values, row-major as [time][freq].
    #[serde(default)]
    pub nf: usize,
    #[serde(default)]
    pub nt: usize,
    /// Max frequency of the spectrogram (Hz).
    #[serde(default)]
    pub fmax: f64,
}

#[derive(Clone, Debug, Default)]
pub struct ViewData {
    pub meta: ViewMeta,
    pub ys: Vec<f32>,
}

impl ViewData {
    /// Raw uniform samples or waterfall cells for one track as a flat slice.
    pub fn samples<'a>(&'a self, t: &ViewTrack) -> &'a [f32] {
        let end = (t.offset + t.count).min(self.ys.len());
        self.ys.get(t.offset.min(self.ys.len())..end).unwrap_or(&[])
    }
}

/// Amplitude-spectrum snuffling result.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SpecMeta {
    pub fmin: f64,
    pub fmax: f64,
    pub tracks: Vec<SpecTrack>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization wire contract for the amplitude-spectrum payload"
)]
pub struct SpecTrack {
    pub label: String,
    pub offset: usize,
    pub count: usize,
    pub ymin: f32,
    pub ymax: f32,
}

#[derive(Clone, Debug, Default)]
pub struct SpectrumData {
    pub meta: SpecMeta,
    pub ys: Vec<f32>,
}

/// A marker produced by a snuffling (e.g. an STA/LTA detection).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SnufMarker {
    pub t: f64,
    /// End time for span markers; `None` ⇒ a point marker at `t`.
    #[serde(default)]
    pub tmax: Option<f64>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub kind: u8,
    /// Drawn as an event triangle on the time axis (catalog events).
    #[serde(default)]
    pub is_event: bool,
    /// Channel scope (net, sta, loc, cha) for loaded markers.
    #[serde(default)]
    pub nslc: Option<(String, String, String, String)>,
    /// First-motion polarity of a phase pick (1/-1/0/None).
    #[serde(default)]
    pub polarity: Option<i8>,
}

/// Result of running a built-in snuffling: a text report and/or markers to add.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "deserialization wire contract mirroring bridge.py; the `scene` field is accepted but not yet consumed by the UI"
)]
pub struct SnufflingResult {
    #[serde(default = "default_show_window")]
    pub show_window: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub markers: Vec<SnufMarker>,
    #[serde(default)]
    pub scene: Option<SceneMeta>,
}

fn default_show_window() -> bool {
    true
}

/// A discontinuity in a cake model (e.g. moho, cmb, icb); depth in km.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeDiscontinuity {
    pub z_km: f32,
    #[serde(default)]
    pub name: Option<String>,
}

/// One layer of a cake model (a depth band in km) for background shading.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeLayer {
    pub ztop_km: f32,
    pub zbot_km: f32,
    pub water: bool,
    pub gradient: bool,
}

/// Velocity-depth model for the Cake Phase "Plot Model" window. `vp`/`vs` are
/// flat interleaved `(velocity_km_s, depth_km)` pairs.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeModelPlot {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub vp: Vec<f32>,
    #[serde(default)]
    pub vs: Vec<f32>,
    #[serde(default)]
    pub discontinuities: Vec<CakeDiscontinuity>,
    #[serde(default)]
    pub layers: Vec<CakeLayer>,
    pub vmax_km: f32,
    pub zmax_km: f32,
}

/// One traced ray for the "Plot Rays" window. Each entry of `segs` is a polyline
/// stored as a flat interleaved `(distance_km, depth_km)` array; `color` indexes
/// the marker palette (assigned per phase in Python so it matches the legend).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeRay {
    pub color: u8,
    #[serde(default)]
    pub segs: Vec<Vec<f32>>,
}

/// A point in the ray-paths cross-section (distance/depth in km).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakePoint {
    #[serde(default)]
    pub x_km: f32,
    pub z_km: f32,
}

/// A phase entry in the ray-paths legend (`color` matches its rays).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeLegendEntry {
    pub phase: String,
    pub color: u8,
}

/// Ray-path cross-section for the Cake Phase "Plot Rays" window (all km).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CakeRaysPlot {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rays: Vec<CakeRay>,
    #[serde(default)]
    pub discontinuities: Vec<CakeDiscontinuity>,
    #[serde(default)]
    pub layers: Vec<CakeLayer>,
    #[serde(default)]
    pub source: CakePoint,
    #[serde(default)]
    pub receivers: Vec<CakePoint>,
    #[serde(default)]
    pub legend: Vec<CakeLegendEntry>,
    pub xmin_km: f32,
    pub xmax_km: f32,
    pub zmax_km: f32,
}

/// Tagged result of a `cake_plot` call (the JSON carries a `kind` discriminator).
/// Only the wasm bridge parses it, so it is gated to that target.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CakePlotResult {
    Model(CakeModelPlot),
    Rays(CakeRaysPlot),
}

/// Outcome of a ':' command line. `action` is "goto" | "scene" | "error" | "none".
/// Only the wasm bridge constructs/consumes this, so it is gated to that target.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct CommandResult {
    pub action: String,
    #[serde(default)]
    pub t: Option<f64>,
    #[serde(default)]
    pub scene: Option<SceneMeta>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Station / event coordinates for the Map overlay (from the FDSN fetches).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct MapStation {
    pub label: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct MapEvent {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub mag: f64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct MapData {
    #[serde(default)]
    pub stations: Vec<MapStation>,
    #[serde(default)]
    pub events: Vec<MapEvent>,
}

/// Lifecycle of a map tile fetch (RGBA bytes are handed to the view once, which
/// turns them into a texture and caches it there).
#[allow(
    dead_code,
    reason = "tile-cache states are only constructed on the wasm (web) target"
)]
enum TileState {
    InFlight,
    Ready(u32, u32, Vec<u8>),
    Consumed,
    Failed,
}

#[derive(Clone, Debug, Default)]
#[allow(
    dead_code,
    reason = "Booting/Loading variants are only constructed on the wasm (web) target"
)]
pub enum Status {
    #[default]
    Booting,
    Loading(String),
    Ready,
    Error(String),
}

#[derive(Default)]
#[allow(
    dead_code,
    reason = "view-dispatch and tile-cache fields are only used on the wasm (web) target"
)]
struct Inner {
    status: Status,
    scene: Option<SceneMeta>,
    /// Incremented every time a new scene is loaded, so the UI can reset its
    /// view window when (and only when) the data actually changes.
    scene_gen: u64,
    view: Option<ViewData>,
    /// Incremented whenever new view data lands (for texture caching).
    view_epoch: u64,
    spectrum: Option<SpectrumData>,
    spectrum_in_flight: bool,
    snuffling: Option<SnufflingResult>,
    snuffling_gen: u64,
    cake_model_plot: Option<CakeModelPlot>,
    cake_rays_plot: Option<CakeRaysPlot>,
    cake_model_in_flight: bool,
    cake_rays_in_flight: bool,
    /// A custom model name the user just uploaded ("Add Model"), for the App to
    /// add to the Cake Phase dropdown and select.
    pending_cake_model: Option<String>,
    map: Option<MapData>,
    tiles: std::collections::HashMap<(u8, u32, u32), TileState>,
    view_in_flight: bool,
    desired: Option<ViewParams>,
    applied: Option<ViewParams>,
    /// A pending `goto <time>` target the App should jump the view to.
    pending_goto: Option<f64>,
    /// Feedback from the last ':' command (an error or confirmation) to show.
    command_msg: Option<String>,
}

#[derive(Clone)]
pub struct Bridge {
    inner: Rc<RefCell<Inner>>,
}

impl Bridge {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner::default())),
        }
    }

    pub fn status(&self) -> Status {
        self.inner.borrow().status.clone()
    }

    pub fn scene(&self) -> Option<SceneMeta> {
        self.inner.borrow().scene.clone()
    }

    /// Generation counter that increments on each new scene load.
    pub fn scene_gen(&self) -> u64 {
        self.inner.borrow().scene_gen
    }

    /// Take a pending `goto` time (set by a ':' command) for the App to apply.
    pub fn take_goto(&self) -> Option<f64> {
        self.inner.borrow_mut().pending_goto.take()
    }

    /// Take the last ':' command's feedback message (error / confirmation).
    pub fn take_command_msg(&self) -> Option<String> {
        self.inner.borrow_mut().command_msg.take()
    }

    /// Counter that increments whenever new view data is available.
    pub fn view_epoch(&self) -> u64 {
        self.inner.borrow().view_epoch
    }

    pub fn spectrum(&self) -> Option<SpectrumData> {
        self.inner.borrow().spectrum.clone()
    }

    /// Generation counter for snuffling results (so the UI applies each once).
    pub fn snuffling_gen(&self) -> u64 {
        self.inner.borrow().snuffling_gen
    }

    pub fn snuffling(&self) -> Option<SnufflingResult> {
        self.inner.borrow().snuffling.clone()
    }

    /// Latest Cake Phase "Plot Model" data (for the model-velocity window).
    pub fn cake_model_plot(&self) -> Option<CakeModelPlot> {
        self.inner.borrow().cake_model_plot.clone()
    }

    /// Latest Cake Phase "Plot Rays" data (for the ray-paths window).
    pub fn cake_rays_plot(&self) -> Option<CakeRaysPlot> {
        self.inner.borrow().cake_rays_plot.clone()
    }

    /// Take a custom model name added via "Add Model" (for the App to register).
    pub fn take_pending_cake_model(&self) -> Option<String> {
        self.inner.borrow_mut().pending_cake_model.take()
    }

    pub fn map_data(&self) -> Option<MapData> {
        self.inner.borrow().map.clone()
    }

    /// Hand the view a ready tile's RGBA exactly once (it caches the texture).
    pub fn take_tile_rgba(&self, z: u8, x: u32, y: u32) -> Option<(u32, u32, Vec<u8>)> {
        let mut i = self.inner.borrow_mut();
        if matches!(i.tiles.get(&(z, x, y)), Some(TileState::Ready(..)))
            && let Some(TileState::Ready(w, h, rgba)) =
                i.tiles.insert((z, x, y), TileState::Consumed)
        {
            return Some((w, h, rgba));
        }
        None
    }

    /// Borrow the current view (decimated waveforms) for drawing.
    pub fn view(&self) -> Option<Ref<'_, ViewData>> {
        let b = self.inner.borrow();
        Ref::filter_map(b, |i| i.view.as_ref()).ok()
    }
}

// Web target: drive Pyodide through web/bridge.js.
#[cfg(target_arch = "wasm32")]
mod web {
    #![allow(
        clippy::wildcard_imports,
        clippy::needless_pass_by_value,
        reason = "the web bridge re-uses most parent types and marshals owned values \
                  (JsValue, JSON strings, params) across the JS/async boundary, where \
                  by-value parameters keep the call sites clean"
    )]

    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::spawn_local;

    #[wasm_bindgen(module = "/web/bridge.js")]
    extern "C" {
        #[wasm_bindgen(js_name = makeExample, catch)]
        async fn js_make_example() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadBytes, catch)]
        async fn js_load_bytes(
            name: String,
            data: Vec<u8>,
            fmt: String,
        ) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadUrl, catch)]
        async fn js_load_url(url: String, fmt: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = getView, catch)]
        async fn js_get_view(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = getSpectrum, catch)]
        async fn js_get_spectrum(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = runSnuffling, catch)]
        async fn js_run_snuffling(name: String, params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = downloadWaveforms, catch)]
        async fn js_download_waveforms(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = catalogSearch, catch)]
        async fn js_catalog_search(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = fetchStations, catch)]
        async fn js_fetch_stations(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = fetchResponses, catch)]
        async fn js_fetch_responses(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = mapData, catch)]
        async fn js_map_data() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = cakePlot, catch)]
        async fn js_cake_plot(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = addCakeModel, catch)]
        async fn js_add_cake_model() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = runCommand, catch)]
        async fn js_run_command(line: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = rmsTraces, catch)]
        async fn js_rms_traces(params_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = fetchTile, catch)]
        async fn js_fetch_tile(url: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = openFiles, catch)]
        async fn js_open_files() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = saveMarkers, catch)]
        async fn js_save_markers(markers_json: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadMarkers, catch)]
        async fn js_load_markers() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = savePng)]
        fn js_save_png(bytes: &[u8], filename: &str);

        #[wasm_bindgen(js_name = saveSvg)]
        fn js_save_svg(svg: &str, filename: &str);
    }

    fn jsval_to_string(v: JsValue) -> String {
        v.as_string().unwrap_or_else(|| format!("{v:?}"))
    }

    fn release_js_buffer_result(obj: &JsValue) {
        use wasm_bindgen::JsCast;

        if let Ok(release) = js_sys::Reflect::get(obj, &JsValue::from_str("release"))
            && let Some(f) = release.dyn_ref::<js_sys::Function>()
        {
            let _ = f.call0(obj);
        }
    }

    fn parse_scene(v: JsValue) -> Result<SceneMeta, String> {
        let s = v
            .as_string()
            .ok_or_else(|| "scene not a string".to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    }

    fn parse_view(obj: JsValue) -> Result<ViewData, String> {
        let parsed = (|| {
            use wasm_bindgen::JsCast;

            let meta_val =
                js_sys::Reflect::get(&obj, &JsValue::from_str("meta")).map_err(jsval_to_string)?;
            let meta_str = meta_val
                .as_string()
                .ok_or_else(|| "view.meta not a string".to_string())?;
            let meta: ViewMeta = serde_json::from_str(&meta_str).map_err(|e| e.to_string())?;
            let ys_val =
                js_sys::Reflect::get(&obj, &JsValue::from_str("ys")).map_err(jsval_to_string)?;
            let ys_arr: js_sys::Float32Array = ys_val
                .dyn_into()
                .map_err(|_| "view.ys is not a Float32Array".to_string())?;
            Ok(ViewData {
                meta,
                ys: ys_arr.to_vec(),
            })
        })();
        release_js_buffer_result(&obj);
        parsed
    }

    fn parse_spectrum(obj: JsValue) -> Result<SpectrumData, String> {
        let parsed = (|| {
            use wasm_bindgen::JsCast;

            let meta_val =
                js_sys::Reflect::get(&obj, &JsValue::from_str("meta")).map_err(jsval_to_string)?;
            let meta_str = meta_val
                .as_string()
                .ok_or_else(|| "spectrum.meta not a string".to_string())?;
            let meta: SpecMeta = serde_json::from_str(&meta_str).map_err(|e| e.to_string())?;
            let ys_val =
                js_sys::Reflect::get(&obj, &JsValue::from_str("ys")).map_err(jsval_to_string)?;
            let ys_arr: js_sys::Float32Array = ys_val
                .dyn_into()
                .map_err(|_| "spectrum.ys is not a Float32Array".to_string())?;
            Ok(SpectrumData {
                meta,
                ys: ys_arr.to_vec(),
            })
        })();
        release_js_buffer_result(&obj);
        parsed
    }

    fn view_track_values(view: &ViewData, track: &ViewTrack) -> (&'static str, Vec<f32>) {
        if track.nf > 0 {
            return ("waterfall", Vec::new());
        }
        if track.dt > 0.0 {
            return ("raw", view.samples(track).to_vec());
        }

        let stride = if track.dt == 0.0 { 2 } else { 3 };
        let start = track.offset.min(view.ys.len());
        let end = (track.offset + track.count.saturating_mul(stride)).min(view.ys.len());
        let Some(payload) = view.ys.get(start..end) else {
            return ("missing", Vec::new());
        };
        if track.dt == 0.0 {
            return (
                "points",
                payload
                    .chunks_exact(2)
                    .filter_map(|pair| match pair {
                        [_, y] => Some(*y),
                        _ => None,
                    })
                    .collect(),
            );
        }
        let mut values = Vec::with_capacity(track.count.saturating_mul(2));
        for triple in payload.chunks_exact(3) {
            let [_, ylo, yhi] = triple else {
                continue;
            };
            values.push(*ylo);
            values.push(*yhi);
        }
        ("legacy-envelope", values)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "sample count to f64 for diagnostic mean/rms; precision loss is irrelevant"
    )]
    fn finite_stats(values: &[f32]) -> Option<(f32, f32, f64, f64)> {
        let mut n = 0usize;
        let mut min_v = f32::INFINITY;
        let mut max_v = f32::NEG_INFINITY;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        for &v in values.iter().filter(|v| v.is_finite()) {
            n += 1;
            min_v = min_v.min(v);
            max_v = max_v.max(v);
            let fv = f64::from(v);
            sum += fv;
            sum_sq += fv * fv;
        }
        if n == 0 {
            return None;
        }
        Some((min_v, max_v, sum / n as f64, (sum_sq / n as f64).sqrt()))
    }

    fn sample_head(values: &[f32]) -> String {
        values
            .iter()
            .take(8)
            .map(|v| format!("{v:.5}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "sample/pixel counts to f64 for a diagnostic ratio; precision loss is irrelevant"
    )]
    fn log_view_debug(params: &ViewParams, view: &ViewData) {
        if params.mode != "wiggle" {
            return;
        }

        let rendered_tracks: Vec<_> = view
            .meta
            .tracks
            .iter()
            .skip(params.track_offset)
            .take(params.track_count)
            .collect();
        let raw_tracks = rendered_tracks
            .iter()
            .filter(|track| track.nf == 0 && track.count > 0 && track.dt > 0.0)
            .count();
        let point_tracks = rendered_tracks
            .iter()
            .filter(|track| track.nf == 0 && track.count > 0 && track.dt == 0.0)
            .count();
        let legacy_tracks = rendered_tracks
            .iter()
            .filter(|track| track.nf == 0 && track.count > 0 && track.dt < 0.0)
            .count();
        let window = params.tmax - params.tmin;
        log::info!(
            "[view] t=[{:.6},{:.6}] dt_window={:.3}s npx={} track_page={}..{} returned_tracks={} ys={} rendered_modes raw={} points={} legacy={}",
            params.tmin,
            params.tmax,
            window,
            params.npx,
            params.track_offset,
            params.track_offset + params.track_count,
            view.meta.tracks.len(),
            view.ys.len(),
            raw_tracks,
            point_tracks,
            legacy_tracks,
        );

        for (idx, track) in view
            .meta
            .tracks
            .iter()
            .enumerate()
            .skip(params.track_offset)
            .take(params.track_count)
        {
            let (mode, values) = view_track_values(view, track);
            let points_per_px = if params.npx == 0 {
                0.0
            } else {
                values.len() as f64 / f64::from(params.npx)
            };
            if let Some((min_v, max_v, mean, rms)) = finite_stats(&values) {
                log::debug!(
                    "[view.track] idx={} label={} mode={} count={} points_per_px={:.3} data_min={:.6} data_max={:.6} data_mean={:.6} data_rms={:.6} scale_min={:.6} scale_max={:.6} scale_mean={:.6} yscale={:.6} t0_rel={:.6} dt={:.9} head=[{}]",
                    idx,
                    track.label,
                    mode,
                    values.len(),
                    points_per_px,
                    min_v,
                    max_v,
                    mean,
                    rms,
                    track.ymin,
                    track.ymax,
                    track.ymean,
                    track.yscale,
                    track.t0_rel,
                    track.dt,
                    sample_head(&values),
                );
            } else {
                log::debug!(
                    "[view.track] idx={} label={} mode={} count=0 scale_min={:.6} scale_max={:.6} scale_mean={:.6} yscale={:.6} t0_rel={:.6} dt={:.9}",
                    idx,
                    track.label,
                    mode,
                    track.ymin,
                    track.ymax,
                    track.ymean,
                    track.yscale,
                    track.t0_rel,
                    track.dt,
                );
            }
        }
    }

    impl Bridge {
        pub fn load_example(&self, ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Loading("Loading example…".into());
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_make_example().await;
                let parsed = match res {
                    Ok(v) => parse_scene(v),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            i.view = None;
                            i.applied = None;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                ctx.request_repaint();
            });
        }

        pub fn load_file(&self, ctx: &egui::Context, name: String, data: Vec<u8>, fmt: String) {
            self.inner.borrow_mut().status = Status::Loading(format!("Loading {name}…"));
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_load_bytes(name, data, fmt).await;
                let parsed = match res {
                    Ok(v) => parse_scene(v),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            i.view = None;
                            i.applied = None;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                ctx.request_repaint();
            });
        }

        pub fn load_url(&self, ctx: &egui::Context, url: String, fmt: String) {
            self.inner.borrow_mut().status = Status::Loading(format!("Loading {url}…"));
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_load_url(url, fmt).await;
                let parsed = match res {
                    Ok(v) => parse_scene(v),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            i.view = None;
                            i.applied = None;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Fetch an amplitude spectrum for the current params (snuffling).
        pub fn fetch_spectrum(&self, ctx: &egui::Context, params: SnufflingRequestParams) {
            {
                let mut i = self.inner.borrow_mut();
                if i.spectrum_in_flight || !matches!(i.status, Status::Ready) {
                    return;
                }
                i.spectrum_in_flight = true;
            }
            let json = serde_json::to_string(&params).unwrap_or_default();
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_get_spectrum(json).await;
                let parsed = match res {
                    Ok(o) => parse_spectrum(o),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Ok(sd) = parsed {
                        i.spectrum = Some(sd);
                    }
                    i.spectrum_in_flight = false;
                }
                ctx.request_repaint();
            });
        }

        /// Run a built-in snuffling (`MinMax`, `STA/LTA`, …) and store its result.
        pub fn run_snuffling(
            &self,
            ctx: &egui::Context,
            name: String,
            params: SnufflingRequestParams,
        ) {
            if !matches!(self.inner.borrow().status, Status::Ready) {
                return;
            }
            let json = serde_json::to_string(&params).unwrap_or_default();
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_run_snuffling(name, json).await;
                let parsed = match res {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<SnufflingResult>(&s).ok()),
                    Err(_) => None,
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Some(mut r) = parsed {
                        if let Some(scene) = r.scene.take() {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.view = None;
                            i.applied = None;
                            i.status = Status::Ready;
                        }
                        i.snuffling = Some(r);
                        i.snuffling_gen += 1;
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Download waveforms from a CORS-enabled FDSN dataselect service and
        /// replace the pile with the result (browser fetch; pyrocko parses).
        pub fn download_waveforms(&self, ctx: &egui::Context, params_json: String) {
            self.inner.borrow_mut().status = Status::Loading("Downloading waveforms…".into());
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_download_waveforms(params_json).await {
                    Ok(v) => parse_scene(v),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            i.view = None;
                            i.applied = None;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Compute blockwise-RMS derived traces (pyrocko's RMS snuffling) and
        /// append them to the pile; the result replaces the scene.
        pub fn rms_traces(&self, ctx: &egui::Context, params: SnufflingRequestParams) {
            let json = serde_json::to_string(&params).unwrap_or_default();
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_rms_traces(json).await {
                    Ok(v) => parse_scene(v),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            i.view = None;
                            i.applied = None;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Search a CORS-enabled FDSN event service; results become event markers.
        pub fn catalog_search(&self, ctx: &egui::Context, params_json: String) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_catalog_search(params_json).await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<SnufflingResult>(&s).ok()),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: jsval_to_string(e),
                        markers: Vec::new(),
                        scene: None,
                    }),
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Some(r) = parsed {
                        i.snuffling = Some(r);
                        i.snuffling_gen += 1;
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Fetch Cake Phase plot data (pyrocko.cake) for the current params. The
        /// `cake_mode` (`plot_model` | `plot_rays`) selects which window's data
        /// is computed and stored; the App draws it (no matplotlib).
        pub fn cake_plot(&self, ctx: &egui::Context, params: SnufflingRequestParams) {
            let rays = params.snufflings.cake.cake_mode == "plot_rays";
            {
                let mut i = self.inner.borrow_mut();
                let busy = if rays {
                    i.cake_rays_in_flight
                } else {
                    i.cake_model_in_flight
                };
                if busy || !matches!(i.status, Status::Ready) {
                    return;
                }
                if rays {
                    i.cake_rays_in_flight = true;
                } else {
                    i.cake_model_in_flight = true;
                }
            }
            let json = serde_json::to_string(&params).unwrap_or_default();
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_cake_plot(json).await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<CakePlotResult>(&s).ok()),
                    Err(_) => None,
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Some(CakePlotResult::Model(m)) => i.cake_model_plot = Some(m),
                        Some(CakePlotResult::Rays(r)) => i.cake_rays_plot = Some(r),
                        None => {}
                    }
                    if rays {
                        i.cake_rays_in_flight = false;
                    } else {
                        i.cake_model_in_flight = false;
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Let the user pick a custom `.nd` model file ("Add Model"); the picked
        /// name is stored for the App to add to the Cake Phase dropdown.
        pub fn add_cake_model(&self, ctx: &egui::Context) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                if let Ok(v) = js_add_cake_model().await
                    && let Some(name) = v.as_string()
                    && !name.is_empty()
                {
                    inner.borrow_mut().pending_cake_model = Some(name);
                }
                ctx.request_repaint();
            });
        }

        /// Execute a ':' command line (goto / hide / unhide / reduce). A goto
        /// stores a pending time the App jumps to; a visibility change swaps the
        /// scene in place (no refit) and forces the view to re-dispatch.
        pub fn run_command(&self, ctx: &egui::Context, line: String) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = match js_run_command(line).await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<CommandResult>(&s).ok()),
                    Err(e) => Some(CommandResult {
                        action: "error".into(),
                        message: Some(jsval_to_string(e)),
                        ..Default::default()
                    }),
                };
                if let Some(r) = res {
                    let mut i = inner.borrow_mut();
                    match r.action.as_str() {
                        "goto" => i.pending_goto = r.t,
                        "scene" => {
                            if let Some(scene) = r.scene {
                                i.scene = Some(scene);
                                i.applied = None; // re-dispatch without refitting time
                            }
                        }
                        "error" => i.command_msg = r.message,
                        _ => {}
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Fetch station coordinates (FDSN station service) to enable distance /
        /// azimuth sorting; forces the view to re-dispatch so the sort recomputes.
        pub fn fetch_stations(&self, ctx: &egui::Context, params_json: String) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_fetch_stations(params_json).await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<SnufflingResult>(&s).ok()),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: jsval_to_string(e),
                        markers: Vec::new(),
                        scene: None,
                    }),
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Some(r) = parsed {
                        i.snuffling = Some(r);
                        i.snuffling_gen += 1;
                    }
                    i.applied = None; // re-dispatch so distance/azimuth sort recomputes
                }
                ctx.request_repaint();
            });
        }

        /// Fetch instrument responses (`StationXML`) so Deconvolution can run;
        /// forces a re-dispatch so the deconvolved view recomputes.
        pub fn fetch_responses(&self, ctx: &egui::Context, params_json: String) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_fetch_responses(params_json).await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<SnufflingResult>(&s).ok()),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: jsval_to_string(e),
                        markers: Vec::new(),
                        scene: None,
                    }),
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Some(r) = parsed {
                        i.snuffling = Some(r);
                        i.snuffling_gen += 1;
                    }
                    i.applied = None; // re-dispatch so deconvolution applies
                }
                ctx.request_repaint();
            });
        }

        /// Open local waveform files via a browser picker and append them.
        pub fn open_files(&self, ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Loading("Opening files…".into());
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let res = js_open_files().await;
                {
                    let mut i = inner.borrow_mut();
                    match res {
                        Ok(v) => match v.as_string() {
                            Some(s) if s.is_empty() => i.status = Status::Ready, // cancelled
                            Some(s) => match serde_json::from_str::<SceneMeta>(&s) {
                                Ok(scene) => {
                                    i.scene = Some(scene);
                                    i.scene_gen += 1;
                                    i.status = Status::Ready;
                                    i.view = None;
                                    i.applied = None;
                                }
                                Err(e) => i.status = Status::Error(e.to_string()),
                            },
                            None => i.status = Status::Error("open_files: bad result".into()),
                        },
                        Err(e) => i.status = Status::Error(jsval_to_string(e)),
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Serialize markers to pyrocko marker format and download them.
        #[allow(
            clippy::unused_self,
            reason = "kept as a method for API symmetry with the other bridge calls; the fire-and-forget download needs no state"
        )]
        pub fn save_markers(&self, _ctx: &egui::Context, markers_json: String) {
            spawn_local(async move {
                let _ = js_save_markers(markers_json).await;
            });
        }

        /// Trigger a browser download of encoded image bytes (PNG export).
        #[allow(
            clippy::unused_self,
            reason = "method form mirrors the other bridge calls; the download needs no state"
        )]
        pub fn save_image_png(&self, bytes: Vec<u8>, filename: String) {
            js_save_png(&bytes, &filename);
        }

        /// Trigger a browser download of an SVG document (vector image export).
        #[allow(
            clippy::unused_self,
            reason = "method form mirrors the other bridge calls; the download needs no state"
        )]
        pub fn save_image_svg(&self, svg: String, filename: String) {
            js_save_svg(&svg, &filename);
        }

        /// Load a marker file via a browser picker; results land as markers.
        pub fn load_markers(&self, ctx: &egui::Context) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let markers = match js_load_markers().await {
                    Ok(v) => v
                        .as_string()
                        .filter(|s| !s.is_empty())
                        .and_then(|s| serde_json::from_str::<Vec<SnufMarker>>(&s).ok()),
                    Err(_) => None,
                };
                if let Some(markers) = markers {
                    let mut i = inner.borrow_mut();
                    let n = markers.len();
                    i.snuffling = Some(SnufflingResult {
                        show_window: true,
                        text: format!("loaded {n} marker(s)"),
                        markers,
                        scene: None,
                    });
                    i.snuffling_gen += 1;
                }
                ctx.request_repaint();
            });
        }

        /// Fetch station + event coordinates for the Map overlay.
        pub fn fetch_map_data(&self, ctx: &egui::Context) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_map_data().await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<MapData>(&s).ok()),
                    Err(_) => None,
                };
                {
                    let mut i = inner.borrow_mut();
                    if let Some(m) = parsed {
                        i.map = Some(m);
                    }
                }
                ctx.request_repaint();
            });
        }

        /// Fetch one map tile (browser fetch + decode); idempotent per (z,x,y).
        pub fn request_tile(&self, ctx: &egui::Context, z: u8, x: u32, y: u32, url: String) {
            {
                let mut i = self.inner.borrow_mut();
                if i.tiles.contains_key(&(z, x, y)) {
                    return;
                }
                i.tiles.insert((z, x, y), TileState::InFlight);
            }
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                use wasm_bindgen::JsCast;
                let state = match js_fetch_tile(url).await {
                    Ok(obj) => {
                        let num = |key: &str| {
                            js_sys::Reflect::get(&obj, &JsValue::from_str(key))
                                .ok()
                                .and_then(|val| val.as_f64())
                                .unwrap_or(0.0)
                        };
                        // Tile dimensions are small non-negative pixel counts.
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "tile width/height are small non-negative integers"
                        )]
                        let (width, height) = (num("w") as u32, num("h") as u32);
                        let rgba = js_sys::Reflect::get(&obj, &JsValue::from_str("rgba"))
                            .ok()
                            .and_then(|val| val.dyn_into::<js_sys::Uint8Array>().ok())
                            .map(|arr| arr.to_vec());
                        match rgba {
                            Some(bytes)
                                if width > 0
                                    && height > 0
                                    && bytes.len() == (width * height * 4) as usize =>
                            {
                                TileState::Ready(width, height, bytes)
                            }
                            _ => TileState::Failed,
                        }
                    }
                    Err(_) => TileState::Failed,
                };
                inner.borrow_mut().tiles.insert((z, x, y), state);
                ctx.request_repaint();
            });
        }

        /// Request a view for the given params, coalescing rapid changes so that
        /// at most one Python call is in flight at a time.
        pub fn ensure_view(&self, ctx: &egui::Context, params: ViewParams) {
            {
                let mut i = self.inner.borrow_mut();
                if !matches!(i.status, Status::Ready) {
                    return;
                }
                i.desired = Some(params);
                if i.view_in_flight || i.applied.as_ref() == i.desired.as_ref() {
                    return;
                }
                i.view_in_flight = true;
            }
            self.dispatch_view(ctx);
        }

        fn dispatch_view(&self, ctx: &egui::Context) {
            let Some(params) = self.inner.borrow().desired.clone() else {
                return;
            };
            let json = serde_json::to_string(&params).unwrap_or_default();
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            let me = self.clone();
            spawn_local(async move {
                let res = js_get_view(json).await;
                let parsed = match res {
                    Ok(obj) => parse_view(obj),
                    Err(e) => Err(jsval_to_string(e)),
                };
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(vd) => {
                            log_view_debug(&params, &vd);
                            i.view = Some(vd);
                            i.view_epoch += 1;
                            i.applied = Some(params);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                    i.view_in_flight = false;
                }
                ctx.request_repaint();
                me.maybe_redispatch(&ctx);
            });
        }

        fn maybe_redispatch(&self, ctx: &egui::Context) {
            let need = {
                let mut i = self.inner.borrow_mut();
                if !i.view_in_flight
                    && i.desired.is_some()
                    && i.applied.as_ref() != i.desired.as_ref()
                {
                    i.view_in_flight = true;
                    true
                } else {
                    false
                }
            };
            if need {
                self.dispatch_view(ctx);
            }
        }
    }
}

// Native target: a UI-only shell. ALL data loading and signal processing happen
// in pyrocko (Python) on the web build. The native build (`cargo run`) exists
// only to lay out the egui UI and deliberately does NO computation of its own,
// so there is no second implementation of pyrocko's math to drift from.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    #![allow(
        clippy::unused_self,
        reason = "these no-op stubs mirror the wasm Bridge API so the native UI shell compiles"
    )]

    use super::{Bridge, SnufflingRequestParams, Status, ViewParams};

    const MSG: &str = "Native build is UI-only — data loading, filtering, and all \
         processing run in pyrocko (Python) in the browser. Use `mise run serve`.";

    impl Bridge {
        pub fn load_example(&self, _ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn load_file(&self, _ctx: &egui::Context, _name: String, _data: Vec<u8>, _fmt: String) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn load_url(&self, _ctx: &egui::Context, _url: String, _fmt: String) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn fetch_spectrum(&self, _ctx: &egui::Context, _params: SnufflingRequestParams) {}

        pub fn run_snuffling(
            &self,
            _ctx: &egui::Context,
            _name: String,
            _params: SnufflingRequestParams,
        ) {
        }

        pub fn download_waveforms(&self, _ctx: &egui::Context, _params_json: String) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn catalog_search(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn cake_plot(&self, _ctx: &egui::Context, _params: SnufflingRequestParams) {}

        pub fn add_cake_model(&self, _ctx: &egui::Context) {}

        pub fn run_command(&self, _ctx: &egui::Context, _line: String) {}

        pub fn rms_traces(&self, _ctx: &egui::Context, _params: SnufflingRequestParams) {}

        pub fn fetch_stations(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn fetch_responses(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn fetch_map_data(&self, _ctx: &egui::Context) {}

        pub fn request_tile(&self, _ctx: &egui::Context, _z: u8, _x: u32, _y: u32, _url: String) {}

        pub fn open_files(&self, _ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn save_markers(&self, _ctx: &egui::Context, _markers_json: String) {}

        pub fn save_image_png(&self, _bytes: Vec<u8>, _filename: String) {}

        pub fn save_image_svg(&self, _svg: String, _filename: String) {}

        pub fn load_markers(&self, _ctx: &egui::Context) {}

        pub fn ensure_view(&self, _ctx: &egui::Context, _params: ViewParams) {}
    }
}
