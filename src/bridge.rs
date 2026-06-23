//! The bridge between the egui frontend and pyrocko.
//!
//! On the web, this calls into `web/bridge.js`, which forwards protobuf request
//! bytes to a Pyodide Web Worker (`CPython` + numpy + scipy + pyrocko, all
//! compiled to WASM) and returns protobuf metadata plus raw sample buffers. On
//! native targets it falls back to a UI-only stub so layouts can be developed
//! with `cargo run` (no browser/Pyodide).
//!
//! `proto/snuffler/bridge/v1/*.proto` is the boundary contract.
//! The Rust structs below are the app-facing decoded representation.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

mod proto;

// The decoded protobuf messages ARE the app-facing domain model — no parallel
// hand-written structs, no conversion layer. prost generates these from
// proto/snuffler/bridge/v1/*.proto; the bridge passes them straight to the UI.
pub use proto::pb::{
    CakeDiscontinuity, CakeLayer, CakeLegendEntry, CakeModelPlot, CakePoint, CakeRaysPlot,
    CakeSnufflingParams, CatalogRequest, MapData, MarkerFile, MarkerFileEntry, Nslc,
    PolarizationSnufflingParams, RmsSnufflingParams, SceneMeta, SnufflingParams,
    SnufflingRequestParams, SnufflingResult, SpecMeta, SpectrumSnufflingParams,
    StaltaSnufflingParams, StationRequest, ViewMeta, ViewParams, ViewTrack,
    WaveformDownloadRequest,
};

/// The deeply-nested cake mode in the prost request type, read ergonomically.
/// Only the wasm bridge inspects the mode (to route the cake plot request).
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn cake_mode(p: &SnufflingRequestParams) -> &str {
    p.snufflings
        .as_ref()
        .and_then(|s| s.cake.as_ref())
        .map_or("", |c| c.cake_mode.as_str())
}

/// Set the deeply-nested cake mode in the prost request type (no-op if the
/// snufflings/cake sub-messages are absent, which the UI builder never leaves).
pub fn set_cake_mode(p: &mut SnufflingRequestParams, mode: &str) {
    if let Some(c) = p.snufflings.as_mut().and_then(|s| s.cake.as_mut()) {
        mode.clone_into(&mut c.cake_mode);
    }
}

/// A view: prost metadata plus the out-of-band zero-copy `f32` sample buffer
/// (`ys` rides alongside the proto, never inside it — see the worker transfer).
#[derive(Clone, Debug, Default)]
pub struct ViewData {
    pub meta: ViewMeta,
    pub ys: Vec<f32>,
}

impl ViewData {
    /// Raw uniform samples or waterfall cells for one track as a flat slice.
    pub fn samples<'a>(&'a self, t: &ViewTrack) -> &'a [f32] {
        let off = usize::try_from(t.offset).unwrap_or(usize::MAX);
        let cnt = usize::try_from(t.count).unwrap_or(usize::MAX);
        let end = off.saturating_add(cnt).min(self.ys.len());
        self.ys.get(off.min(self.ys.len())..end).unwrap_or(&[])
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpectrumData {
    pub meta: SpecMeta,
    pub ys: Vec<f32>,
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
                  (JsValue, protobuf bytes, params) across the JS/async boundary, where \
                  by-value parameters keep the call sites clean"
    )]

    use super::proto::{self, pb};
    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::spawn_local;

    #[wasm_bindgen(module = "/web/gen/bridge.js")]
    extern "C" {
        // The single transport seam for every unary proto RPC: (method, request
        // bytes, optional out-of-band data bytes) -> { result: BridgeResult bytes,
        // ys: Uint8Array }. bridge.js forwards it to the Pyodide worker, which runs
        // bridge.py and ships the zero-copy ys buffer alongside the proto. This is
        // the custom transport the typed service client rides on.
        #[wasm_bindgen(js_name = call, catch)]
        async fn js_call(method: &str, request: Vec<u8>, data: Vec<u8>)
        -> Result<JsValue, JsValue>;

        // DOM-only calls keep bespoke externs: they drive browser APIs (file
        // pickers, downloads, the File System Access API, tile fetch+decode) that
        // can't be expressed as a uniform request->response RPC.
        #[wasm_bindgen(js_name = addCakeModel, catch)]
        async fn js_add_cake_model() -> Result<JsValue, JsValue>;

        #[cfg(feature = "gf")]
        #[wasm_bindgen(js_name = addGfStore, catch)]
        async fn js_add_gf_store() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = fetchTile, catch)]
        async fn js_fetch_tile(url: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = openFiles, catch)]
        async fn js_open_files() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = saveMarkers, catch)]
        async fn js_save_markers(request: Vec<u8>) -> Result<JsValue, JsValue>;

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

    fn js_bytes(v: JsValue, what: &str) -> Result<Vec<u8>, String> {
        use wasm_bindgen::JsCast;

        v.dyn_into::<js_sys::Uint8Array>()
            .map_err(|_| format!("{what} is not a Uint8Array"))
            .map(|arr| arr.to_vec())
    }

    fn object_uint8(obj: &JsValue, key: &str) -> Result<Vec<u8>, String> {
        let val = js_sys::Reflect::get(obj, &JsValue::from_str(key)).map_err(jsval_to_string)?;
        js_bytes(val, key)
    }

    /// The frontend's `BridgeTransport`: the generated typed client marshals each
    /// encoded request through `js_call` to the worker and reads back the
    /// `BridgeResult` bytes (and the out-of-band `ys` for buffer methods). Unit
    /// struct — the worker holds all session state.
    struct WorkerTransport;

    impl pb::BridgeTransport for WorkerTransport {
        async fn unary(&self, method: &str, request: Vec<u8>) -> Result<Vec<u8>, String> {
            let obj = js_call(method, request, Vec::new())
                .await
                .map_err(jsval_to_string)?;
            object_uint8(&obj, "result")
        }

        async fn unary_with_buffer(
            &self,
            method: &str,
            request: Vec<u8>,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            let obj = js_call(method, request, Vec::new())
                .await
                .map_err(jsval_to_string)?;
            Ok((object_uint8(&obj, "result")?, object_uint8(&obj, "ys")?))
        }
    }

    fn client() -> pb::BridgeServiceClient<WorkerTransport> {
        pb::BridgeServiceClient::new(WorkerTransport)
    }

    fn view_track_values(view: &ViewData, track: &ViewTrack) -> (&'static str, Vec<f32>) {
        if track.nf > 0 {
            return ("waterfall", Vec::new());
        }
        if track.dt > 0.0 {
            return ("raw", view.samples(track).to_vec());
        }

        let stride = if track.dt == 0.0 { 2 } else { 3 };
        let off = usize::try_from(track.offset).unwrap_or(usize::MAX);
        let cnt = usize::try_from(track.count).unwrap_or(usize::MAX);
        let start = off.min(view.ys.len());
        let end = off
            .saturating_add(cnt.saturating_mul(stride))
            .min(view.ys.len());
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
        let mut values = Vec::with_capacity(cnt.saturating_mul(2));
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
        let toff = usize::try_from(params.track_offset).unwrap_or(usize::MAX);
        let tcount = usize::try_from(params.track_count).unwrap_or(usize::MAX);

        let rendered_tracks: Vec<_> = view.meta.tracks.iter().skip(toff).take(tcount).collect();
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

        for (idx, track) in view.meta.tracks.iter().enumerate().skip(toff).take(tcount) {
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
                let parsed = client().make_example().await;
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
                let parsed = client()
                    .load_bytes(pb::LoadBytesRequest {
                        name,
                        fmt,
                        append: false,
                        content: data,
                    })
                    .await;
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
                let parsed = client().load_url(pb::LoadUrlRequest { url, fmt }).await;
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
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = client()
                    .get_spectrum(params)
                    .await
                    .map(|(meta, ys)| SpectrumData { meta, ys });
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
            let request = pb::NamedSnufflingRequest {
                name,
                params: Some(params),
            };
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = client().run_snuffling(request).await.ok();
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
        pub fn download_waveforms(&self, ctx: &egui::Context, params: WaveformDownloadRequest) {
            self.inner.borrow_mut().status = Status::Loading("Downloading waveforms…".into());
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = client().download_waveforms(params).await;
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
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = client().rms_traces(params).await;
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
        pub fn catalog_search(&self, ctx: &egui::Context, params: CatalogRequest) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match client().catalog_search(params).await {
                    Ok(r) => Some(r),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: e,
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
            let rays = cake_mode(&params) == "plot_rays";
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
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = client().cake_plot(params).await.ok();
                {
                    let mut i = inner.borrow_mut();
                    match parsed.and_then(|p| p.result) {
                        Some(pb::cake_plot_result::Result::Model(m)) => {
                            i.cake_model_plot = Some(m);
                        }
                        Some(pb::cake_plot_result::Result::Rays(r)) => {
                            i.cake_rays_plot = Some(r);
                        }
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

        /// Mount a Green's-functions store directory (`NativeFS`, via the File
        /// System Access API) and add its embedded 1D earth model to the Cake
        /// Phase dropdown as `GF Store: <id>`. Errors (unsupported browser, not a
        /// store) surface in the snuffling text window.
        #[cfg(feature = "gf")]
        pub fn add_gf_store(&self, ctx: &egui::Context) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                match js_add_gf_store().await {
                    Ok(v) => {
                        if let Some(name) = v.as_string()
                            && !name.is_empty()
                        {
                            inner.borrow_mut().pending_cake_model = Some(name);
                        }
                    }
                    Err(e) => {
                        let mut i = inner.borrow_mut();
                        i.snuffling = Some(SnufflingResult {
                            show_window: true,
                            text: format!("Add GF store failed: {}", jsval_to_string(e)),
                            markers: Vec::new(),
                            scene: None,
                        });
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
                let res = match client()
                    .run_command(pb::StringRequest { value: line })
                    .await
                {
                    Ok(r) => Some(r),
                    Err(e) => Some(pb::CommandResult {
                        action: "error".into(),
                        message: Some(e),
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
        pub fn fetch_stations(&self, ctx: &egui::Context, params: StationRequest) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match client().fetch_stations(params).await {
                    Ok(r) => Some(r),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: e,
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
        pub fn fetch_responses(&self, ctx: &egui::Context, params: StationRequest) {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            spawn_local(async move {
                let parsed = match client().fetch_responses(params).await {
                    Ok(r) => Some(r),
                    Err(e) => Some(SnufflingResult {
                        show_window: true,
                        text: e,
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
                        Ok(v) => match js_bytes(v, "open_files") {
                            Ok(bytes) if bytes.is_empty() => i.status = Status::Ready,
                            Ok(bytes) => match proto::scene_from_bytes(&bytes, "open_files") {
                                Ok(scene) => {
                                    i.scene = Some(scene);
                                    i.scene_gen += 1;
                                    i.status = Status::Ready;
                                    i.view = None;
                                    i.applied = None;
                                }
                                Err(e) => i.status = Status::Error(e),
                            },
                            Err(e) => i.status = Status::Error(e),
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
        pub fn save_markers(&self, _ctx: &egui::Context, markers: MarkerFile) {
            spawn_local(async move {
                let _ = js_save_markers(proto::encode_message(&markers)).await;
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
                    Ok(v) => js_bytes(v, "markers")
                        .ok()
                        .filter(|bytes| !bytes.is_empty())
                        .and_then(|bytes| proto::markers_from_bytes(&bytes, "markers").ok())
                        .map(|msg| msg.markers),
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
                let parsed = client().map_data().await.ok();
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
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            let me = self.clone();
            spawn_local(async move {
                let parsed = client()
                    .get_view(params.clone())
                    .await
                    .map(|(meta, ys)| ViewData { meta, ys });
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

    use super::{
        Bridge, CatalogRequest, MarkerFile, SnufflingRequestParams, StationRequest, Status,
        ViewParams, WaveformDownloadRequest,
    };

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

        pub fn download_waveforms(&self, _ctx: &egui::Context, _params: WaveformDownloadRequest) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn catalog_search(&self, _ctx: &egui::Context, _params: CatalogRequest) {}

        pub fn cake_plot(&self, _ctx: &egui::Context, _params: SnufflingRequestParams) {}

        pub fn add_cake_model(&self, _ctx: &egui::Context) {}

        #[cfg(feature = "gf")]
        pub fn add_gf_store(&self, _ctx: &egui::Context) {}

        pub fn run_command(&self, _ctx: &egui::Context, _line: String) {}

        pub fn rms_traces(&self, _ctx: &egui::Context, _params: SnufflingRequestParams) {}

        pub fn fetch_stations(&self, _ctx: &egui::Context, _params: StationRequest) {}

        pub fn fetch_responses(&self, _ctx: &egui::Context, _params: StationRequest) {}

        pub fn fetch_map_data(&self, _ctx: &egui::Context) {}

        pub fn request_tile(&self, _ctx: &egui::Context, _z: u8, _x: u32, _y: u32, _url: String) {}

        pub fn open_files(&self, _ctx: &egui::Context) {
            self.inner.borrow_mut().status = Status::Error(MSG.into());
        }

        pub fn save_markers(&self, _ctx: &egui::Context, _markers: MarkerFile) {}

        pub fn save_image_png(&self, _bytes: Vec<u8>, _filename: String) {}

        pub fn save_image_svg(&self, _svg: String, _filename: String) {}

        pub fn load_markers(&self, _ctx: &egui::Context) {}

        pub fn ensure_view(&self, _ctx: &egui::Context, _params: ViewParams) {}
    }
}
