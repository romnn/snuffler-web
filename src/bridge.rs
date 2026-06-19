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
}

/// What comes back: per-track decimated polylines + amplitude stats. The actual
/// sample values live in a single flat `ys` array; each track points into it
/// via `offset`/`count`, with `t0_rel`/`dt` giving sample times.
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
    /// Time between drawn samples (seconds).
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
    /// Samples for one track as a slice of the flat array.
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
pub struct SnufflingResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub markers: Vec<SnufMarker>,
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

        #[wasm_bindgen(js_name = cakeArrivals, catch)]
        async fn js_cake_arrivals() -> Result<JsValue, JsValue>;

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
    }

    fn jsval_to_string(v: JsValue) -> String {
        v.as_string().unwrap_or_else(|| format!("{v:?}"))
    }

    fn parse_scene(v: JsValue) -> Result<SceneMeta, String> {
        let s = v
            .as_string()
            .ok_or_else(|| "scene not a string".to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    }

    fn parse_view(obj: JsValue) -> Result<ViewData, String> {
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
    }

    fn parse_spectrum(obj: JsValue) -> Result<SpectrumData, String> {
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
        pub fn fetch_spectrum(&self, ctx: &egui::Context, params: ViewParams) {
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
        pub fn run_snuffling(&self, ctx: &egui::Context, name: String, params: ViewParams) {
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
                    if let Some(r) = parsed {
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
        pub fn rms_traces(&self, ctx: &egui::Context, params: ViewParams) {
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
                        text: jsval_to_string(e),
                        markers: Vec::new(),
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

        /// Compute theoretical P/S arrivals (pyrocko.cake) for the active event
        /// at the loaded stations and add them as phase markers.
        pub fn cake_arrivals(&self, ctx: &egui::Context) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match js_cake_arrivals().await {
                    Ok(v) => v
                        .as_string()
                        .and_then(|s| serde_json::from_str::<SnufflingResult>(&s).ok()),
                    Err(e) => Some(SnufflingResult {
                        text: jsval_to_string(e),
                        markers: Vec::new(),
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
                        text: jsval_to_string(e),
                        markers: Vec::new(),
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
                        text: jsval_to_string(e),
                        markers: Vec::new(),
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
                        text: format!("loaded {n} marker(s)"),
                        markers,
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

    use super::{Bridge, Status, ViewParams};

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

        pub fn fetch_spectrum(&self, _ctx: &egui::Context, _params: ViewParams) {}

        pub fn run_snuffling(&self, _ctx: &egui::Context, _name: String, _params: ViewParams) {}

        pub fn download_waveforms(&self, _ctx: &egui::Context, _params_json: String) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn catalog_search(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn cake_arrivals(&self, _ctx: &egui::Context) {}

        pub fn run_command(&self, _ctx: &egui::Context, _line: String) {}

        pub fn rms_traces(&self, _ctx: &egui::Context, _params: ViewParams) {}

        pub fn fetch_stations(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn fetch_responses(&self, _ctx: &egui::Context, _params_json: String) {}

        pub fn fetch_map_data(&self, _ctx: &egui::Context) {}

        pub fn request_tile(&self, _ctx: &egui::Context, _z: u8, _x: u32, _y: u32, _url: String) {}

        pub fn open_files(&self, _ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn save_markers(&self, _ctx: &egui::Context, _markers_json: String) {}

        pub fn load_markers(&self, _ctx: &egui::Context) {}

        pub fn ensure_view(&self, _ctx: &egui::Context, _params: ViewParams) {}
    }
}
