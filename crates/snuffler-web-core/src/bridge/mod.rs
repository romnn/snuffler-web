//! The bridge data model plus the wasm transport between a frontend and pyrocko.
//!
//! On the web, this calls into `web/bridge.js`, which forwards protobuf request
//! bytes to a Pyodide Web Worker (`CPython` + numpy + scipy + pyrocko, all
//! compiled to WASM) and returns protobuf metadata plus raw sample buffers.
//!
//! `proto/snuffler/bridge/v1/*.proto` is the boundary contract.
//! The Rust structs below are the app-facing decoded representation.
#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "host builds exercise pure protocol code while the bridge runtime is only active in wasm"
    )
)]

use std::cell::{Ref, RefCell};
use std::rc::Rc;

mod proto;

// The decoded protobuf messages ARE the app-facing domain model — no parallel
// hand-written structs, no conversion layer. prost generates these from
// proto/snuffler/bridge/v1/*.proto; the bridge passes them straight to the frontend.
pub use proto::pb::{
    BundleView, CakeSnufflingParams, CatalogRequest, CommandRequest, ExampleDataset, FigureRequest,
    IngestMiniSeedRequest, MapData, MarkerFile, MarkerFileEntry, MomentTensor, Nslc,
    PolarizationSnufflingParams, RmsSnufflingParams, SceneMeta, SeisbenchSnufflingParams,
    SeismosizerSnufflingParams, SetActiveEventRequest, SnufflingParams, SnufflingRequestParams,
    SnufflingResult, SpectrogramSnufflingParams, SpectrumSnufflingParams, StaltaSnufflingParams,
    StationRequest, ViewMeta, ViewParams, ViewSpan, ViewTrack, WaveformDownloadRequest,
};

/// Local-file header of a zip archive, which is how a dataset bundle is
/// recognized wherever one can arrive.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// Visible live waveform window once enough samples have arrived.
pub const LIVE_FOLLOW_SECONDS: f64 = 300.0;
/// Amount of live data kept in the Pyodide trace pile before older samples are
/// discarded.
pub const LIVE_RETAIN_SECONDS: f64 = 1800.0;

/// Metadata for the currently active `SeedLink` acquisition session.
#[derive(Clone, Copy, Debug)]
pub struct LiveSeedlinkSession {
    /// Epoch time when the current live source was started by the user.
    pub started_at: f64,
    /// Width of the live-follow viewport, in seconds.
    pub follow_seconds: f64,
}

/// A view: prost metadata plus the out-of-band `f32` sample buffer (`ys` rides
/// alongside the proto, never inside it — see the worker transfer).
#[derive(Clone, Debug, Default)]
pub struct ViewData {
    /// Track metadata and the absolute time represented by the sample buffer.
    pub meta: ViewMeta,
    /// Out-of-band waveform samples, envelope points, or packed waterfall cells.
    pub ys: Vec<f32>,
}

impl ViewData {
    /// Raw uniform samples or waterfall cells for one span as a flat slice.
    #[must_use]
    pub fn samples<'a>(&'a self, span: &ViewSpan) -> &'a [f32] {
        self.points(span, 1)
    }

    /// A span's payload as a flat slice, `stride` floats per point: 1 for uniform
    /// samples, 2 for `(t_rel, y)` pairs, 3 for `(t_rel, ylo, yhi)` envelope triples.
    #[must_use]
    pub fn points<'a>(&'a self, span: &ViewSpan, stride: usize) -> &'a [f32] {
        let off = usize::try_from(span.offset).unwrap_or(usize::MAX);
        let cnt = usize::try_from(span.count)
            .unwrap_or(usize::MAX)
            .saturating_mul(stride);
        let end = off.saturating_add(cnt).min(self.ys.len());
        self.ys.get(off.min(self.ys.len())..end).unwrap_or(&[])
    }
}

/// A rendered pyrocko/matplotlib figure: PNG bytes, or a non-empty `error` string
/// the window shows as text (a figure that fails never takes down the viewer —
/// mirroring how snuffling errors surface). The frontend decodes the PNG into a
/// texture once per `FigureSlot::generation`; the texture carries its own size.
#[derive(Clone, Debug, Default)]
pub struct FigureData {
    /// Encoded matplotlib PNG, empty until a successful render lands.
    pub png: Vec<u8>,
    /// Render failure text, empty when `png` is valid.
    pub error: String,
}

/// User-entered parameters for a live `SeedLink` acquisition.
#[derive(Clone, Debug, Default)]
pub struct SeedlinkParams {
    /// `SeedLink` server hostname.
    pub host: String,
    /// `SeedLink` TCP port.
    pub port: String,
    /// Network selector.
    pub net: String,
    /// Station selector.
    pub sta: String,
    /// Channel selector expression.
    pub selectors: String,
}

/// One figure window's latest render, keyed by figure kind in `Inner::figures`.
/// `gen` bumps on every fresh render so the frontend re-decodes the image only then;
/// `in_flight` de-dupes concurrent requests for the same kind.
#[derive(Clone, Debug, Default)]
struct FigureSlot {
    data: FigureData,
    generation: u64,
    in_flight: bool,
}

/// Lifecycle of a map tile fetch (RGBA bytes are handed to the view once, which
/// turns them into a texture and caches it there).
enum TileState {
    InFlight,
    Ready(u32, u32, Vec<u8>),
    Consumed,
    Failed,
}

/// Current lifecycle state of the asynchronous pyrocko bridge.
#[derive(Clone, Debug, Default)]
pub enum Status {
    /// Pyodide and the worker have not finished initializing.
    #[default]
    Booting,
    /// A user-visible operation is in progress.
    Loading(String),
    /// The bridge can accept requests.
    Ready,
    /// The latest operation failed with a user-visible message.
    Error(String),
}

/// How a ':' command line executed by pyrocko turned out.
#[derive(Clone, Debug)]
pub enum CommandOutcome {
    /// The command ran; any view change it asked for has been applied.
    Succeeded,
    /// The command was rejected, with the message to show under the input.
    Failed(String),
}

/// The Python runtime the worker booted, read back from the live interpreter
/// rather than assumed from the build, so the About window reports what is
/// actually running.
#[derive(Clone, Debug)]
pub struct RuntimeInfo {
    /// `sys.version`'s first token, e.g. `3.14.0`.
    pub python: String,
    /// `numpy.__version__`.
    pub numpy: String,
    /// `pyrocko.__version__`, or `unknown` on a build that does not carry one.
    pub pyrocko: String,
    /// True once pthreads are confirmed live in the interpreter — meaning the
    /// py-mt build came up with `OpenMP` and SIMD, not a single-threaded fallback.
    pub threaded: bool,
    /// `navigator.hardwareConcurrency` as seen by the worker; 0 when unavailable.
    pub cores: u32,
}

/// Progress reported by the in-browser `SeisBench` runtime.
#[derive(Clone, Debug)]
pub struct SeisbenchProgress {
    /// Completed fraction in the inclusive range `0.0..=1.0`.
    pub fraction: f32,
    /// Current model-loading or inference phase.
    pub label: String,
}

#[derive(Default)]
struct Inner {
    status: Status,
    scene: Option<SceneMeta>,
    /// Incremented every time a new scene is loaded, so the frontend can reset its
    /// view window when (and only when) the data actually changes.
    scene_gen: u64,
    view: Option<ViewData>,
    /// Incremented whenever new view data lands (for texture caching).
    view_epoch: u64,
    /// Latest render of each open figure window (amplitude spectrum, cake, …),
    /// keyed by figure kind. See [`FigureSlot`].
    figures: std::collections::HashMap<String, FigureSlot>,
    snuffling: Option<SnufflingResult>,
    snuffling_gen: u64,
    seisbench_in_flight: bool,
    last_seisbench_progress: Option<SeisbenchProgress>,
    next_seisbench_progress_poll_s: f64,
    /// A custom model name the user just uploaded ("Add Model"), for the frontend to
    /// add to the Cake Phase dropdown and select.
    pending_cake_model: Option<String>,
    map: Option<MapData>,
    /// Great-circle distance (degrees) from the active event to each station,
    /// keyed by (net, sta), for the marker editor's distance column. Refetched
    /// when the snuffling generation it was computed for changes.
    station_dists: std::collections::HashMap<(String, String), f64>,
    station_dists_gen: Option<(u64, u64)>,
    station_dists_in_flight: bool,
    /// Bumped when the user changes the active event, so distances refetch and the
    /// marker Kagan column updates without re-appending the marker set.
    active_event_gen: u64,
    /// Kagan angle (degrees) per event hash vs the active event (only events with a
    /// computed angle are present; missing = no MT / no active event).
    kagan_by_hash: std::collections::HashMap<String, f64>,
    tiles: std::collections::HashMap<(u8, u32, u32), TileState>,
    /// Bumped by [`Bridge::clear_tiles`]. Tiles are keyed by `(z, x, y)` alone, so
    /// a fetch still in flight when the basemap changes would otherwise land in
    /// the cleared map under the same key as its replacement.
    tile_epoch: u64,
    view_in_flight: Option<ViewParams>,
    desired: Option<ViewParams>,
    applied: Option<ViewParams>,
    view_request_gen: u64,
    /// A pending `goto <time>` target the session should jump the view to.
    pending_goto: Option<(f64, Option<f64>)>,
    /// How the last ':' command dispatched to pyrocko went, for the command line
    /// to close itself or show the error.
    command_outcome: Option<CommandOutcome>,
    live_seedlink_started_at: Option<f64>,
    /// The datasets `File ▸ Examples` offers, fetched the first time that menu is
    /// opened rather than compiled in — so a deployment can gain an example
    /// without a new wasm build. `None` until it has been asked for.
    examples: Option<Result<Vec<ExampleDataset>, String>>,
    examples_in_flight: bool,
    /// The view a just-loaded bundle asked for, for the frontend to apply — the
    /// bridge does not own the sort state, so it parks it here the way a fetched
    /// cake model waits in `pending_cake_model`.
    pending_view: Option<BundleView>,
}

/// Schedules frontend redraws after asynchronous bridge work completes.
#[derive(Clone)]
pub struct Repaint {
    request: Rc<dyn Fn()>,
    request_after: Rc<dyn Fn(std::time::Duration)>,
}

impl Repaint {
    /// Creates a redraw scheduler from frontend-owned callbacks.
    pub fn new(
        request: impl Fn() + 'static,
        request_after: impl Fn(std::time::Duration) + 'static,
    ) -> Self {
        Self {
            request: Rc::new(request),
            request_after: Rc::new(request_after),
        }
    }

    /// Requests a redraw as soon as the frontend can schedule one.
    pub fn request_repaint(&self) {
        (self.request)();
    }

    /// Requests a redraw no later than the supplied delay.
    pub fn request_repaint_after(&self, delay: std::time::Duration) {
        (self.request_after)(delay);
    }
}

/// Cloneable handle to pyrocko's asynchronous worker transport and latest snapshots.
#[derive(Clone)]
pub struct Bridge {
    inner: Rc<RefCell<Inner>>,
    repaint: Repaint,
}

impl Bridge {
    /// Creates an empty transport handle using the frontend's repaint scheduler.
    #[must_use]
    pub fn new(repaint: Repaint) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner::default())),
            repaint,
        }
    }

    /// Returns the latest bridge lifecycle state.
    #[must_use]
    pub fn status(&self) -> Status {
        self.inner.borrow().status.clone()
    }

    /// The example datasets on offer, or why they could not be listed.
    ///
    /// `None` means the index has not been fetched yet, which is what the menu
    /// shows as "Loading…" while [`Bridge::list_examples`] is in flight.
    #[must_use]
    pub fn examples(&self) -> Option<Result<Vec<ExampleDataset>, String>> {
        self.inner.borrow().examples.clone()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn request_repaint_after(&self, delay: std::time::Duration) {
        self.repaint.request_repaint_after(delay);
    }

    /// Returns the booted Python runtime, or `None` until the worker reports it.
    #[must_use]
    pub fn runtime_info(&self) -> Option<RuntimeInfo> {
        #[cfg(target_arch = "wasm32")]
        {
            web::runtime_info()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            None
        }
    }

    /// Returns the current inference device and optional `SeisBench` progress.
    #[cfg(target_arch = "wasm32")]
    pub fn seisbench_runtime(&self) -> (String, Option<SeisbenchProgress>) {
        let progress = self
            .inner
            .borrow()
            .seisbench_in_flight
            .then(web::seisbench_progress)
            .flatten();
        (web::seisbench_device(), progress)
    }

    /// Polls `SeisBench` progress at a bounded rate and schedules another repaint.
    #[cfg(target_arch = "wasm32")]
    pub fn refresh_seisbench_progress(&self, now: f64) {
        let repaint = self.repaint.clone();
        {
            let mut i = self.inner.borrow_mut();
            if !i.seisbench_in_flight {
                return;
            }
            if now < i.next_seisbench_progress_poll_s {
                repaint.request_repaint_after(std::time::Duration::from_millis(17));
                return;
            }
            i.next_seisbench_progress_poll_s = now + 0.1;
        }
        if let Some(progress) = web::seisbench_progress() {
            let pct = (progress.fraction * 100.0).round().clamp(0.0, 100.0);
            let mut i = self.inner.borrow_mut();
            let changed = i.last_seisbench_progress.as_ref().is_none_or(|last| {
                (last.fraction - progress.fraction).abs() > f32::EPSILON
                    || last.label != progress.label
            });
            if changed {
                i.status = Status::Loading(format!("{} · {pct:.0}%", progress.label));
                i.last_seisbench_progress = Some(progress);
            }
        }
        repaint.request_repaint_after(std::time::Duration::from_millis(17));
    }

    /// Returns the latest scene metadata snapshot.
    #[must_use]
    pub fn scene(&self) -> Option<SceneMeta> {
        self.inner.borrow().scene.clone()
    }

    /// Generation counter that increments on each new scene load.
    #[must_use]
    pub fn scene_gen(&self) -> u64 {
        self.inner.borrow().scene_gen
    }

    /// Current `SeedLink` session metadata, if live acquisition is active.
    #[must_use]
    pub fn live_seedlink_session(&self) -> Option<LiveSeedlinkSession> {
        self.inner
            .borrow()
            .live_seedlink_started_at
            .map(|started_at| LiveSeedlinkSession {
                started_at,
                follow_seconds: LIVE_FOLLOW_SECONDS,
            })
    }

    /// Take a pending `goto` (time, optional window length) set by a ':' command
    /// for the session to apply. A window length zooms to match the date specificity.
    #[must_use]
    pub fn take_goto(&self) -> Option<(f64, Option<f64>)> {
        self.inner.borrow_mut().pending_goto.take()
    }

    /// Take how the last ':' command dispatched to pyrocko turned out.
    #[must_use]
    pub fn take_command_outcome(&self) -> Option<CommandOutcome> {
        self.inner.borrow_mut().command_outcome.take()
    }

    /// Counter that increments whenever new view data is available.
    #[must_use]
    pub fn view_epoch(&self) -> u64 {
        self.inner.borrow().view_epoch
    }

    /// Generation of the latest render for a figure kind (0 = never rendered), so
    /// the frontend decodes the PNG only when it actually changes.
    #[must_use]
    pub fn figure_gen(&self, kind: &str) -> u64 {
        self.inner
            .borrow()
            .figures
            .get(kind)
            .map_or(0, |slot| slot.generation)
    }

    /// The latest rendered figure for a kind (PNG bytes or an error string).
    #[must_use]
    pub fn figure(&self, kind: &str) -> Option<FigureData> {
        self.inner
            .borrow()
            .figures
            .get(kind)
            .map(|slot| slot.data.clone())
    }

    /// Generation counter for snuffling results (so the session applies each once).
    #[must_use]
    pub fn snuffling_gen(&self) -> u64 {
        self.inner.borrow().snuffling_gen
    }

    /// Returns the latest marker/text result produced by a snuffling request.
    #[must_use]
    pub fn snuffling(&self) -> Option<SnufflingResult> {
        self.inner.borrow().snuffling.clone()
    }

    /// Takes a custom model name added via "Add Model" for shared configs to register.
    #[must_use]
    pub fn take_pending_cake_model(&self) -> Option<String> {
        self.inner.borrow_mut().pending_cake_model.take()
    }

    /// Takes the view a just-loaded bundle asked to be opened in, if it asked.
    #[must_use]
    pub fn take_pending_view(&self) -> Option<BundleView> {
        self.inner.borrow_mut().pending_view.take()
    }

    /// Returns the latest station and event coordinates for the map.
    #[must_use]
    pub fn map_data(&self) -> Option<MapData> {
        self.inner.borrow().map.clone()
    }

    /// Great-circle distance (degrees) from the active event to a station, if
    /// station coordinates + an active event are known (else `None`).
    #[must_use]
    pub fn station_distance(&self, net: &str, sta: &str) -> Option<f64> {
        self.inner
            .borrow()
            .station_dists
            .get(&(net.to_owned(), sta.to_owned()))
            .copied()
    }

    /// Generation counter bumped on each active-event change.
    #[must_use]
    pub fn active_event_gen(&self) -> u64 {
        self.inner.borrow().active_event_gen
    }

    /// Kagan angle (degrees) for an event hash vs the current active event, or
    /// `None` when it has no computed angle (no MT, or no active event).
    #[must_use]
    pub fn kagan_for(&self, hash: &str) -> Option<f64> {
        self.inner.borrow().kagan_by_hash.get(hash).copied()
    }

    /// Hand the view a ready tile's RGBA exactly once (it caches the texture).
    #[must_use]
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

    /// Discard every cached, in-flight, and failed tile so later requests refetch.
    ///
    /// The frontend calls this when the basemap provider changes: tile keys carry
    /// no provider, so without a reset the already-fetched tiles would keep
    /// showing the previous basemap while only new keys use the new one.
    pub fn clear_tiles(&self) {
        let mut i = self.inner.borrow_mut();
        i.tiles.clear();
        i.tile_epoch = i.tile_epoch.wrapping_add(1);
    }

    /// Borrow the current view (decimated waveforms) for drawing.
    #[must_use]
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
    use std::cell::{Cell, RefCell as StdRefCell};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::spawn_local;

    // The module path escapes the crate (`/` roots it at the crate directory,
    // `../..` walks up to the repo), so wasm-bindgen cannot embed the file as a
    // snippet: the emitted import resolves to `web/gen/bridge.js` under the site
    // root, and the trunk post-build hook must stage the file there
    // (`stage_bridge_module` in scripts/trunk-post-build.sh).
    #[wasm_bindgen(module = "/../../web/gen/bridge.js")]
    extern "C" {
        // The single transport seam for every unary proto RPC: (method, request
        // bytes, optional out-of-band data bytes) -> { result: BridgeResult bytes,
        // ys: Uint8Array }. bridge.js forwards it to the Pyodide worker, which runs
        // bridge.py and ships the out-of-band ys buffer alongside the proto. This is
        // the custom transport the typed service client rides on.
        #[wasm_bindgen(js_name = call, catch)]
        async fn js_call(method: &str, request: Vec<u8>, data: Vec<u8>)
        -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = seisbenchDevice)]
        fn js_seisbench_device() -> String;

        #[wasm_bindgen(js_name = seisbenchProgressFraction)]
        fn js_seisbench_progress_fraction() -> f32;

        #[wasm_bindgen(js_name = seisbenchProgressLabel)]
        fn js_seisbench_progress_label() -> String;

        #[wasm_bindgen(js_name = clearSeisbenchProgress)]
        fn js_clear_seisbench_progress();

        #[wasm_bindgen(js_name = runtimeInfo)]
        fn js_runtime_info() -> JsValue;

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

        #[wasm_bindgen(js_name = reloadFiles, catch)]
        async fn js_reload_files() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = openDirectory, catch)]
        async fn js_open_directory() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = reloadSnufflings, catch)]
        async fn js_reload_snufflings() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = saveMarkers, catch)]
        async fn js_save_markers(request: Vec<u8>) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadMarkers, catch)]
        async fn js_load_markers() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadStationXml, catch)]
        async fn js_load_station_xml() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadStations, catch)]
        async fn js_load_stations() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadEvents, catch)]
        async fn js_load_events() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = savePng)]
        fn js_save_png(bytes: &[u8], filename: &str);

        #[wasm_bindgen(js_name = saveSvg)]
        fn js_save_svg(svg: &str, filename: &str);
    }

    fn jsval_to_string(v: JsValue) -> String {
        v.as_string().unwrap_or_else(|| format!("{v:?}"))
    }

    pub(super) fn seisbench_device() -> String {
        js_seisbench_device()
    }

    pub(super) fn runtime_info() -> Option<RuntimeInfo> {
        let info = js_runtime_info();
        if !info.is_object() {
            return None;
        }
        let string = |key: &str| {
            js_sys::Reflect::get(&info, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "unknown".to_owned())
        };
        let flag = |key: &str| {
            js_sys::Reflect::get(&info, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_bool())
                == Some(true)
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "hardwareConcurrency is a small non-negative core count"
        )]
        let cores = js_sys::Reflect::get(&info, &JsValue::from_str("cores"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .max(0.0) as u32;
        Some(RuntimeInfo {
            python: string("python"),
            numpy: string("numpy"),
            pyrocko: string("pyrocko"),
            // Both have to be true: the build says it is threaded, the live
            // interpreter says pthreads really came up.
            threaded: flag("threaded") && flag("pthreads"),
            cores,
        })
    }

    pub(super) fn seisbench_progress() -> Option<SeisbenchProgress> {
        let fraction = js_seisbench_progress_fraction();
        if !(0.0..=1.0).contains(&fraction) {
            return None;
        }
        Some(SeisbenchProgress {
            fraction,
            label: js_seisbench_progress_label(),
        })
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

    fn object_f32(obj: &JsValue, key: &str) -> Result<Vec<f32>, String> {
        use wasm_bindgen::JsCast;

        let val = js_sys::Reflect::get(obj, &JsValue::from_str(key)).map_err(jsval_to_string)?;
        let arr = val
            .dyn_into::<js_sys::Uint8Array>()
            .map_err(|_| format!("{key} is not a Uint8Array"))?;
        if arr.byte_length() % 4 != 0 {
            return Err(format!("{key} byte length is not divisible by 4"));
        }
        let values = arr.byte_length() / 4;
        Ok(js_sys::Float32Array::new_with_byte_offset_and_length(
            &arr.buffer(),
            arr.byte_offset(),
            values,
        )
        .to_vec())
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::cast_precision_loss,
        reason = "transport diagnostics convert byte counts to MB and keep separate phase timings"
    )]
    fn log_transport_perf(
        method: &str,
        request_bytes: usize,
        result_bytes: usize,
        ys_bytes: usize,
        js_call_ms: f64,
        result_copy_ms: f64,
        ys_copy_ms: f64,
        total_ms: f64,
    ) {
        if !method.ends_with("/GetView") {
            return;
        }
        log::info!(
            "[bridge.transport] method=getView request_bytes={} result_bytes={} ys_bytes={} ys_mb={:.3} js_call_ms={:.2} rust_result_copy_ms={:.2} rust_ys_copy_ms={:.2} total_ms={:.2}",
            request_bytes,
            result_bytes,
            ys_bytes,
            ys_bytes as f64 / 1_000_000.0,
            js_call_ms,
            result_copy_ms,
            ys_copy_ms,
            total_ms,
        );
    }

    /// The frontend's `BridgeTransport`: the generated typed client marshals each
    /// encoded request through `js_call` to the worker and reads back the
    /// `BridgeResult` bytes (and the out-of-band `ys` for buffer methods). Unit
    /// struct — the worker holds all session state.
    struct WorkerTransport;

    const BRIDGE_TRANSPORT_PERF_LOG: bool = false;

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
        ) -> Result<(Vec<u8>, Vec<f32>), String> {
            if !BRIDGE_TRANSPORT_PERF_LOG {
                let obj = js_call(method, request, Vec::new())
                    .await
                    .map_err(jsval_to_string)?;
                return Ok((object_uint8(&obj, "result")?, object_f32(&obj, "ys")?));
            }
            let t0 = js_sys::Date::now();
            let request_bytes = request.len();
            let obj = js_call(method, request, Vec::new())
                .await
                .map_err(jsval_to_string)?;
            let js_done = js_sys::Date::now();
            let result = object_uint8(&obj, "result")?;
            let result_done = js_sys::Date::now();
            let ys = object_f32(&obj, "ys")?;
            let done = js_sys::Date::now();
            log_transport_perf(
                method,
                request_bytes,
                result.len(),
                ys.len() * 4,
                js_done - t0,
                result_done - js_done,
                done - result_done,
                done - t0,
            );
            Ok((result, ys))
        }
    }

    fn client() -> pb::BridgeServiceClient<WorkerTransport> {
        pb::BridgeServiceClient::new(WorkerTransport)
    }

    thread_local! {
        static LIVE_SEEDLINK: StdRefCell<Option<LiveSeedlinkHandle>> = const { StdRefCell::new(None) };
    }

    struct LiveSeedlinkHandle {
        ws: web_sys::WebSocket,
        on_open: Closure<dyn FnMut(web_sys::Event)>,
        on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
        on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
        on_close: Closure<dyn FnMut(web_sys::Event)>,
    }

    impl Drop for LiveSeedlinkHandle {
        fn drop(&mut self) {
            self.ws.set_onopen(None);
            self.ws.set_onmessage(None);
            self.ws.set_onerror(None);
            self.ws.set_onclose(None);
            let _ = self.ws.close();
            let _ = (
                &self.on_open,
                &self.on_message,
                &self.on_error,
                &self.on_close,
            );
        }
    }

    impl LiveSeedlinkHandle {
        fn connect(
            inner: Rc<RefCell<Inner>>,
            repaint: Repaint,
            params: SeedlinkParams,
            started_at: f64,
        ) -> Result<Self, String> {
            let conn = seedlink_connection(&params)?;
            let first_batch = Rc::new(Cell::new(true));
            let on_open = seedlink_on_open(
                conn.ws.clone(),
                inner.clone(),
                repaint.clone(),
                conn.source.clone(),
                conn.commands,
            );
            let on_message = seedlink_on_message(
                inner.clone(),
                repaint.clone(),
                conn.source.clone(),
                started_at,
                first_batch,
            );
            let on_error = seedlink_on_error(inner.clone(), repaint.clone());
            let on_close = seedlink_on_close(inner, repaint, conn.source);

            conn.ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            conn.ws
                .set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            conn.ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            conn.ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

            Ok(Self {
                ws: conn.ws,
                on_open,
                on_message,
                on_error,
                on_close,
            })
        }
    }

    struct SeedlinkConnection {
        ws: web_sys::WebSocket,
        source: String,
        commands: Vec<String>,
    }

    fn seedlink_connection(params: &SeedlinkParams) -> Result<SeedlinkConnection, String> {
        let port = params
            .port
            .trim()
            .parse::<u16>()
            .map_err(|err| format!("SeedLink port: {err}"))?;
        let source = format!(
            "{}:{} {}.{} {}",
            params.host.trim(),
            port,
            params.net.trim(),
            params.sta.trim(),
            params.selectors.trim()
        );
        let ws = web_sys::WebSocket::new(&seedlink_proxy_url(params.host.trim(), port))
            .map_err(jsval_to_string)?;
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
        Ok(SeedlinkConnection {
            ws,
            source,
            commands: seedlink_commands(params),
        })
    }

    fn seedlink_on_open(
        ws: web_sys::WebSocket,
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        commands: Vec<String>,
    ) -> Closure<dyn FnMut(web_sys::Event)> {
        Closure::wrap(Box::new(move |_event: web_sys::Event| {
            for command in &commands {
                if let Err(err) = ws.send_with_str(command) {
                    set_bridge_error(&inner, format!("SeedLink send failed: {err:?}"));
                    repaint.request_repaint();
                    return;
                }
            }
            set_snuffling_text(&inner, format!("SeedLink connected: {source}"), false);
            inner.borrow_mut().status = Status::Ready;
            repaint.request_repaint();
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_message(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        started_at: f64,
        first_batch: Rc<Cell<bool>>,
    ) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
        let buffer = Rc::new(RefCell::new(Vec::new()));
        Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
            let bytes = seedlink_event_bytes(&event);
            if bytes.is_empty() {
                return;
            }
            let records = {
                let mut buf = buffer.borrow_mut();
                buf.extend_from_slice(&bytes);
                drain_seedlink_records(&mut buf)
            };
            if !records.is_empty() {
                let reset = first_batch.replace(false);
                ingest_live_mseed(
                    inner.clone(),
                    repaint.clone(),
                    source.clone(),
                    records,
                    started_at,
                    reset,
                );
            }
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_error(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
    ) -> Closure<dyn FnMut(web_sys::ErrorEvent)> {
        Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
            let msg = event.message();
            set_bridge_error(
                &inner,
                if msg.is_empty() {
                    "SeedLink WebSocket error".to_owned()
                } else {
                    format!("SeedLink WebSocket error: {msg}")
                },
            );
            repaint.request_repaint();
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_close(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
    ) -> Closure<dyn FnMut(web_sys::Event)> {
        Closure::wrap(Box::new(move |_event: web_sys::Event| {
            inner.borrow_mut().live_seedlink_started_at = None;
            set_snuffling_text(&inner, format!("SeedLink disconnected: {source}"), false);
            repaint.request_repaint();
            spawn_local(async {
                LIVE_SEEDLINK.with(|slot| {
                    slot.borrow_mut().take();
                });
            });
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_event_bytes(event: &web_sys::MessageEvent) -> Vec<u8> {
        let data = event.data();
        if data.is_instance_of::<js_sys::ArrayBuffer>() {
            js_sys::Uint8Array::new(&data).to_vec()
        } else if let Some(text) = data.as_string() {
            text.into_bytes()
        } else {
            Vec::new()
        }
    }

    fn set_bridge_error(inner: &Rc<RefCell<Inner>>, error: String) {
        inner.borrow_mut().status = Status::Error(error);
    }

    fn set_snuffling_text(inner: &Rc<RefCell<Inner>>, text: String, show_window: bool) {
        let mut i = inner.borrow_mut();
        i.snuffling = Some(SnufflingResult {
            show_window,
            text,
            markers: Vec::new(),
            scene: None,
        });
        i.snuffling_gen += 1;
    }

    fn seedlink_proxy_url(host: &str, port: u16) -> String {
        format!(
            "ws://127.0.0.1:17684/seedlink?host={}&port={}",
            encode_query_component(host),
            port
        )
    }

    fn encode_query_component(value: &str) -> String {
        js_sys::encode_uri_component(value)
            .as_string()
            .unwrap_or_default()
    }

    fn seedlink_commands(params: &SeedlinkParams) -> Vec<String> {
        let net = params.net.trim();
        let sta = params.sta.trim();
        let selectors = params.selectors.trim();
        let mut commands = vec!["HELLO\r".to_owned(), "SLPROTO 4.0\r".to_owned()];
        if !sta.is_empty() {
            commands.push(if net.is_empty() {
                format!("STATION {sta}\r")
            } else {
                format!("STATION {net}_{sta}\r")
            });
        }
        if !selectors.is_empty() {
            commands.push(format!(
                "SELECT {}\r",
                crate::seedlink::v4_selectors(selectors)
            ));
        }
        commands.push("DATA\r".to_owned());
        commands.push("END\r".to_owned());
        commands
    }

    use crate::seedlink::drain_records as drain_seedlink_records;

    fn ingest_live_mseed(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        content: Vec<u8>,
        start_time: f64,
        reset: bool,
    ) {
        spawn_local(async move {
            let parsed = client()
                .ingest_mini_seed(IngestMiniSeedRequest {
                    source,
                    content,
                    start_time,
                    reset,
                    retain_seconds: LIVE_RETAIN_SECONDS,
                })
                .await;
            {
                let mut i = inner.borrow_mut();
                match parsed {
                    Ok(scene) => {
                        i.scene = Some(scene);
                        i.scene_gen += 1;
                        i.status = Status::Ready;
                        clear_view(&mut i);
                    }
                    Err(e) => i.status = Status::Error(e),
                }
            }
            repaint.request_repaint();
        });
    }

    fn invalidate_view_request(i: &mut Inner) {
        i.applied = None;
        i.desired = None;
        i.view_in_flight = None;
        i.view_request_gen = i.view_request_gen.wrapping_add(1);
    }

    fn clear_view(i: &mut Inner) {
        i.view = None;
        invalidate_view_request(i);
    }

    /// The values one track would draw, flattened across its spans, for the
    /// diagnostic log. A gapped channel contributes every segment.
    fn view_track_values(view: &ViewData, track: &ViewTrack) -> (&'static str, Vec<f32>) {
        if track.nf > 0 {
            return ("waterfall", Vec::new());
        }
        let mut kind = "missing";
        let mut values = Vec::new();
        for span in &track.spans {
            if span.dt > 0.0 {
                kind = "raw";
                values.extend_from_slice(view.samples(span));
            } else if span.dt == 0.0 {
                kind = "points";
                values.extend(
                    view.points(span, 2)
                        .chunks_exact(2)
                        .filter_map(|pair| match pair {
                            [_, y] => Some(*y),
                            _ => None,
                        }),
                );
            } else {
                kind = "legacy-envelope";
                for triple in view.points(span, 3).chunks_exact(3) {
                    let [_, ylo, yhi] = triple else {
                        continue;
                    };
                    values.push(*ylo);
                    values.push(*yhi);
                }
            }
        }
        (kind, values)
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
    fn log_view_debug(params: &ViewParams, view: &ViewData, request_ms: f64) {
        if params.mode != "wiggle" {
            return;
        }
        let toff = usize::try_from(params.track_offset).unwrap_or(usize::MAX);
        let tcount = usize::try_from(params.track_count).unwrap_or(usize::MAX);

        let rendered_tracks: Vec<_> = view.meta.tracks.iter().skip(toff).take(tcount).collect();
        // Counted per span: a track carries one per segment of its channel, and a
        // gapped channel can well be raw in one piece and decimated in another.
        let payload_spans = || {
            rendered_tracks
                .iter()
                .filter(|track| track.nf == 0)
                .flat_map(|track| track.spans.iter())
                .filter(|span| span.count > 0)
        };
        let raw_tracks = payload_spans().filter(|span| span.dt > 0.0).count();
        let point_tracks = payload_spans().filter(|span| span.dt == 0.0).count();
        let legacy_tracks = payload_spans().filter(|span| span.dt < 0.0).count();
        let window = params.tmax - params.tmin;
        let ys_mb = view.ys.len() as f64 * 4.0 / 1_000_000.0;
        let throughput = if request_ms > 0.0 {
            ys_mb / (request_ms / 1000.0)
        } else {
            0.0
        };
        log::info!(
            "[view] t=[{:.6},{:.6}] dt_window={:.3}s npx={} track_page={}..{} returned_tracks={} ys={} ys_mb={:.3} request_ms={:.2} effective_mb_s={:.1} rendered_modes raw={} points={} legacy={}",
            params.tmin,
            params.tmax,
            window,
            params.npx,
            params.track_offset,
            params.track_offset + params.track_count,
            view.meta.tracks.len(),
            view.ys.len(),
            ys_mb,
            request_ms,
            throughput,
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
            // The first span stands for the row in this diagnostic line; a row that
            // carries several says so through `spans=`, and the detail belongs in a
            // per-span line if anyone ever needs it.
            let first = track.spans.first();
            let t0_rel = first.map_or(0.0, |span| span.t0_rel);
            let dt = first.map_or(0.0, |span| span.dt);
            let ymin = first.map_or(0.0, |span| span.ymin);
            let ymax = first.map_or(0.0, |span| span.ymax);
            let ymean = first.map_or(0.0, |span| span.ymean);
            let yscale = first.map_or(0.0, |span| span.yscale);
            if let Some((min_v, max_v, mean, rms)) = finite_stats(&values) {
                log::debug!(
                    "[view.track] idx={} label={} mode={} spans={} count={} points_per_px={:.3} data_min={:.6} data_max={:.6} data_mean={:.6} data_rms={:.6} scale_min={:.6} scale_max={:.6} scale_mean={:.6} yscale={:.6} t0_rel={:.6} dt={:.9} head=[{}]",
                    idx,
                    track.label,
                    mode,
                    track.spans.len(),
                    values.len(),
                    points_per_px,
                    min_v,
                    max_v,
                    mean,
                    rms,
                    ymin,
                    ymax,
                    ymean,
                    yscale,
                    t0_rel,
                    dt,
                    sample_head(&values),
                );
            } else {
                log::debug!(
                    "[view.track] idx={} label={} mode={} spans={} count=0 scale_min={:.6} scale_max={:.6} scale_mean={:.6} yscale={:.6} t0_rel={:.6} dt={:.9}",
                    idx,
                    track.label,
                    mode,
                    track.spans.len(),
                    ymin,
                    ymax,
                    ymean,
                    yscale,
                    t0_rel,
                    dt,
                );
            }
        }
    }

    impl Bridge {
        /// Replaces the current pile with the bundled synthetic example.
        pub fn load_example(&self) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Loading example…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().make_example().await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Replaces the current pile with waveform bytes supplied by the frontend.
        pub fn load_file(&self, name: String, data: Vec<u8>, fmt: String) {
            // A dropped file may be a whole dataset. Deciding on the bytes rather
            // than the extension means a bundle behaves the same however it arrives
            // — dropped, picked, or fetched from the Examples menu.
            if data.starts_with(ZIP_MAGIC) {
                self.load_bundle_bytes(name, data);
                return;
            }
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading(format!("Loading {name}…"));
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client()
                    .load_bytes(pb::LoadBytesRequest {
                        name,
                        fmt,
                        append: false,
                        content: data,
                        source_id: String::new(),
                    })
                    .await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Replaces the current pile with a waveform fetched from a URL.
        pub fn load_url(&self, url: String, fmt: String) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading(format!("Loading {url}…"));
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().load_url(pb::LoadUrlRequest { url, fmt }).await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Fetch the example index, once.
        ///
        /// The menu calls this on every frame it is open, so "once" has to mean
        /// once: a *failed* fetch is remembered too, or a datacenter that is down
        /// would be retried at the frame rate. `force` is the user asking again,
        /// from the retry the menu offers in its place.
        pub fn list_examples(&self, force: bool) {
            {
                let mut i = self.inner.borrow_mut();
                if i.examples_in_flight || (i.examples.is_some() && !force) {
                    return;
                }
                i.examples_in_flight = true;
            }
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            spawn_local(async move {
                let parsed = client().list_examples().await;
                {
                    let mut i = inner.borrow_mut();
                    i.examples_in_flight = false;
                    i.examples = Some(parsed.map(|list| list.examples));
                }
                repaint.request_repaint();
            });
        }

        /// Replaces the current pile with a whole dataset fetched as one archive:
        /// waveforms, station metadata and an event catalogue in a single request.
        ///
        /// The station and event results come back with the scene and are applied
        /// through the same path a separate metadata load would use, so the event
        /// markers appear exactly as if the catalogue had been opened by hand.
        pub fn load_bundle(&self, url: String, sha256: String, title: String) {
            let request = pb::LoadBundleRequest {
                url,
                sha256,
                content: Vec::new(),
                name: title.clone(),
            };
            self.dispatch_bundle(request, title);
        }

        /// Load an example by slug, fetching the index first if it has not been
        /// read yet. This is what `?example=<slug>` resolves to: a link someone can
        /// hand out that opens the app on a named dataset.
        ///
        /// An unknown slug is an error the user has to see — they followed a link
        /// that promised data — so it lands in the status line rather than only in
        /// the console.
        pub fn open_example(&self, slug: String) {
            let bridge = self.clone();
            self.inner.borrow_mut().status = Status::Loading(format!("Looking up {slug}…"));
            let repaint = self.repaint.clone();
            spawn_local(async move {
                let known = bridge.inner.borrow().examples.clone();
                let listed = match known {
                    Some(Ok(examples)) => Ok(examples),
                    _ => client().list_examples().await.map(|list| list.examples),
                };
                match listed {
                    Ok(examples) => {
                        let found = examples.iter().find(|e| e.slug == slug).cloned();
                        bridge.inner.borrow_mut().examples = Some(Ok(examples));
                        match found {
                            Some(example) => {
                                bridge.load_bundle(example.url, example.sha256, example.title)
                            }
                            None => {
                                bridge.inner.borrow_mut().status =
                                    Status::Error(format!("no example dataset called {slug:?}"));
                            }
                        }
                    }
                    Err(error) => bridge.inner.borrow_mut().status = Status::Error(error),
                }
                repaint.request_repaint();
            });
        }

        /// The same load for an archive that is already in hand — dropped on the
        /// window, or picked from disk — rather than fetched.
        pub(super) fn load_bundle_bytes(&self, name: String, data: Vec<u8>) {
            let request = pb::LoadBundleRequest {
                url: String::new(),
                sha256: String::new(),
                content: data,
                name: name.clone(),
            };
            self.dispatch_bundle(request, name);
        }

        fn dispatch_bundle(&self, request: pb::LoadBundleRequest, label: String) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading(format!("Loading {label}…"));
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().load_bundle(request).await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(bundle) => {
                            i.scene = Some(bundle.scene.unwrap_or_default());
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                            i.pending_view = bundle.view;
                            if let Some(info) = bundle.info {
                                i.snuffling = Some(info);
                                i.snuffling_gen += 1;
                            }
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Render a real pyrocko/matplotlib figure (amplitude spectrum, cake, …)
        /// in Pyodide and store the resulting PNG for the matching figure window.
        /// Keyed by `req.kind`, so several figure windows can be open at once.
        pub fn render_figure(&self, req: FigureRequest) {
            let repaint = self.repaint.clone();
            let kind = req.kind.clone();
            {
                let mut i = self.inner.borrow_mut();
                if !matches!(i.status, Status::Ready) {
                    return;
                }
                let slot = i.figures.entry(kind.clone()).or_default();
                if slot.in_flight {
                    return;
                }
                slot.in_flight = true;
            }
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().render_figure(req).await;
                {
                    let mut i = inner.borrow_mut();
                    let slot = i.figures.entry(kind).or_default();
                    slot.in_flight = false;
                    if let Ok(fr) = parsed {
                        slot.data = FigureData {
                            png: fr.png,
                            error: fr.error,
                        };
                        slot.generation = slot.generation.wrapping_add(1);
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Run a built-in snuffling (`MinMax`, `STA/LTA`, …) and store its result.
        pub fn run_snuffling(&self, name: String, params: SnufflingRequestParams) {
            let repaint = self.repaint.clone();
            let is_seisbench = name == "seisbench_picker";
            {
                let mut i = self.inner.borrow_mut();
                if !matches!(i.status, Status::Ready) {
                    return;
                }
                if is_seisbench {
                    js_clear_seisbench_progress();
                    i.seisbench_in_flight = true;
                    i.last_seisbench_progress = None;
                    i.next_seisbench_progress_poll_s = 0.0;
                    i.status = Status::Loading("SeisBench picker · 0%".to_owned());
                }
            }
            let request = pb::NamedSnufflingRequest {
                name,
                params: Some(params),
            };
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().run_snuffling(request).await;
                {
                    let mut i = inner.borrow_mut();
                    if is_seisbench {
                        i.seisbench_in_flight = false;
                        i.last_seisbench_progress = None;
                    }
                    match parsed {
                        Ok(mut r) => {
                            if let Some(scene) = r.scene.take() {
                                i.scene = Some(scene);
                                i.scene_gen += 1;
                                clear_view(&mut i);
                            }
                            i.status = Status::Ready;
                            i.snuffling = Some(r);
                            i.snuffling_gen += 1;
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Download waveforms from a CORS-enabled FDSN dataselect service and
        /// replace the pile with the result (browser fetch; pyrocko parses).
        pub fn download_waveforms(&self, params: WaveformDownloadRequest) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Downloading waveforms…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().download_waveforms(params).await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Compute blockwise-RMS derived traces (pyrocko's RMS snuffling) and
        /// append them to the pile; the result replaces the scene.
        pub fn rms_traces(&self, params: SnufflingRequestParams) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().rms_traces(params).await;
                {
                    let mut i = inner.borrow_mut();
                    match parsed {
                        Ok(scene) => {
                            i.scene = Some(scene);
                            i.scene_gen += 1;
                            i.status = Status::Ready;
                            clear_view(&mut i);
                        }
                        Err(e) => i.status = Status::Error(e),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Search a CORS-enabled FDSN event service; results become event markers.
        /// The largest-magnitude hit becomes the active event, so this forces a
        /// re-dispatch for the distance/azimuth sort to recompute against it.
        pub fn catalog_search(&self, params: CatalogRequest) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                    invalidate_view_request(&mut i);
                }
                repaint.request_repaint();
            });
        }

        /// Let the user pick a custom `.nd` model file ("Add Model"); the picked
        /// name is stored for the frontend to add to the Cake Phase dropdown.
        pub fn add_cake_model(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                if let Ok(v) = js_add_cake_model().await
                    && let Some(name) = v.as_string()
                    && !name.is_empty()
                {
                    inner.borrow_mut().pending_cake_model = Some(name);
                }
                repaint.request_repaint();
            });
        }

        /// Mount a Green's-functions store by fetching its files from an HTTP base
        /// URL into the Pyodide filesystem (no directory picker) and register it as
        /// `GF Store: <id>`. Errors surface in the snuffling text window.
        pub fn load_gf_store_url(&self, url: String) {
            let repaint = self.repaint.clone();
            if !matches!(self.inner.borrow().status, Status::Ready) {
                return;
            }
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                match client()
                    .load_gf_store_url(pb::StringRequest { value: url })
                    .await
                {
                    Ok(label) if !label.value.is_empty() => {
                        inner.borrow_mut().pending_cake_model = Some(label.value);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let mut i = inner.borrow_mut();
                        i.snuffling = Some(SnufflingResult {
                            show_window: true,
                            text: format!("Load GF store from URL failed: {e}"),
                            markers: Vec::new(),
                            scene: None,
                        });
                        i.snuffling_gen += 1;
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Mount a Green's-functions store directory (`NativeFS`, via the File
        /// System Access API) and add its embedded 1D earth model to the Cake
        /// Phase dropdown as `GF Store: <id>`. Errors (unsupported browser, not a
        /// store) surface in the snuffling text window.
        #[cfg(feature = "gf")]
        pub fn add_gf_store(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                repaint.request_repaint();
            });
        }

        /// Execute a ':' command line (goto / hide / unhide / reduce / scaling). A
        /// goto stores a pending time the session jumps to; a visibility change swaps
        /// the scene in place (no refit) and forces the view to re-dispatch. Either
        /// way the outcome lands in `command_outcome` for the command line to act on.
        pub fn run_command(&self, request: CommandRequest) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let result = match client().run_command(request).await {
                    Ok(r) => r,
                    Err(e) => pb::CommandResult {
                        action: "error".into(),
                        message: Some(e),
                        ..Default::default()
                    },
                };
                // Scoped so the borrow ends before `request_repaint()`: `Inner` is
                // the same `RefCell` the render loop reads through its own `Rc`, and
                // this is an async block, so a guard still alive at the repaint — or
                // at any `.await` a later edit puts below — is a `BorrowMutError`
                // panic on whichever frame or bridge result lands next.
                {
                    let mut i = inner.borrow_mut();
                    match result.action.as_str() {
                        "goto" => i.pending_goto = result.t.map(|t| (t, result.tlen)),
                        "scene" => {
                            if let Some(scene) = result.scene {
                                i.scene = Some(scene);
                                invalidate_view_request(&mut i); // re-dispatch without refitting time
                            }
                        }
                        _ => {}
                    }
                    i.command_outcome = Some(match (result.action.as_str(), result.message) {
                        ("error", Some(message)) => CommandOutcome::Failed(message),
                        ("error", None) => CommandOutcome::Failed("command failed".to_owned()),
                        _ => CommandOutcome::Succeeded,
                    });
                }
                repaint.request_repaint();
            });
        }

        /// Fetch station coordinates (FDSN station service) to enable distance /
        /// azimuth sorting; forces the view to re-dispatch so the sort recomputes.
        pub fn fetch_stations(&self, params: StationRequest) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                    invalidate_view_request(&mut i); // re-dispatch so distance/azimuth sort recomputes
                }
                repaint.request_repaint();
            });
        }

        /// Fetch instrument responses (`StationXML`) so Deconvolution can run;
        /// forces a re-dispatch so the deconvolved view recomputes.
        pub fn fetch_responses(&self, params: StationRequest) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                    invalidate_view_request(&mut i); // re-dispatch so deconvolution applies
                }
                repaint.request_repaint();
            });
        }

        /// Reports whether a live `SeedLink` socket is active.
        pub fn seedlink_active(&self) -> bool {
            let _scene_gen = self.inner.borrow().scene_gen;
            LIVE_SEEDLINK.with(|slot| slot.borrow().is_some())
        }

        /// Starts a live `SeedLink` source and replaces the current pile.
        pub fn start_seedlink(&self, params: SeedlinkParams) {
            let repaint = self.repaint.clone();
            LIVE_SEEDLINK.with(|slot| {
                slot.borrow_mut().take();
            });
            let started_at = js_sys::Date::now() / 1000.0;
            match LiveSeedlinkHandle::connect(
                self.inner.clone(),
                repaint.clone(),
                params,
                started_at,
            ) {
                Ok(handle) => {
                    LIVE_SEEDLINK.with(|slot| {
                        *slot.borrow_mut() = Some(handle);
                    });
                    let mut i = self.inner.borrow_mut();
                    i.live_seedlink_started_at = Some(started_at);
                    i.scene = Some(SceneMeta {
                        tmin: started_at,
                        tmax: started_at + LIVE_FOLLOW_SECONDS,
                        tracks: Vec::new(),
                    });
                    i.scene_gen += 1;
                    i.status = Status::Loading("Connecting SeedLink…".to_owned());
                    i.snuffling = None;
                    i.kagan_by_hash.clear();
                    i.station_dists.clear();
                    i.station_dists_gen = None;
                    i.active_event_gen = i.active_event_gen.wrapping_add(1);
                    clear_view(&mut i);
                }
                Err(err) => {
                    let mut i = self.inner.borrow_mut();
                    i.live_seedlink_started_at = None;
                    i.status = Status::Error(err);
                }
            }
            repaint.request_repaint();
        }

        /// Stops live `SeedLink` acquisition while retaining received samples.
        pub fn stop_seedlink(&self) {
            LIVE_SEEDLINK.with(|slot| {
                slot.borrow_mut().take();
            });
            self.inner.borrow_mut().live_seedlink_started_at = None;
            set_snuffling_text(&self.inner, "SeedLink stopped".to_owned(), false);
        }

        /// Open local waveform files via a browser picker and append them.
        pub fn open_files(&self) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Opening files…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let res = js_open_files().await;
                {
                    let mut i = inner.borrow_mut();
                    match res {
                        Ok(v) => match js_bytes(v, "open_files") {
                            Ok(bytes) if bytes.is_empty() => i.status = Status::Ready,
                            Ok(bytes) => {
                                match proto::scene_or_bundle_from_bytes(&bytes, "open_files") {
                                    Ok((scene, info)) => {
                                        i.scene = Some(scene);
                                        i.scene_gen += 1;
                                        i.status = Status::Ready;
                                        clear_view(&mut i);
                                        // A picked bundle also loaded stations and
                                        // events; its markers reach the app the
                                        // same way `load_events` delivers them.
                                        if let Some(info) = info {
                                            i.snuffling = Some(info);
                                            i.snuffling_gen += 1;
                                        }
                                    }
                                    Err(e) => i.status = Status::Error(e),
                                }
                            }
                            Err(e) => i.status = Status::Error(e),
                        },
                        Err(e) => i.status = Status::Error(jsval_to_string(e)),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Recursively load every waveform file under a picked directory
        /// (Chromium File System Access); appends them to the pile.
        pub fn open_directory(&self) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Loading directory…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let res = js_open_directory().await;
                {
                    let mut i = inner.borrow_mut();
                    match res {
                        Ok(v) => match js_bytes(v, "open_directory") {
                            Ok(bytes) if bytes.is_empty() => i.status = Status::Ready,
                            Ok(bytes) => match proto::scene_from_bytes(&bytes, "open_directory") {
                                Ok(scene) => {
                                    i.scene = Some(scene);
                                    i.scene_gen += 1;
                                    i.status = Status::Ready;
                                    clear_view(&mut i);
                                }
                                Err(e) => i.status = Status::Error(e),
                            },
                            Err(e) => i.status = Status::Error(e),
                        },
                        Err(e) => i.status = Status::Error(jsval_to_string(e)),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Reload the Python snuffling code (bridge.py) in the worker, preserving
        /// the loaded pile (snuffler's Shift+R). Forces a view re-fetch since the
        /// reload resets Python's view cache; the scene (track list) is unchanged.
        pub fn reload_snufflings(&self) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Reloading snufflings…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let res = js_reload_snufflings().await;
                {
                    let mut i = inner.borrow_mut();
                    match res {
                        Ok(_) => {
                            i.status = Status::Ready;
                            invalidate_view_request(&mut i);
                        }
                        Err(e) => i.status = Status::Error(jsval_to_string(e)),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Re-read any opened files whose on-disk mtime changed and replace their
        /// traces (snuffler's "r"). A no-op (stays Ready) when nothing changed or
        /// the files were opened without a re-readable File System Access handle.
        pub fn reload_files(&self) {
            let repaint = self.repaint.clone();
            self.inner.borrow_mut().status = Status::Loading("Reloading modified files…".into());
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let res = js_reload_files().await;
                {
                    let mut i = inner.borrow_mut();
                    match res {
                        Ok(v) => match js_bytes(v, "reload_files") {
                            Ok(bytes) if bytes.is_empty() => i.status = Status::Ready,
                            Ok(bytes) => match proto::scene_from_bytes(&bytes, "reload_files") {
                                Ok(scene) => {
                                    i.scene = Some(scene);
                                    i.scene_gen += 1;
                                    i.status = Status::Ready;
                                    clear_view(&mut i);
                                }
                                Err(e) => i.status = Status::Error(e),
                            },
                            Err(e) => i.status = Status::Error(e),
                        },
                        Err(e) => i.status = Status::Error(jsval_to_string(e)),
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Serialize markers to pyrocko marker format and download them.
        #[allow(
            clippy::unused_self,
            reason = "kept as a method for API symmetry with the other bridge calls; the fire-and-forget download needs no state"
        )]
        pub fn save_markers(&self, markers: MarkerFile) {
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
        pub fn load_markers(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                repaint.request_repaint();
            });
        }

        /// Pick and merge local `StationXML` files (instrument responses for
        /// deconvolution + station coordinates for sorting) using the same parsing
        /// as fetched `FDSN` responses.
        pub fn load_station_xml(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let result = match js_load_station_xml().await {
                    Ok(v) => js_bytes(v, "stationxml")
                        .ok()
                        .filter(|bytes| !bytes.is_empty())
                        .and_then(|bytes| proto::snuffling_from_bytes(&bytes, "stationxml").ok()),
                    Err(_) => None,
                };
                if let Some(result) = result {
                    let mut i = inner.borrow_mut();
                    i.snuffling = Some(result);
                    i.snuffling_gen += 1;
                }
                repaint.request_repaint();
            });
        }

        /// Pick a local pyrocko station file and load its station coordinates
        /// (distance/azimuth sorting, cake, map) via `model.load_stations`.
        pub fn load_stations(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let result = match js_load_stations().await {
                    Ok(v) => js_bytes(v, "stations")
                        .ok()
                        .filter(|bytes| !bytes.is_empty())
                        .and_then(|bytes| proto::snuffling_from_bytes(&bytes, "stations").ok()),
                    Err(_) => None,
                };
                if let Some(result) = result {
                    let mut i = inner.borrow_mut();
                    i.snuffling = Some(result);
                    i.snuffling_gen += 1;
                }
                repaint.request_repaint();
            });
        }

        /// Pick a local pyrocko event file and load it: event markers + the active
        /// event (largest magnitude) for distance/azimuth sorting, which forces a
        /// re-dispatch so that sort recomputes against the new active event.
        pub fn load_events(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let result = match js_load_events().await {
                    Ok(v) => js_bytes(v, "events")
                        .ok()
                        .filter(|bytes| !bytes.is_empty())
                        .and_then(|bytes| proto::snuffling_from_bytes(&bytes, "events").ok()),
                    Err(_) => None,
                };
                if let Some(result) = result {
                    let mut i = inner.borrow_mut();
                    i.snuffling = Some(result);
                    i.snuffling_gen += 1;
                    invalidate_view_request(&mut i);
                }
                repaint.request_repaint();
            });
        }

        /// Fetch station + event coordinates for the Map overlay.
        pub fn fetch_map_data(&self) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().map_data().await.ok();
                {
                    let mut i = inner.borrow_mut();
                    if let Some(m) = parsed {
                        i.map = Some(m);
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Ensure the per-station distances are current for the given snuffling
        /// generation (the active event changes with each catalog/event load). A
        /// no-op if already fetched for this generation or a fetch is in flight.
        pub fn ensure_station_distances(&self, snuffling_gen: u64) {
            let repaint = self.repaint.clone();
            let token;
            {
                let mut i = self.inner.borrow_mut();
                token = (snuffling_gen, i.active_event_gen);
                if i.station_dists_in_flight
                    || i.station_dists_gen == Some(token)
                    || !matches!(i.status, Status::Ready)
                {
                    return;
                }
                i.station_dists_in_flight = true;
            }
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let parsed = client().station_distances().await.ok();
                {
                    let mut i = inner.borrow_mut();
                    i.station_dists_in_flight = false;
                    i.station_dists_gen = Some(token);
                    if let Some(list) = parsed {
                        i.station_dists.clear();
                        for s in list.stations {
                            i.station_dists.insert((s.net, s.sta), s.distance_deg);
                        }
                    }
                }
                repaint.request_repaint();
            });
        }

        /// Silent variant of `reload_files` for the "Watch Files" poll: swaps in a
        /// new scene only when a watched file actually changed, without flickering
        /// the status line.
        pub fn watch_poll(&self) {
            let repaint = self.repaint.clone();
            if !matches!(self.inner.borrow().status, Status::Ready) {
                return;
            }
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let Ok(v) = js_reload_files().await else {
                    return;
                };
                let Ok(bytes) = js_bytes(v, "watch_poll") else {
                    return;
                };
                if bytes.is_empty() {
                    return;
                }
                let Ok(scene) = proto::scene_from_bytes(&bytes, "watch_poll") else {
                    return;
                };
                {
                    let mut i = inner.borrow_mut();
                    i.scene = Some(scene);
                    i.scene_gen += 1;
                    clear_view(&mut i);
                }
                repaint.request_repaint();
            });
        }

        /// Set (or clear) the active event in Python and pull back the recomputed
        /// per-event Kagan angles; bumps `active_event_gen` so distances refetch.
        pub fn set_active_event(&self, req: SetActiveEventRequest) {
            let repaint = self.repaint.clone();
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            spawn_local(async move {
                let res = client().set_active_event(req).await.ok();
                {
                    let mut i = inner.borrow_mut();
                    i.active_event_gen += 1;
                    if let Some(r) = res {
                        i.kagan_by_hash.clear();
                        for k in r.kagan {
                            if let Some(v) = k.kagan_deg {
                                i.kagan_by_hash.insert(k.event_hash, v);
                            }
                        }
                    }
                    // The distance/azimuth sorts and their track annotations are
                    // measured from the active event, so the view has to be
                    // rebuilt against the new one — its request params are
                    // unchanged, so nothing else would re-dispatch it.
                    invalidate_view_request(&mut i);
                }
                repaint.request_repaint();
            });
        }

        /// Fetch one map tile (browser fetch + decode); idempotent per (z,x,y).
        pub fn request_tile(&self, z: u8, x: u32, y: u32, url: String) {
            let repaint = self.repaint.clone();
            let epoch = {
                let mut i = self.inner.borrow_mut();
                if i.tiles.contains_key(&(z, x, y)) {
                    return;
                }
                i.tiles.insert((z, x, y), TileState::InFlight);
                i.tile_epoch
            };
            let inner = self.inner.clone();
            let repaint = repaint.clone();
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
                {
                    let mut i = inner.borrow_mut();
                    // The basemap changed while this tile was in flight; it now
                    // holds the wrong provider's imagery for its key.
                    if i.tile_epoch != epoch {
                        return;
                    }
                    i.tiles.insert((z, x, y), state);
                }
                repaint.request_repaint();
            });
        }

        /// Request a view for the given params, coalescing rapid changes so that
        /// at most one Python call is in flight at a time.
        pub fn ensure_view(&self, params: ViewParams) {
            let repaint = self.repaint.clone();
            {
                let mut i = self.inner.borrow_mut();
                if !matches!(i.status, Status::Ready) {
                    return;
                }
                if i.applied.as_ref() == Some(&params) {
                    if i.view_in_flight.is_none() {
                        i.desired = None;
                    } else {
                        i.desired = Some(params);
                        repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    }
                    return;
                }
                if i.view_in_flight.as_ref() == Some(&params) {
                    i.desired = None;
                    repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    return;
                }
                i.desired = Some(params);
                if i.view_in_flight.is_some() {
                    repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    return;
                }
            }
            self.dispatch_view();
        }

        fn dispatch_view(&self) {
            let repaint = self.repaint.clone();
            let (params, request_gen) = {
                let mut i = self.inner.borrow_mut();
                if i.view_in_flight.is_some() {
                    return;
                }
                let Some(params) = i.desired.take() else {
                    return;
                };
                if i.applied.as_ref() == Some(&params) {
                    return;
                }
                i.view_in_flight = Some(params.clone());
                (params, i.view_request_gen)
            };
            repaint.request_repaint_after(std::time::Duration::from_millis(17));
            let inner = self.inner.clone();
            let repaint = repaint.clone();
            let me = self.clone();
            spawn_local(async move {
                let t0 = js_sys::Date::now();
                let parsed = client()
                    .get_view(params.clone())
                    .await
                    .map(|(meta, ys)| ViewData { meta, ys });
                let request_ms = js_sys::Date::now() - t0;
                {
                    let mut i = inner.borrow_mut();
                    if i.view_request_gen == request_gen {
                        match parsed {
                            Ok(vd) => {
                                log_view_debug(&params, &vd, request_ms);
                                if i.desired.as_ref().is_none_or(|desired| desired == &params) {
                                    i.view = Some(vd);
                                    i.view_epoch += 1;
                                    i.applied = Some(params.clone());
                                    if i.desired.as_ref() == Some(&params) {
                                        i.desired = None;
                                    }
                                }
                            }
                            Err(e) => i.status = Status::Error(e),
                        }
                        if i.view_in_flight.as_ref() == Some(&params) {
                            i.view_in_flight = None;
                        }
                    } else if i.view_in_flight.as_ref() == Some(&params) {
                        i.view_in_flight = None;
                    }
                }
                repaint.request_repaint();
                me.maybe_redispatch();
            });
        }

        fn maybe_redispatch(&self) {
            let need = {
                let mut i = self.inner.borrow_mut();
                if i.view_in_flight.is_some() {
                    false
                } else if i.desired.is_some() && i.applied.as_ref() != i.desired.as_ref() {
                    true
                } else {
                    i.desired = None;
                    false
                }
            };
            if need {
                self.dispatch_view();
            }
        }
    }
}
