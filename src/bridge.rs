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
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ViewParams {
    pub tmin: f64,
    pub tmax: f64,
    pub npx: u32,
    pub highpass: Option<f64>,
    pub lowpass: Option<f64>,
    pub sort: String,
    /// Rotation of horizontal components, degrees.
    pub rotate: f64,
    /// Deconvolve instrument response (inert without response metadata).
    pub remove_response: bool,
    /// "wiggle" or "waterfall" (spectrogram).
    pub mode: String,
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
    view_in_flight: bool,
    desired: Option<ViewParams>,
    applied: Option<ViewParams>,
    #[cfg(not(target_arch = "wasm32"))]
    native: native::NativeData,
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

// Native target: a self-contained synthetic stub so the UI runs under `cargo run`.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;

    pub struct NativeTrace {
        pub net: String,
        pub sta: String,
        pub loc: String,
        pub cha: String,
        pub tmin: f64,
        pub deltat: f64,
        pub ys: Vec<f32>,
    }

    impl NativeTrace {
        fn label(&self) -> String {
            format!("{}.{}.{}.{}", self.net, self.sta, self.loc, self.cha)
        }
        fn meta(&self) -> TraceMeta {
            TraceMeta {
                net: self.net.clone(),
                sta: self.sta.clone(),
                loc: self.loc.clone(),
                cha: self.cha.clone(),
                label: self.label(),
                tmin: self.tmin,
                tmax: self.tmin + self.deltat * (self.ys.len().max(1) - 1) as f64,
                deltat: self.deltat,
                nsamples: self.ys.len(),
            }
        }
    }

    #[derive(Default)]
    pub struct NativeData {
        pub traces: Vec<NativeTrace>,
    }

    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> f32 {
            // xorshift32 -> [-1, 1)
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            (x as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    impl NativeData {
        pub fn make_example(&mut self) -> SceneMeta {
            let deltat = 0.02_f64; // 50 Hz
            let n = 30_000usize; // 600 s
            let tmin = 1_700_000_000.0_f64;
            let stations = ["STA01", "STA02", "STA03", "STA04", "STA05", "STA06"];
            let channels = ["HHZ", "HHN", "HHE"];
            let mut traces = Vec::new();
            let mut seed = 0x1234_5678u32;
            for (si, sta) in stations.iter().enumerate() {
                let arrival = 180.0 + si as f64 * 18.0; // s, moves out with distance
                for cha in channels {
                    seed = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
                    let mut rng = Rng(seed | 1);
                    let mut ys = vec![0.0f32; n];
                    let mut lp = 0.0f32; // smoothed noise
                    for (i, y) in ys.iter_mut().enumerate() {
                        let t = i as f64 * deltat;
                        lp = lp * 0.96 + rng.next() * 0.04;
                        let mut v = lp * 0.3;
                        let dt = t - arrival;
                        if dt > 0.0 {
                            let env = (-(dt * dt) / (2.0 * 8.0 * 8.0)).exp();
                            v += (env * (2.0 * std::f64::consts::PI * 1.5 * dt).sin()) as f32;
                        }
                        *y = v;
                    }
                    traces.push(NativeTrace {
                        net: "XX".into(),
                        sta: (*sta).into(),
                        loc: "".into(),
                        cha: cha.into(),
                        tmin,
                        deltat,
                        ys,
                    });
                }
            }
            self.traces = traces;
            SceneMeta {
                tmin,
                tmax: tmin + deltat * (n - 1) as f64,
                tracks: self.traces.iter().map(|t| t.meta()).collect(),
            }
        }

        pub fn compute_view(&self, p: &ViewParams) -> ViewData {
            let mut order: Vec<usize> = (0..self.traces.len()).collect();
            order.sort_by(|&a, &b| self.traces[a].label().cmp(&self.traces[b].label()));
            if p.sort == "name_reverse" {
                order.reverse();
            }
            let npx = p.npx.max(16) as usize;
            let mut ys: Vec<f32> = Vec::new();
            let mut tracks = Vec::new();
            for idx in order {
                let tr = &self.traces[idx];
                let i0 = (((p.tmin - tr.tmin) / tr.deltat).floor() as i64).max(0) as usize;
                let i1 = (((p.tmax - tr.tmin) / tr.deltat).ceil() as i64).max(0) as usize;
                let i1 = i1.min(tr.ys.len());
                if i0 >= i1 {
                    continue;
                }
                let mut seg: Vec<f32> = tr.ys[i0..i1].to_vec();
                apply_filter(&mut seg, tr.deltat, p.highpass, p.lowpass);
                let offset = ys.len();
                let (count, dt) = decimate_into(&seg, tr.deltat, npx, &mut ys);
                let drawn = &ys[offset..offset + count];
                let (mut mn, mut mx, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f64);
                for &v in drawn {
                    mn = mn.min(v);
                    mx = mx.max(v);
                    sum += v as f64;
                }
                tracks.push(ViewTrack {
                    net: tr.net.clone(),
                    sta: tr.sta.clone(),
                    loc: tr.loc.clone(),
                    cha: tr.cha.clone(),
                    label: tr.label(),
                    t0_rel: (tr.tmin + i0 as f64 * tr.deltat) - p.tmin,
                    dt,
                    count,
                    offset,
                    ymin: if mn.is_finite() { mn } else { 0.0 },
                    ymax: if mx.is_finite() { mx } else { 0.0 },
                    ymean: if count > 0 {
                        (sum / count as f64) as f32
                    } else {
                        0.0
                    },
                    nf: 0,
                    nt: 0,
                    fmax: 0.0,
                });
            }
            ViewData {
                meta: ViewMeta {
                    view_tmin: p.tmin,
                    view_tmax: p.tmax,
                    tracks,
                },
                ys,
            }
        }
    }

    /// Max-abs decimation: collapse to ~`npx` bins, keeping the visually
    /// dominant sample of each bin. Returns (count, dt_between_drawn_samples).
    fn decimate_into(seg: &[f32], deltat: f64, npx: usize, out: &mut Vec<f32>) -> (usize, f64) {
        let n = seg.len();
        if n <= npx * 2 {
            out.extend_from_slice(seg);
            return (n, deltat);
        }
        let stride = n.div_ceil(npx);
        let mut count = 0;
        let mut i = 0;
        while i < n {
            let end = (i + stride).min(n);
            let mut best = seg[i];
            for &v in &seg[i..end] {
                if v.abs() > best.abs() {
                    best = v;
                }
            }
            out.push(best);
            count += 1;
            i = end;
        }
        (count, deltat * stride as f64)
    }

    /// Cheap one-pole IIR high/low pass — only for the native dev stub; the real
    /// (Butterworth, scipy) filtering happens in pyrocko on the web.
    fn apply_filter(seg: &mut [f32], deltat: f64, highpass: Option<f64>, lowpass: Option<f64>) {
        if let Some(fc) = lowpass {
            let rc = 1.0 / (2.0 * std::f64::consts::PI * fc);
            let alpha = (deltat / (rc + deltat)) as f32;
            let mut y = seg.first().copied().unwrap_or(0.0);
            for x in seg.iter_mut() {
                y += alpha * (*x - y);
                *x = y;
            }
        }
        if let Some(fc) = highpass {
            let rc = 1.0 / (2.0 * std::f64::consts::PI * fc);
            let a = (rc / (rc + deltat)) as f32;
            let mut prev_x = seg.first().copied().unwrap_or(0.0);
            let mut prev_y = 0.0f32;
            for x in seg.iter_mut() {
                let cur = *x;
                prev_y = a * (prev_y + cur - prev_x);
                prev_x = cur;
                *x = prev_y;
            }
        }
    }

    impl Bridge {
        pub fn load_example(&self, _ctx: &egui::Context) {
            let mut i = self.inner.borrow_mut();
            let scene = i.native.make_example();
            i.scene = Some(scene);
            i.scene_gen += 1;
            i.status = Status::Ready;
            i.view = None;
            i.applied = None;
        }

        pub fn load_file(&self, _ctx: &egui::Context, name: String, _data: Vec<u8>, _fmt: String) {
            // No file decoding in the native stub.
            self.inner.borrow_mut().status =
                Status::Error(format!("file loading ({name}) only works in the browser"));
        }

        pub fn load_url(&self, _ctx: &egui::Context, url: String, _fmt: String) {
            self.inner.borrow_mut().status =
                Status::Error(format!("loading {url} only works in the browser"));
        }

        pub fn fetch_spectrum(&self, _ctx: &egui::Context, _params: ViewParams) {
            // Spectrum snuffling needs numpy's FFT — web only.
        }

        pub fn ensure_view(&self, _ctx: &egui::Context, params: ViewParams) {
            let mut i = self.inner.borrow_mut();
            if !matches!(i.status, Status::Ready) {
                return;
            }
            if i.applied.as_ref() == Some(&params) {
                return;
            }
            let vd = i.native.compute_view(&params);
            i.view = Some(vd);
            i.view_epoch += 1;
            i.applied = Some(params);
        }
    }
}
