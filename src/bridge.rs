//! The bridge between the egui frontend and pyrocko.
//!
//! On the web, this calls into `web/bridge.js`, which drives Pyodide
//! (CPython + numpy + scipy + pyrocko, all compiled to WASM) and returns
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
#[allow(dead_code)] // full wire contract; not every field is consumed yet
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
    /// Amplitude grouping: "individual" | "common" | "per_station" |
    /// "per_station_location" | "per_component" (pyrocko derives the per-track
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
#[allow(dead_code)] // net/sta/loc/cha kept for selection/marker scoping
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
        &self.ys[t.offset.min(self.ys.len())..end]
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
#[allow(dead_code)]
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
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub kind: u8,
}

/// Result of running a built-in snuffling: a text report and/or markers to add.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SnufflingResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub markers: Vec<SnufMarker>,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // Loading/Booting are only constructed on the web target
pub enum Status {
    #[default]
    Booting,
    Loading(String),
    Ready,
    Error(String),
}

#[derive(Default)]
#[allow(dead_code)] // view_in_flight/desired are only used on the web target
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
    view_in_flight: bool,
    desired: Option<ViewParams>,
    applied: Option<ViewParams>,
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

    /// Borrow the current view (decimated waveforms) for drawing.
    pub fn view(&self) -> Option<Ref<'_, ViewData>> {
        let b = self.inner.borrow();
        if b.view.is_some() {
            Some(Ref::map(b, |i| i.view.as_ref().unwrap()))
        } else {
            None
        }
    }
}

// Web target: drive Pyodide through web/bridge.js.
#[cfg(target_arch = "wasm32")]
mod web {
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

        /// Run a built-in snuffling (MinMax, STA/LTA, …) and store its result.
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
            let params = match self.inner.borrow().desired.clone() {
                Some(p) => p,
                None => return,
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
    use super::*;

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

        pub fn ensure_view(&self, _ctx: &egui::Context, _params: ViewParams) {}
    }
}
