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

use crate::activity::{
    ActivityLedger, CancelRequest, JobError, JobHandle, JobRecord, Operation, Outcome,
    RuntimeState, Submission,
};
#[cfg(target_arch = "wasm32")]
use crate::activity::{JobState, Progress};

mod error;
mod jobs;
mod proto;
mod runtime;

pub use error::{
    BridgeError, MarshalError, ReloadError, ReloadFailureReason, ReloadSourceError, SeedlinkError,
};
pub use jobs::{CommandOutcome, FigureData, ReadyTile, TileRequest, ViewData};
use jobs::{FigureSlot, TileState};
#[cfg(target_arch = "wasm32")]
use jobs::{TILE_BATCH, TILE_BATCHES_IN_FLIGHT};
pub use runtime::{
    LIVE_FOLLOW_SECONDS, LIVE_RETAIN_SECONDS, LiveSeedlinkSession, RuntimeInfo, SeedlinkParams,
    SeisbenchProgress,
};
#[cfg(target_arch = "wasm32")]
use runtime::{LiveIngestBatch, SessionEpoch, StreamId};
use runtime::{LiveIngestMailbox, LiveIngestState};
pub use snuffler_web_proto::{CallError, RemoteError, TransportError};

// The decoded protobuf messages ARE the app-facing domain model — no parallel
// hand-written structs, no conversion layer. prost generates these from
// proto/snuffler/bridge/v1/*.proto; the bridge passes them straight to the frontend.
pub use proto::pb::{
    ActiveEventResult, BrowserFileManifest, BrowserSourceAck, BrowserSourceAckList, BundleAbout,
    BundleView, CakeSnufflingParams, CatalogRequest, CommandRequest, EventKagan, EventSnapshot,
    ExampleDataset, FigureRequest, IngestMiniSeedRequest, MapData, MapRequest,
    MarkerBudget as ProtoMarkerBudget, MarkerFile, MarkerFileEntry, MomentTensor, Nslc,
    PolarizationSnufflingParams, RemoveEventsRequest, RmsSnufflingParams, SceneMeta,
    SeisbenchSnufflingParams, SeismosizerSnufflingParams, SetActiveEventRequest, SnufflingParams,
    SnufflingRequestParams, SnufflingResult, SnufflingSpan, SpectrogramSnufflingParams,
    SpectrumSnufflingParams, StaltaSnufflingParams, StationRequest, TraceBox, ViewMeta, ViewParams,
    ViewSpan, ViewTrack, WaveformDownloadRequest,
};

/// Local-file header of a zip archive, which is how a dataset bundle is
/// recognized wherever one can arrive.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

#[derive(Default)]
struct Inner {
    activity: ActivityLedger,
    /// Latest recoverable failure that should remain visible without making the
    /// bridge operationally unavailable.
    diagnostic: Option<BridgeError>,
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
    /// A custom model name the user just uploaded ("Add Model"), for the frontend to
    /// add to the Cake Phase dropdown and select.
    pending_cake_model: Option<String>,
    map: Option<MapData>,
    map_request: Option<MapRequest>,
    map_request_gen: u64,
    map_job: Option<JobHandle>,
    /// Great-circle distance (degrees) from the active event to each station,
    /// keyed by (net, sta), for the marker editor's distance column. Refetched
    /// when the snuffling generation it was computed for changes.
    station_dists: std::collections::HashMap<(String, String), f64>,
    station_dists_gen: Option<(u64, u64)>,
    station_dists_job: Option<JobHandle>,
    /// Bumped whenever Python returns a canonical event-state projection.
    active_event_gen: u64,
    event_state: Option<ActiveEventResult>,
    tiles: std::collections::HashMap<(u8, u32, u32), TileState>,
    /// Bumped by [`Bridge::clear_tiles`]. Tiles are keyed by `(z, x, y)` alone, so
    /// a fetch still in flight when the basemap changes would otherwise land in
    /// the cleared map under the same key as its replacement.
    tile_epoch: u64,
    /// Wanted tiles that no batch has started yet, ordered by priority. Replaced
    /// wholesale on every [`Bridge::request_tiles`], which is what lets a zoom or
    /// a pan abandon the tiles it just moved away from instead of fetching them.
    tile_queue: Vec<TileRequest>,
    /// How many batches are out; see [`TILE_BATCHES_IN_FLIGHT`].
    tile_batches: usize,
    view_job: Option<(JobHandle, ViewParams)>,
    view_last_error: Option<BridgeError>,
    view_retry_after_s: f64,
    next_view_sequence: u64,
    python_session_generation: u64,
    desired: Option<ViewParams>,
    applied: Option<ViewParams>,
    view_request_gen: u64,
    /// A pending `goto <time>` target the session should jump the view to.
    pending_goto: Option<(f64, Option<f64>)>,
    /// How the last ':' command dispatched to pyrocko went, for the command line
    /// to close itself or show the error.
    command_outcome: Option<CommandOutcome>,
    live_seedlink_started_at: Option<f64>,
    session_epoch: u64,
    next_stream_id: u64,
    live_ingest: LiveIngestState,
    ingest_job: Option<JobHandle>,
    ingest_mailbox: LiveIngestMailbox,
    /// Follow-viewport width of the active live session, from its start form
    /// (0.0 while no session runs; readers fall back to the default).
    live_seedlink_follow_s: f64,
    /// The datasets `File ▸ Examples` offers, fetched the first time that menu is
    /// opened rather than compiled in — so a deployment can gain an example
    /// without a new wasm build. `None` until it has been asked for.
    examples: Option<Result<Vec<ExampleDataset>, BridgeError>>,
    examples_job: Option<JobHandle>,
    warm_requested: bool,
    /// The view a just-loaded bundle asked for, for the frontend to apply — the
    /// bridge does not own the sort state, so it parks it here the way a fetched
    /// cake model waits in `pending_cake_model`.
    pending_view: Option<BundleView>,
    /// What the loaded dataset says about itself, paired with the scene generation
    /// it arrived with.
    ///
    /// Unlike the view, this is not spent on arrival — `Help ▸ About this dataset`
    /// shows it again later — so it needs invalidating, and the generation does
    /// that on its own: any later load bumps `scene_gen`, and prose about the
    /// previous pile stops being about what is on screen without a single call
    /// site having to remember to clear it.
    dataset_note: Option<(u64, BundleAbout)>,
    /// Whether that note has yet to be put on screen. A dataset introduces itself
    /// once, when it arrives — which is a fact known here, at the arrival, rather
    /// than something the frontend should have to infer by watching for the
    /// generation to change.
    dataset_note_unseen: bool,
}

fn install_bundle_presentation(
    inner: &mut Inner,
    view: Option<BundleView>,
    about: Option<BundleAbout>,
) {
    inner.pending_view = view;
    inner.dataset_note_unseen = about.is_some();
    inner.dataset_note = about.map(|about| (inner.scene_gen, about));
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

    /// Returns runtime health and an owned snapshot of all visible jobs.
    #[must_use]
    pub fn activity_snapshot(&self) -> (RuntimeState, Vec<(JobHandle, JobRecord)>) {
        #[cfg(target_arch = "wasm32")]
        web::sync_runtime(&self.inner);
        let inner = self.inner.borrow();
        (
            inner.activity.runtime().clone(),
            inner
                .activity
                .records()
                .map(|(handle, record)| (handle, record.clone()))
                .collect(),
        )
    }

    /// Registers executor-owned local work before it leaves the render loop.
    pub fn begin_local(&self, operation: Operation, detail: Option<String>) -> Submission {
        #[cfg(target_arch = "wasm32")]
        let now_s = js_sys::Date::now() / 1000.0;
        #[cfg(not(target_arch = "wasm32"))]
        let now_s = 0.0;
        self.inner
            .borrow_mut()
            .activity
            .submit_local(operation, detail, now_s)
    }

    /// Records the sole terminal result for executor-owned local work.
    pub fn finish_local(&self, handle: JobHandle, result: Result<(), BridgeError>) {
        let outcome = match result {
            Ok(()) => Outcome::Succeeded,
            Err(error) => Outcome::Failed(JobError::Bridge(error)),
        };
        self.inner.borrow_mut().activity.complete(handle, outcome);
        self.repaint.request_repaint();
    }

    /// Requests cancellation without allowing repeated UI actions to resubmit work.
    #[must_use]
    pub fn cancel(&self, handle: JobHandle) -> proto::pb::CancelDisposition {
        #[cfg(target_arch = "wasm32")]
        let now_s = js_sys::Date::now() / 1000.0;
        #[cfg(not(target_arch = "wasm32"))]
        let now_s = 0.0;
        let request = self
            .inner
            .borrow_mut()
            .activity
            .request_cancel(handle, now_s);
        let disposition = match request {
            CancelRequest::Cancelled => proto::pb::CancelDisposition::Queued,
            CancelRequest::Interrupt | CancelRequest::AlreadyCancelling => {
                #[cfg(target_arch = "wasm32")]
                {
                    match web::cancel_job(handle) {
                        Ok(disposition) => disposition,
                        Err(error) => {
                            self.inner.borrow_mut().diagnostic = Some(error);
                            proto::pb::CancelDisposition::Cancelling
                        }
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    proto::pb::CancelDisposition::Cancelling
                }
            }
            CancelRequest::TooLate => proto::pb::CancelDisposition::TooLate,
            CancelRequest::NotFound => proto::pb::CancelDisposition::NotFound,
            CancelRequest::NotCancellable => proto::pb::CancelDisposition::NotCancellable,
        };
        if disposition == proto::pb::CancelDisposition::TooLate {
            self.inner
                .borrow_mut()
                .activity
                .mark_cancel_too_late(handle);
        }
        self.repaint.request_repaint();
        disposition
    }

    /// Replaces an unresponsive Python worker after cancellation escalation.
    pub fn restart_runtime(&self) {
        #[cfg(target_arch = "wasm32")]
        web::restart_runtime(&self.inner);
        self.repaint.request_repaint();
    }

    /// Returns the latest recoverable diagnostic while the bridge remains ready.
    #[must_use]
    pub fn diagnostic(&self) -> Option<BridgeError> {
        self.inner.borrow().diagnostic.clone()
    }

    /// The example datasets on offer, or why they could not be listed.
    ///
    /// `None` means the index has not been fetched yet, which is what the menu
    /// shows as "Loading…" while [`Bridge::list_examples`] is in flight.
    #[must_use]
    pub fn examples(&self) -> Option<Result<Vec<ExampleDataset>, BridgeError>> {
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
            .activity
            .records()
            .find_map(|(_, record)| {
                if record.operation() != Operation::RunSnuffling
                    || record.detail() != Some("seisbench_picker")
                {
                    return None;
                }
                let JobState::Running {
                    progress: Progress::Fraction { fraction, phase },
                    ..
                } = record.state()
                else {
                    return None;
                };
                Some(SeisbenchProgress {
                    fraction: *fraction,
                    label: phase.to_string(),
                })
            });
        (web::seisbench_device(), progress)
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
        let inner = self.inner.borrow();
        inner
            .live_seedlink_started_at
            .map(|started_at| LiveSeedlinkSession {
                started_at,
                follow_seconds: if inner.live_seedlink_follow_s > 0.0 {
                    inner.live_seedlink_follow_s
                } else {
                    LIVE_FOLLOW_SECONDS
                },
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

    /// Returns the ledger handle behind a pending figure render.
    #[must_use]
    pub fn figure_job(&self, kind: &str) -> Option<JobHandle> {
        self.inner
            .borrow()
            .figures
            .get(kind)
            .and_then(|slot| slot.job)
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

    /// Whether a dataset has just introduced itself and has not been shown yet.
    /// True once per load, for the frontend to open the window with.
    #[must_use]
    pub fn take_unseen_dataset_note(&self) -> bool {
        std::mem::take(&mut self.inner.borrow_mut().dataset_note_unseen)
    }

    /// Whether the dataset now on screen says anything about itself — for the
    /// menu entry that only needs to exist or not, without cloning the prose.
    #[must_use]
    pub fn has_dataset_note(&self) -> bool {
        let inner = self.inner.borrow();
        inner
            .dataset_note
            .as_ref()
            .is_some_and(|(described, _)| *described == inner.scene_gen)
    }

    /// What the dataset now on screen says about itself, if it says anything.
    #[must_use]
    pub fn dataset_note(&self) -> Option<BundleAbout> {
        let inner = self.inner.borrow();
        inner
            .dataset_note
            .as_ref()
            .filter(|(described, _)| *described == inner.scene_gen)
            .map(|(_, about)| about.clone())
    }

    /// Returns the latest station and event coordinates for the map.
    #[must_use]
    pub fn map_data(&self) -> Option<MapData> {
        self.inner.borrow().map.clone()
    }

    /// Reports whether the cached or in-flight map belongs to this request.
    #[must_use]
    pub fn map_request_is_current(&self, request: &MapRequest) -> bool {
        self.inner.borrow().map_request.as_ref() == Some(request)
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

    /// Returns Python's latest canonical event-registry projection.
    #[must_use]
    pub fn event_state(&self) -> Option<ActiveEventResult> {
        self.inner.borrow().event_state.clone()
    }

    /// Take every tile that has finished decoding, each exactly once, for the view
    /// to upload as a texture.
    ///
    /// Draining all of them — rather than asking per key — matters because the
    /// view moves on: a tile that lands after a zoom has left its level would
    /// otherwise sit here holding a megabyte of RGBA that nobody ever asks for.
    #[must_use]
    pub fn take_ready_tiles(&self) -> Vec<ReadyTile> {
        let mut i = self.inner.borrow_mut();
        let ready: Vec<(u8, u32, u32)> = i
            .tiles
            .iter()
            .filter(|(_, state)| matches!(state, TileState::Ready(..)))
            .map(|(key, _)| *key)
            .collect();
        ready
            .into_iter()
            .filter_map(|key| match i.tiles.insert(key, TileState::Consumed) {
                Some(TileState::Ready(width, height, rgba)) => Some(ReadyTile {
                    z: key.0,
                    x: key.1,
                    y: key.2,
                    width,
                    height,
                    rgba,
                }),
                _ => None,
            })
            .collect()
    }

    /// Forget a tile so a later request fetches it again.
    ///
    /// The view calls this when it evicts a texture: a tile it has already
    /// consumed is remembered here as fetched, so without this the evicted tile
    /// could never be asked for a second time.
    pub fn forget_tile(&self, z: u8, x: u32, y: u32) {
        self.inner.borrow_mut().tiles.remove(&(z, x, y));
    }

    /// Discard every cached, in-flight, and failed tile so later requests refetch.
    ///
    /// The frontend calls this when the basemap provider changes: tile keys carry
    /// no provider, so without a reset the already-fetched tiles would keep
    /// showing the previous basemap while only new keys use the new one.
    pub fn clear_tiles(&self) {
        let mut i = self.inner.borrow_mut();
        i.tiles.clear();
        i.tile_queue.clear();
        i.tile_epoch = i.tile_epoch.wrapping_add(1);
    }

    /// Borrow the current view (decimated waveforms) for drawing.
    #[must_use]
    pub fn view(&self) -> Option<Ref<'_, ViewData>> {
        let b = self.inner.borrow();
        Ref::filter_map(b, |i| i.view.as_ref()).ok()
    }

    /// Whether the current view is drawable and has no replacement queued or in flight.
    #[must_use]
    pub fn view_is_settled(&self) -> bool {
        let i = self.inner.borrow();
        i.view.is_some() && i.desired.is_none() && i.view_job.is_none()
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
    use crate::activity::RuntimeEpoch;
    use snuffler_web_proto::prost::Message as _;
    use std::cell::{Cell, RefCell as StdRefCell};
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
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
        async fn js_call(
            method: &str,
            request: Vec<u8>,
            data: Vec<u8>,
            job_id: u64,
        ) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = takeOperationEvents)]
        fn js_take_operation_events() -> js_sys::Array;

        #[wasm_bindgen(js_name = seisbenchDevice)]
        fn js_seisbench_device() -> String;

        #[wasm_bindgen(js_name = runtimeInfo)]
        fn js_runtime_info() -> JsValue;

        #[wasm_bindgen(js_name = currentRuntimeEpoch)]
        fn js_current_runtime_epoch() -> u32;

        #[wasm_bindgen(js_name = cancelJob, catch)]
        fn js_cancel_job(request: Vec<u8>) -> Result<Vec<u8>, JsValue>;

        #[wasm_bindgen(js_name = restartRuntime)]
        fn js_restart_runtime(reason: &str) -> u32;

        // DOM-only calls keep bespoke externs: they drive browser APIs (file
        // pickers, downloads, the File System Access API, tile fetch+decode) that
        // can't be expressed as a uniform request->response RPC.
        #[wasm_bindgen(js_name = addCakeModel, catch)]
        async fn js_add_cake_model(job_id: u64) -> Result<JsValue, JsValue>;

        #[cfg(feature = "gf")]
        #[wasm_bindgen(js_name = addGfStore, catch)]
        async fn js_add_gf_store(job_id: u64) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = fetchTiles, catch)]
        async fn js_fetch_tiles(
            urls: js_sys::Array,
            on_tile: js_sys::Function,
        ) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = openFiles, catch)]
        async fn js_open_files() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = reloadFiles, catch)]
        async fn js_reload_files() -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = acknowledgeSources)]
        fn js_acknowledge_sources(bytes: Vec<u8>);
        #[wasm_bindgen(js_name = clearOpenSources)]
        fn js_clear_open_sources();

        #[wasm_bindgen(js_name = openDirectory, catch)]
        async fn js_open_directory() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = reloadSnufflings, catch)]
        async fn js_reload_snufflings(job_id: u64) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = downloadFile, catch)]
        async fn js_download_file(
            bytes: Vec<u8>,
            filename: String,
            mime: String,
        ) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadMarkers, catch)]
        async fn js_load_markers() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadStationXml, catch)]
        async fn js_load_station_xml() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadStations, catch)]
        async fn js_load_stations() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = loadEvents, catch)]
        async fn js_load_events() -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = savePng, catch)]
        async fn js_save_png(bytes: Vec<u8>, filename: String) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_name = saveSvg, catch)]
        async fn js_save_svg(svg: String, filename: String) -> Result<JsValue, JsValue>;
    }

    pub(super) fn sync_runtime(inner: &Rc<RefCell<Inner>>) {
        let info = js_runtime_info();
        let ready = !info.is_null() && !info.is_undefined();
        {
            let mut state = inner.borrow_mut();
            if ready && !matches!(state.activity.runtime(), RuntimeState::Ready) {
                state.activity.set_runtime(RuntimeState::Ready);
            }
        }
        drain_operation_events(inner);
    }

    pub(super) fn cancel_job(handle: JobHandle) -> Result<pb::CancelDisposition, BridgeError> {
        let request = pb::CancelJobRequest {
            job_id: handle.id().get(),
        }
        .encode_to_vec();
        let bytes = js_cancel_job(request).map_err(|error| {
            BridgeError::browser("cancelling Python work", jsval_to_string(error))
        })?;
        let reply: pb::CancelJobReply = proto::decode_message(&bytes, "cancelJob")?;
        pb::CancelDisposition::try_from(reply.disposition).map_err(|_| {
            BridgeError::browser("cancelling Python work", "unknown cancellation disposition")
        })
    }

    pub(super) fn restart_runtime(inner: &Rc<RefCell<Inner>>) {
        let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
        inner.borrow_mut().activity.runtime_lost(epoch);
        js_restart_runtime("Python did not stop after cancellation");
    }

    fn drain_operation_events(inner: &Rc<RefCell<Inner>>) {
        for value in js_take_operation_events().iter() {
            let Ok(bytes) = value.dyn_into::<js_sys::Uint8Array>() else {
                set_bridge_error(
                    inner,
                    BridgeError::browser("reading Python progress", "event was not bytes"),
                );
                continue;
            };
            let event = match pb::WorkerEvent::decode(bytes.to_vec().as_slice()) {
                Ok(event) => event,
                Err(error) => {
                    set_bridge_error(
                        inner,
                        BridgeError::browser("decoding Python progress", error.to_string()),
                    );
                    continue;
                }
            };
            if event.runtime_epoch != js_current_runtime_epoch() {
                continue;
            }
            let Some(pb::worker_event::Event::Operation(operation)) = event.event else {
                continue;
            };
            match operation.event {
                Some(pb::operation_event::Event::Started(_)) => {
                    inner
                        .borrow_mut()
                        .activity
                        .mark_wire_running(operation.job_id, js_sys::Date::now() / 1000.0);
                }
                Some(pb::operation_event::Event::Progress(progress)) => {
                    let phase = progress.phase.into();
                    let value = match progress.fraction {
                        None => Progress::Count {
                            done: progress.done,
                            total: progress.total,
                            phase,
                        },
                        Some(fraction) => {
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "wire progress is normalized before conversion to the UI's f32"
                            )]
                            let fraction = fraction.clamp(0.0, 1.0) as f32;
                            Progress::Fraction { fraction, phase }
                        }
                    };
                    inner
                        .borrow_mut()
                        .activity
                        .update_wire_progress(operation.job_id, value);
                }
                Some(pb::operation_event::Event::Heartbeat(_)) | None => {}
            }
        }
    }

    struct DispatchPermit {
        inner: Rc<RefCell<Inner>>,
        handle: JobHandle,
    }

    thread_local! {
        static ACTIVE_LEDGER_JOBS: StdRefCell<Vec<(JobHandle, crate::activity::Lane)>> =
            const { StdRefCell::new(Vec::new()) };
    }

    fn require_registered_job(
        operation: pb::Operation,
        method: &'static str,
    ) -> Result<JobHandle, TransportError> {
        ACTIVE_LEDGER_JOBS.with(|active| {
            active
                .borrow()
                .iter()
                .find(|(_, lane)| *lane == operation.activity().lane)
                .map(|(handle, _)| *handle)
                .map_or_else(
                    || {
                        Err(TransportError::new(
                            method,
                            "Python call has no registered activity job",
                        ))
                    },
                    Ok,
                )
        })
    }

    impl Future for DispatchPermit {
        type Output = bool;

        fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            if this.inner.borrow().activity.is_terminal(this.handle) {
                return Poll::Ready(false);
            }
            if this
                .inner
                .borrow_mut()
                .activity
                .try_dispatch(this.handle, context.waker())
            {
                Poll::Ready(true)
            } else {
                Poll::Pending
            }
        }
    }

    fn submit_job<F>(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        operation: Operation,
        detail: Option<String>,
        dedup_key: Vec<u8>,
        work: F,
    ) -> Submission
    where
        F: Future<Output = Result<(), BridgeError>> + 'static,
    {
        sync_runtime(&inner);
        let submission = inner.borrow_mut().activity.submit(
            operation,
            detail,
            dedup_key,
            js_sys::Date::now() / 1000.0,
        );
        if operation.activity().supersedes {
            let active: Vec<JobHandle> = inner
                .borrow()
                .activity
                .records()
                .filter(|(handle, record)| {
                    *handle != submission.handle()
                        && record.operation().activity().lane == crate::activity::Lane::Job
                        && matches!(
                            record.state(),
                            JobState::Dispatching | JobState::Running { .. }
                        )
                })
                .map(|(handle, _)| handle)
                .collect();
            for handle in active {
                let cancel = inner
                    .borrow_mut()
                    .activity
                    .request_cancel(handle, js_sys::Date::now() / 1000.0);
                if matches!(
                    cancel,
                    CancelRequest::Interrupt | CancelRequest::AlreadyCancelling
                ) {
                    match cancel_job(handle) {
                        Ok(pb::CancelDisposition::TooLate) => {
                            inner.borrow_mut().activity.mark_cancel_too_late(handle);
                        }
                        Ok(_) => {}
                        Err(error) => inner.borrow_mut().diagnostic = Some(error),
                    }
                }
            }
        }
        if let Submission::Accepted(handle) = submission {
            let scheduled_inner = inner.clone();
            let scheduled_repaint = repaint.clone();
            spawn_local(async move {
                let epoch = RuntimeEpoch::new(js_current_runtime_epoch());
                let dispatch = DispatchPermit {
                    inner: scheduled_inner.clone(),
                    handle,
                }
                .await;
                if !dispatch {
                    scheduled_repaint.request_repaint();
                    return;
                }
                scheduled_repaint.request_repaint();
                ACTIVE_LEDGER_JOBS.with(|active| {
                    active
                        .borrow_mut()
                        .push((handle, operation.activity().lane));
                });
                let result = work.await;
                ACTIVE_LEDGER_JOBS.with(|active| {
                    active.borrow_mut().retain(|(active, _)| *active != handle);
                });
                let runtime_info = js_runtime_info();
                let runtime_missing = runtime_info.is_null() || runtime_info.is_undefined();
                if result.is_err() && runtime_missing {
                    scheduled_inner.borrow_mut().activity.runtime_lost(epoch);
                } else {
                    let outcome = match result {
                        Ok(()) => Outcome::Succeeded,
                        Err(BridgeError::Call(CallError::Cancelled { .. })) => Outcome::Cancelled,
                        Err(BridgeError::Call(CallError::Stale { .. })) => Outcome::Stale,
                        Err(error) => Outcome::Failed(JobError::Bridge(error)),
                    };
                    scheduled_inner
                        .borrow_mut()
                        .activity
                        .complete(handle, outcome);
                }
                scheduled_repaint.request_repaint();
            });
        }
        submission
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

    fn js_bytes(v: JsValue, field: &'static str) -> Result<Vec<u8>, MarshalError> {
        use wasm_bindgen::JsCast;

        v.dyn_into::<js_sys::Uint8Array>()
            .map_err(|_| MarshalError::NotBytes { field })
            .map(|arr| arr.to_vec())
    }

    fn browser_manifest(
        result: Result<JsValue, JsValue>,
        field: &'static str,
        action: &'static str,
    ) -> Result<BrowserFileManifest, BridgeError> {
        let value = result.map_err(|error| BridgeError::browser(action, jsval_to_string(error)))?;
        let bytes = js_bytes(value, field)?;
        proto::decode_message(&bytes, field).map_err(Into::into)
    }

    fn manifest_texts(manifest: BrowserFileManifest) -> Result<Vec<String>, BridgeError> {
        if manifest.cancelled {
            return Ok(Vec::new());
        }
        if manifest.files.is_empty() && !manifest.failures.is_empty() {
            return Err(BridgeError::browser(
                "reading selected files",
                manifest
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.name, failure.detail))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        manifest
            .files
            .into_iter()
            .map(|file| {
                String::from_utf8(file.content).map_err(|error| {
                    BridgeError::browser("decoding a selected text file", error.to_string())
                })
            })
            .collect()
    }

    async fn download_bytes(
        bytes: Vec<u8>,
        filename: String,
        mime: String,
        action: &'static str,
    ) -> Result<(), BridgeError> {
        js_download_file(bytes, filename, mime)
            .await
            .map_err(|error| BridgeError::browser(action, jsval_to_string(error)))?;
        Ok(())
    }

    async fn download_blob(
        result: Result<pb::FileBlob, CallError>,
        action: &'static str,
    ) -> Result<(), BridgeError> {
        let blob = result?;
        download_bytes(blob.content, blob.filename, blob.mime, action).await
    }

    fn object_uint8(obj: &JsValue, field: &'static str) -> Result<Vec<u8>, MarshalError> {
        let val = js_sys::Reflect::get(obj, &JsValue::from_str(field))
            .map_err(|_| MarshalError::Missing { field })?;
        js_bytes(val, field)
    }

    fn object_f32(obj: &JsValue, field: &'static str) -> Result<Vec<f32>, MarshalError> {
        use wasm_bindgen::JsCast;

        let val = js_sys::Reflect::get(obj, &JsValue::from_str(field))
            .map_err(|_| MarshalError::Missing { field })?;
        let arr = val
            .dyn_into::<js_sys::Uint8Array>()
            .map_err(|_| MarshalError::NotBytes { field })?;
        if arr.byte_length() % 4 != 0 {
            return Err(MarshalError::UnalignedSamples {
                field,
                len: arr.byte_length(),
            });
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
        async fn unary(
            &self,
            operation: pb::Operation,
            method: &'static str,
            request: Vec<u8>,
        ) -> Result<Vec<u8>, TransportError> {
            let job = require_registered_job(operation, method)?;
            let obj = js_call(method, request, Vec::new(), job.id().get())
                .await
                .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
            object_uint8(&obj, "result").map_err(|e| TransportError::new(method, e.to_string()))
        }

        async fn unary_with_buffer(
            &self,
            operation: pb::Operation,
            method: &'static str,
            request: Vec<u8>,
        ) -> Result<(Vec<u8>, Vec<f32>), TransportError> {
            let job = require_registered_job(operation, method)?;
            let marshal = |e: MarshalError| TransportError::new(method, e.to_string());
            if !BRIDGE_TRANSPORT_PERF_LOG {
                let obj = js_call(method, request, Vec::new(), job.id().get())
                    .await
                    .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
                return Ok((
                    object_uint8(&obj, "result").map_err(marshal)?,
                    object_f32(&obj, "ys").map_err(marshal)?,
                ));
            }
            let t0 = js_sys::Date::now();
            let request_bytes = request.len();
            let obj = js_call(method, request, Vec::new(), job.id().get())
                .await
                .map_err(|e| TransportError::new(method, jsval_to_string(e)))?;
            let js_done = js_sys::Date::now();
            let result = object_uint8(&obj, "result").map_err(marshal)?;
            let result_done = js_sys::Date::now();
            let ys = object_f32(&obj, "ys").map_err(marshal)?;
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

    fn marker_budget() -> pb::MarkerBudget {
        pb::MarkerBudget {
            max_markers: crate::marker_store::MarkerBudget::standard().get(),
        }
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
            stream: StreamId,
            session: SessionEpoch,
        ) -> Result<Self, SeedlinkError> {
            let conn = seedlink_connection(&params)?;
            let next_seq = Rc::new(Cell::new(0_u64));
            let retain_s = if params.retain_s > 0.0 {
                params.retain_s
            } else {
                LIVE_RETAIN_SECONDS
            };
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
                stream,
                session,
                next_seq,
                retain_s,
            );
            let on_error = seedlink_on_error(inner.clone(), repaint.clone(), stream, session);
            let on_close = seedlink_on_close(inner, repaint, conn.source, stream, session);

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

    fn seedlink_connection(params: &SeedlinkParams) -> Result<SeedlinkConnection, SeedlinkError> {
        let port = params
            .port
            .trim()
            .parse::<u16>()
            .map_err(|source| SeedlinkError::Port {
                port: params.port.trim().to_owned(),
                source,
            })?;
        let source = format!(
            "{}:{} {}.{} {}",
            params.host.trim(),
            port,
            params.net.trim(),
            params.sta.trim(),
            params.selectors.trim()
        );
        let url = seedlink_proxy_url(params.host.trim(), port);
        let ws = web_sys::WebSocket::new(&url).map_err(|e| SeedlinkError::Connect {
            url,
            detail: jsval_to_string(e),
        })?;
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
                    set_bridge_error(
                        &inner,
                        SeedlinkError::Send {
                            command: command.clone(),
                            detail: jsval_to_string(err),
                        }
                        .into(),
                    );
                    repaint.request_repaint();
                    return;
                }
            }
            set_snuffling_text(&inner, format!("SeedLink connected: {source}"), false);
            repaint.request_repaint();
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_message(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        started_at: f64,
        stream: StreamId,
        session: SessionEpoch,
        next_seq: Rc<Cell<u64>>,
        retain_s: f64,
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
            let records = match records {
                Ok(records) => records,
                Err(error) => {
                    if !close_live_ingest(&inner, stream, session) {
                        return;
                    }
                    set_bridge_error(&inner, SeedlinkError::from(error).into());
                    repaint.request_repaint();
                    spawn_local(async {
                        LIVE_SEEDLINK.with(|slot| {
                            slot.borrow_mut().take();
                        });
                    });
                    return;
                }
            };
            if !records.is_empty() {
                let seq = next_seq.get();
                next_seq.set(seq.wrapping_add(1));
                ingest_live_mseed(
                    inner.clone(),
                    repaint.clone(),
                    source.clone(),
                    records,
                    started_at,
                    stream,
                    session,
                    seq,
                    retain_s,
                );
            }
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_error(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        stream: StreamId,
        session: SessionEpoch,
    ) -> Closure<dyn FnMut(web_sys::ErrorEvent)> {
        Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
            if !close_live_ingest(&inner, stream, session) {
                return;
            }
            let msg = event.message();
            set_bridge_error(
                &inner,
                SeedlinkError::Socket {
                    detail: if msg.is_empty() {
                        "the browser gave no reason".to_owned()
                    } else {
                        msg
                    },
                }
                .into(),
            );
            repaint.request_repaint();
            spawn_local(async {
                LIVE_SEEDLINK.with(|slot| {
                    slot.borrow_mut().take();
                });
            });
        }) as Box<dyn FnMut(_)>)
    }

    fn seedlink_on_close(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        stream: StreamId,
        session: SessionEpoch,
    ) -> Closure<dyn FnMut(web_sys::Event)> {
        Closure::wrap(Box::new(move |_event: web_sys::Event| {
            if close_live_ingest(&inner, stream, session) {
                inner.borrow_mut().live_seedlink_started_at = None;
                set_snuffling_text(&inner, format!("SeedLink disconnected: {source}"), false);
                spawn_local(async {
                    LIVE_SEEDLINK.with(|slot| {
                        slot.borrow_mut().take();
                    });
                });
            }
            repaint.request_repaint();
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

    fn set_bridge_error(inner: &Rc<RefCell<Inner>>, error: BridgeError) {
        inner.borrow_mut().diagnostic = Some(error);
    }

    fn live_ingest_matches(
        state: &LiveIngestState,
        stream: StreamId,
        session: SessionEpoch,
    ) -> bool {
        match state {
            LiveIngestState::Opening {
                stream: current,
                session: current_session,
            }
            | LiveIngestState::ResetInFlight {
                stream: current,
                session: current_session,
            }
            | LiveIngestState::Streaming {
                stream: current,
                session: current_session,
                ..
            }
            | LiveIngestState::Lagged {
                stream: current,
                session: current_session,
                ..
            } => *current == stream && *current_session == session,
            LiveIngestState::Closed => false,
        }
    }

    fn close_live_ingest(
        inner: &Rc<RefCell<Inner>>,
        stream: StreamId,
        session: SessionEpoch,
    ) -> bool {
        let mut state = inner.borrow_mut();
        if !live_ingest_matches(&state.live_ingest, stream, session) {
            return false;
        }
        state.live_ingest = LiveIngestState::Closed;
        state.ingest_mailbox = LiveIngestMailbox::default();
        true
    }

    fn retire_live_source(inner: &Rc<RefCell<Inner>>) -> SessionEpoch {
        LIVE_SEEDLINK.with(|slot| {
            slot.borrow_mut().take();
        });
        let mut state = inner.borrow_mut();
        state.session_epoch = state.session_epoch.wrapping_add(1).max(1);
        state.live_ingest = LiveIngestState::Closed;
        state.ingest_mailbox = LiveIngestMailbox::default();
        state.ingest_job = None;
        state.live_seedlink_started_at = None;
        SessionEpoch(state.session_epoch)
    }

    fn set_snuffling_text(inner: &Rc<RefCell<Inner>>, text: String, show_window: bool) {
        let mut i = inner.borrow_mut();
        install_snuffling_result(
            &mut i,
            SnufflingResult {
                show_window,
                text,
                markers: Vec::new(),
                scene: None,
                origin: String::new(),
                event_state: None,
                markers_dropped: 0,
                marker_budget: crate::marker_store::MarkerBudget::standard().get(),
                nslc_table: Vec::new(),
            },
        );
    }

    fn clear_map_state(inner: &mut Inner) {
        inner.map = None;
        inner.map_request = None;
        inner.map_job = None;
        inner.map_request_gen = inner.map_request_gen.wrapping_add(1);
    }

    fn clear_event_state(inner: &mut Inner) {
        inner.event_state = None;
        inner.active_event_gen = inner.active_event_gen.wrapping_add(1);
        clear_map_state(inner);
    }

    fn install_event_state(inner: &mut Inner, state: ActiveEventResult) {
        inner.event_state = Some(state);
        inner.active_event_gen = inner.active_event_gen.wrapping_add(1);
        inner.station_dists_gen = None;
        clear_map_state(inner);
        invalidate_view_request(inner);
    }

    fn install_snuffling_result(inner: &mut Inner, mut result: SnufflingResult) {
        if let Some(event_state) = result.event_state.take() {
            install_event_state(inner, event_state);
        }
        inner.snuffling = Some(result);
        inner.snuffling_gen = inner.snuffling_gen.wrapping_add(1);
    }

    fn install_scene(inner: &mut Inner, scene: Option<SceneMeta>) {
        inner.scene = Some(scene.unwrap_or_default());
        inner.scene_gen = inner.scene_gen.wrapping_add(1);
        inner.diagnostic = None;
        clear_view(inner);
    }

    fn install_waveform_result(inner: &mut Inner, mut result: pb::LoadBytesResult) {
        install_scene(inner, result.scene.take());
        if let Some(event_state) = result.event_state.take() {
            install_event_state(inner, event_state);
        }
    }

    fn install_bundle_result(inner: &mut Inner, mut result: pb::BundleResult) {
        install_scene(inner, result.scene.take());
        install_bundle_presentation(inner, result.view.take(), result.about.take());
        if let Some(info) = result.info.take() {
            install_snuffling_result(inner, info);
        }
    }

    fn update_active_progress(inner: &Rc<RefCell<Inner>>, done: u64, total: u64, phase: String) {
        ACTIVE_LEDGER_JOBS.with(|active| {
            let job = active
                .borrow()
                .iter()
                .find(|(_, lane)| *lane == crate::activity::Lane::Job)
                .map(|(handle, _)| *handle);
            if let Some(job) = job {
                inner.borrow_mut().activity.update_progress(
                    job,
                    Progress::Count {
                        done,
                        total: Some(total),
                        phase: phase.into(),
                    },
                );
            }
        });
    }

    fn acknowledge_source(file: &pb::BrowserFile) {
        if file.handle_id.is_empty() {
            return;
        }
        js_acknowledge_sources(
            BrowserSourceAckList {
                sources: vec![BrowserSourceAck {
                    handle_id: file.handle_id.clone(),
                    last_modified: file.last_modified,
                    bundle: file.bundle,
                }],
            }
            .encode_to_vec(),
        );
    }

    async fn apply_file_manifest(
        inner: Rc<RefCell<Inner>>,
        manifest: BrowserFileManifest,
        replace_sources: bool,
    ) -> Result<(), BridgeError> {
        if manifest.cancelled {
            return Ok(());
        }
        let total = u64::try_from(manifest.files.len()).unwrap_or(u64::MAX);
        let mut committed = 0_u64;
        let mut failures: Vec<String> = manifest
            .failures
            .iter()
            .map(|failure| format!("{}: {}", failure.name, failure.detail))
            .collect();
        for (index, file) in manifest.files.into_iter().enumerate() {
            update_active_progress(
                &inner,
                u64::try_from(index).unwrap_or(u64::MAX),
                total,
                format!("Loading {}", file.name),
            );
            let loaded = if file.bundle {
                retire_live_source(&inner);
                client()
                    .load_bundle(pb::LoadBundleRequest {
                        url: String::new(),
                        sha256: String::new(),
                        content: file.content.clone(),
                        name: file.name.clone(),
                    })
                    .await
                    .map(|bundle| {
                        install_bundle_result(&mut inner.borrow_mut(), bundle);
                        true
                    })
            } else {
                client()
                    .load_bytes(pb::LoadBytesRequest {
                        name: file.name.clone(),
                        fmt: "detect".to_owned(),
                        append: true,
                        content: file.content.clone(),
                        source_id: file.handle_id.clone(),
                        replace_source: replace_sources && !file.handle_id.is_empty(),
                    })
                    .await
                    .map(|result| {
                        let loaded = result.loaded;
                        install_waveform_result(&mut inner.borrow_mut(), result);
                        loaded
                    })
            };
            match loaded {
                Ok(true) => {
                    committed += 1;
                    acknowledge_source(&file);
                }
                Ok(false) => {}
                Err(error) => failures.push(format!("{}: {error}", file.name)),
            }
        }
        update_active_progress(&inner, total, total, "Finishing file flow".to_owned());
        if failures.is_empty() {
            inner.borrow_mut().diagnostic = None;
            return Ok(());
        }
        let error = BridgeError::browser("loading selected files", failures.join("; "));
        inner.borrow_mut().diagnostic = Some(error.clone());
        if committed == 0 { Err(error) } else { Ok(()) }
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

    const INGEST_MAILBOX_BYTES: usize = 8 * 1024 * 1024;
    const INGEST_MAILBOX_RESIDENCE_S: f64 = 10.0;

    fn note_dropped_batch(mailbox: &mut LiveIngestMailbox, batch: LiveIngestBatch) {
        mailbox.bytes = mailbox.bytes.saturating_sub(batch.content.len());
        mailbox.dropped_seq_first.get_or_insert(batch.seq);
        mailbox.dropped_seq_last = Some(batch.seq);
        mailbox.dropped_bytes = mailbox
            .dropped_bytes
            .saturating_add(u64::try_from(batch.content.len()).unwrap_or(u64::MAX));
    }

    fn enqueue_live_batch(
        inner: &Rc<RefCell<Inner>>,
        stream: StreamId,
        session: SessionEpoch,
        batch: LiveIngestBatch,
    ) {
        let now_s = js_sys::Date::now() / 1000.0;
        let batch_seq = batch.seq;
        let dropped = {
            let mut state = inner.borrow_mut();
            let expected = state.ingest_mailbox.batches.back().map_or_else(
                || match state.live_ingest {
                    LiveIngestState::ResetInFlight { .. } => 1,
                    LiveIngestState::Streaming { next_seq, .. }
                    | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                    LiveIngestState::Opening { .. } | LiveIngestState::Closed => 0,
                },
                |queued| queued.seq.wrapping_add(1),
            );
            if batch_seq != expected {
                state.live_ingest = LiveIngestState::Closed;
                return;
            }
            state.ingest_mailbox.bytes = state
                .ingest_mailbox
                .bytes
                .saturating_add(batch.content.len());
            state.ingest_mailbox.batches.push_back(batch);
            while state
                .ingest_mailbox
                .batches
                .front()
                .is_some_and(|oldest| now_s - oldest.received_s > INGEST_MAILBOX_RESIDENCE_S)
                || state.ingest_mailbox.bytes > INGEST_MAILBOX_BYTES
            {
                let Some(dropped) = state.ingest_mailbox.batches.pop_front() else {
                    break;
                };
                note_dropped_batch(&mut state.ingest_mailbox, dropped);
            }
            let dropped = state.ingest_mailbox.dropped_bytes;
            if dropped > 0 {
                let next_seq = state
                    .ingest_mailbox
                    .batches
                    .back()
                    .map_or(batch_seq.wrapping_add(1), |queued| {
                        queued.seq.wrapping_add(1)
                    });
                state.live_ingest = LiveIngestState::Lagged {
                    stream,
                    session,
                    next_seq,
                    dropped_bytes: dropped,
                };
            }
            dropped
        };
        if dropped > 0 {
            set_snuffling_text(
                inner,
                format!("SeedLink lagged: dropped {dropped} buffered bytes"),
                false,
            );
        }
    }

    fn take_live_mailbox(
        state: &mut Inner,
        stream: StreamId,
        session: SessionEpoch,
        start_time: f64,
        retain_s: f64,
    ) -> Option<IngestMiniSeedRequest> {
        let mut mailbox = std::mem::take(&mut state.ingest_mailbox);
        let Some(first) = mailbox.batches.front() else {
            state.ingest_mailbox = mailbox;
            return None;
        };
        let Some(last) = mailbox.batches.back() else {
            state.ingest_mailbox = mailbox;
            return None;
        };
        let source = first.source.clone();
        let batch_seq_first = first.seq;
        let batch_seq_last = last.seq;
        let mut content = Vec::with_capacity(mailbox.bytes);
        for batch in mailbox.batches.drain(..) {
            content.extend_from_slice(&batch.content);
        }
        Some(IngestMiniSeedRequest {
            source,
            content,
            start_time,
            reset: false,
            retain_seconds: retain_s,
            stream_id: stream.0,
            session_epoch: session.0,
            batch_seq_first,
            batch_seq_last,
            dropped_seq_first: mailbox.dropped_seq_first.unwrap_or(0),
            dropped_seq_last: mailbox.dropped_seq_last.unwrap_or(0),
            dropped_bytes: mailbox.dropped_bytes,
        })
    }

    fn ingest_live_mseed(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        source: String,
        content: Vec<u8>,
        start_time: f64,
        stream: StreamId,
        session: SessionEpoch,
        seq: u64,
        retain_s: f64,
    ) {
        if seq == 0 && content.len() > INGEST_MAILBOX_BYTES {
            set_bridge_error(
                &inner,
                SeedlinkError::from(crate::seedlink::FrameError::Oversized {
                    announced: content.len(),
                    limit: INGEST_MAILBOX_BYTES,
                })
                .into(),
            );
            close_live_ingest(&inner, stream, session);
            spawn_local(async {
                LIVE_SEEDLINK.with(|slot| {
                    slot.borrow_mut().take();
                });
            });
            repaint.request_repaint();
            return;
        }
        let enqueue = {
            let mut state = inner.borrow_mut();
            if !live_ingest_matches(&state.live_ingest, stream, session) {
                return;
            }
            if state.ingest_job.is_some() {
                true
            } else {
                let expected = match state.live_ingest {
                    LiveIngestState::Opening { .. } => 0,
                    LiveIngestState::Streaming { next_seq, .. }
                    | LiveIngestState::Lagged { next_seq, .. } => next_seq,
                    LiveIngestState::ResetInFlight { .. } | LiveIngestState::Closed => return,
                };
                if seq != expected {
                    state.live_ingest = LiveIngestState::Closed;
                    return;
                }
                if seq == 0 {
                    state.live_ingest = LiveIngestState::ResetInFlight { stream, session };
                }
                false
            }
        };
        if enqueue {
            enqueue_live_batch(
                &inner,
                stream,
                session,
                LiveIngestBatch {
                    source,
                    content,
                    seq,
                    received_s: js_sys::Date::now() / 1000.0,
                },
            );
            return;
        }

        let request = IngestMiniSeedRequest {
            source,
            content,
            start_time,
            reset: seq == 0,
            retain_seconds: retain_s,
            stream_id: stream.0,
            session_epoch: session.0,
            batch_seq_first: seq,
            batch_seq_last: seq,
            dropped_seq_first: 0,
            dropped_seq_last: 0,
            dropped_bytes: 0,
        };
        dispatch_live_ingest(
            inner, repaint, request, stream, session, start_time, retain_s,
        );
    }

    fn dispatch_live_ingest(
        inner: Rc<RefCell<Inner>>,
        repaint: Repaint,
        request: IngestMiniSeedRequest,
        stream: StreamId,
        session: SessionEpoch,
        start_time: f64,
        retain_s: f64,
    ) {
        let expected_first = request.batch_seq_first;
        let expected_last = request.batch_seq_last;

        let work_inner = inner.clone();
        let work_repaint = repaint.clone();
        let job_slot = Rc::new(Cell::new(None));
        let work_job_slot = job_slot.clone();
        let submission = submit_job(
            inner.clone(),
            repaint,
            Operation::IngestMiniSeed,
            Some(request.source.clone()),
            Vec::new(),
            async move {
                let parsed = client().ingest_mini_seed(request).await;
                let failed = parsed.is_err();
                let mut acknowledgement_error = None;
                let pending = {
                    let mut state = work_inner.borrow_mut();
                    if state.ingest_job == work_job_slot.get() {
                        state.ingest_job = None;
                    }
                    if !live_ingest_matches(&state.live_ingest, stream, session) {
                        return Ok(());
                    }
                    if let Ok(ack) = &parsed {
                        if ack.stream_id != stream.0
                            || ack.session_epoch != session.0
                            || ack.ack_seq_first != expected_first
                            || ack.ack_seq_last != expected_last
                        {
                            state.live_ingest = LiveIngestState::Closed;
                            acknowledgement_error = Some(BridgeError::browser(
                                "acknowledging SeedLink data",
                                "stale or out-of-order ingest acknowledgement",
                            ));
                        } else {
                            state.scene = ack.scene.clone();
                            state.scene_gen += 1;
                            clear_view(&mut state);
                            if let LiveIngestState::Lagged { dropped_bytes, .. } = state.live_ingest
                            {
                                log::warn!(
                                    "SeedLink resumed after acknowledging a gap of {dropped_bytes} bytes"
                                );
                            }
                            state.live_ingest = LiveIngestState::Streaming {
                                stream,
                                session,
                                next_seq: expected_last.wrapping_add(1),
                            };
                        }
                    } else {
                        state.live_ingest = LiveIngestState::Closed;
                    }
                    if acknowledgement_error.is_none() {
                        take_live_mailbox(&mut state, stream, session, start_time, retain_s)
                    } else {
                        None
                    }
                };
                if let Some(pending) = pending {
                    dispatch_live_ingest(
                        work_inner,
                        work_repaint,
                        pending,
                        stream,
                        session,
                        start_time,
                        retain_s,
                    );
                }
                if let Some(error) = acknowledgement_error {
                    spawn_local(async {
                        LIVE_SEEDLINK.with(|slot| {
                            slot.borrow_mut().take();
                        });
                    });
                    return Err(error);
                }
                if failed {
                    spawn_local(async {
                        LIVE_SEEDLINK.with(|slot| {
                            slot.borrow_mut().take();
                        });
                    });
                }
                parsed.map(|_| ()).map_err(BridgeError::from)
            },
        );
        if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
            job_slot.set(Some(handle));
            inner.borrow_mut().ingest_job = Some(handle);
        }
    }

    fn invalidate_view_request(i: &mut Inner) {
        i.applied = None;
        i.desired = None;
        i.view_job = None;
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
        fn warm_deferred(&self) -> Option<Submission> {
            {
                let mut state = self.inner.borrow_mut();
                if state.warm_requested {
                    return None;
                }
                state.warm_requested = true;
            }
            let inner = self.inner.clone();
            Some(submit_job(
                inner,
                self.repaint.clone(),
                Operation::WarmDeferred,
                None,
                Vec::new(),
                async move {
                    client()
                        .warm_deferred()
                        .await
                        .map(|_| ())
                        .map_err(BridgeError::from)
                },
            ))
        }

        /// Replaces the current pile with the bundled synthetic example.
        pub fn load_example(&self) -> Submission {
            retire_live_source(&self.inner);
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::MakeExample,
                None,
                Vec::new(),
                async move {
                    let parsed = client().make_example().await;
                    let scene = parsed.map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    clear_event_state(&mut i);
                    i.scene = Some(scene);
                    i.scene_gen += 1;
                    clear_view(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Empties the pile and the event context (`File ▸ Close all`), leaving
        /// the blank viewer Qt starts with when given no arguments.
        pub fn empty_scene(&self) -> Submission {
            let repaint = self.repaint.clone();
            js_clear_open_sources();
            retire_live_source(&self.inner);
            {
                let mut i = self.inner.borrow_mut();
                i.diagnostic = None;
                i.snuffling = None;
                clear_event_state(&mut i);
                i.station_dists.clear();
                i.station_dists_gen = None;
                i.live_seedlink_started_at = None;
            }
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                repaint,
                Operation::EmptyScene,
                None,
                Vec::new(),
                async move {
                    let scene = client().empty_scene().await.map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.scene = Some(scene);
                    i.scene_gen += 1;
                    clear_view(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Appends waveform bytes supplied by the frontend to the pile —
        /// snuffler's `open_waveforms` appends too (`pile_viewer.py:1651`); the
        /// boot example is dropped first by the bridge's example-pile rule.
        pub fn load_file(&self, name: String, data: Vec<u8>, fmt: String) -> Submission {
            // A dropped file may be a whole dataset. Deciding on the bytes rather
            // than the extension means a bundle behaves the same however it arrives
            // — dropped, picked, or fetched from the Examples menu.
            if data.starts_with(ZIP_MAGIC) {
                return self.load_bundle_bytes(name, data);
            }
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let detail = Some(name.clone());
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadBytes,
                detail,
                Vec::new(),
                async move {
                    let result = client()
                        .load_bytes(pb::LoadBytesRequest {
                            name,
                            fmt,
                            append: true,
                            content: data,
                            source_id: String::new(),
                            replace_source: false,
                        })
                        .await
                        .map_err(BridgeError::from)?;
                    install_waveform_result(&mut work_inner.borrow_mut(), result);
                    Ok(())
                },
            );
            submission
        }

        /// Replaces the current pile with a waveform fetched from a URL.
        pub fn load_url(&self, url: String, fmt: String) -> Submission {
            retire_live_source(&self.inner);
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let detail = Some(url.clone());
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadUrl,
                detail,
                Vec::new(),
                async move {
                    let scene = client()
                        .load_url(pb::LoadUrlRequest { url, fmt })
                        .await
                        .map_err(BridgeError::from)?;
                    js_clear_open_sources();
                    let mut i = work_inner.borrow_mut();
                    clear_event_state(&mut i);
                    i.scene = Some(scene);
                    i.scene_gen += 1;
                    clear_view(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Fetch the example index, once.
        ///
        /// The menu calls this on every frame it is open, so "once" has to mean
        /// once: a *failed* fetch is remembered too, or a datacenter that is down
        /// would be retried at the frame rate. `force` is the user asking again,
        /// from the retry the menu offers in its place.
        pub fn list_examples(&self, force: bool) -> Option<Submission> {
            {
                let i = self.inner.borrow();
                if let Some(job) = i.examples_job {
                    return Some(Submission::Joined(job));
                }
                if i.examples.is_some() && !force {
                    return None;
                }
            }
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ListExamples,
                None,
                Vec::new(),
                async move {
                    let parsed = client().list_examples().await;
                    let mut i = work_inner.borrow_mut();
                    i.examples_job = None;
                    match parsed {
                        Ok(list) => {
                            i.examples = Some(Ok(list.examples));
                            Ok(())
                        }
                        Err(error) => {
                            let error = BridgeError::from(error);
                            i.examples = Some(Err(error.clone()));
                            Err(error)
                        }
                    }
                },
            );
            if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
                self.inner.borrow_mut().examples_job = Some(handle);
            }
            Some(submission)
        }

        /// Replaces the current pile with a whole dataset fetched as one archive:
        /// waveforms, station metadata and an event catalogue in a single request.
        ///
        /// The station and event results come back with the scene and are applied
        /// through the same path a separate metadata load would use, so the event
        /// markers appear exactly as if the catalogue had been opened by hand.
        pub fn load_bundle(&self, url: String, sha256: String, title: String) -> Submission {
            let request = pb::LoadBundleRequest {
                url,
                sha256,
                content: Vec::new(),
                name: title.clone(),
            };
            self.dispatch_bundle(request, title)
        }

        /// Load an example by slug, fetching the index first if it has not been
        /// read yet. This is what `?example=<slug>` resolves to: a link someone can
        /// hand out that opens the app on a named dataset.
        ///
        /// An unknown slug is an error the user has to see — they followed a link
        /// that promised data — so it lands in the status line rather than only in
        /// the console.
        pub fn open_example(&self, slug: String) -> Submission {
            let bridge = self.clone();
            let inner = self.inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ListExamples,
                Some(slug.clone()),
                slug.as_bytes().to_vec(),
                async move {
                    let known = bridge.inner.borrow().examples.clone();
                    let listed = match known {
                        Some(Ok(examples)) => Ok(examples),
                        _ => client().list_examples().await.map(|list| list.examples),
                    }
                    .map_err(BridgeError::from)?;
                    let found = listed.iter().find(|example| example.slug == slug).cloned();
                    bridge.inner.borrow_mut().examples = Some(Ok(listed));
                    let example = found.ok_or_else(|| BridgeError::UnknownExample { slug })?;
                    let _submission =
                        bridge.load_bundle(example.url, example.sha256, example.title);
                    Ok(())
                },
            );
            submission
        }

        /// The same load for an archive that is already in hand — dropped on the
        /// window, or picked from disk — rather than fetched.
        pub(super) fn load_bundle_bytes(&self, name: String, data: Vec<u8>) -> Submission {
            let request = pb::LoadBundleRequest {
                url: String::new(),
                sha256: String::new(),
                content: data,
                name: name.clone(),
            };
            self.dispatch_bundle(request, name)
        }

        fn dispatch_bundle(&self, request: pb::LoadBundleRequest, label: String) -> Submission {
            retire_live_source(&self.inner);
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadBundle,
                Some(label),
                Vec::new(),
                async move {
                    let bundle = client()
                        .load_bundle(request)
                        .await
                        .map_err(BridgeError::from)?;
                    js_clear_open_sources();
                    install_bundle_result(&mut work_inner.borrow_mut(), bundle);
                    Ok(())
                },
            );
            submission
        }

        /// Render a real pyrocko/matplotlib figure (amplitude spectrum, cake, …)
        /// in Pyodide and store the resulting PNG for the matching figure window.
        /// Keyed by `req.kind`, so several figure windows can be open at once.
        pub fn render_figure(&self, req: FigureRequest) -> Submission {
            let kind = req.kind.clone();
            {
                let mut i = self.inner.borrow_mut();
                let slot = i.figures.entry(kind.clone()).or_default();
                if let Some(job) = slot.job {
                    return Submission::Joined(job);
                }
            }
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let dedup_key = req.encode_to_vec();
            let work_kind = kind.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::RenderFigure,
                Some(kind.clone()),
                dedup_key,
                async move {
                    let parsed = client().render_figure(req).await;
                    let mut i = work_inner.borrow_mut();
                    let slot = i.figures.entry(work_kind).or_default();
                    slot.job = None;
                    let fr = parsed.map_err(BridgeError::from)?;
                    slot.data = FigureData {
                        png: fr.png,
                        error: fr.error,
                    };
                    slot.generation = slot.generation.wrapping_add(1);
                    Ok(())
                },
            );
            if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
                self.inner.borrow_mut().figures.entry(kind).or_default().job = Some(handle);
            }
            submission
        }

        /// Run a built-in snuffling (`MinMax`, `STA/LTA`, …) and store its result.
        pub fn run_snuffling(&self, name: String, params: SnufflingRequestParams) -> Submission {
            let request = pb::NamedSnufflingRequest {
                name,
                params: Some(params),
            };
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let dedup_key = request.encode_to_vec();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::RunSnuffling,
                Some(request.name.clone()),
                dedup_key,
                async move {
                    let parsed = client().run_snuffling(request).await;
                    let mut i = work_inner.borrow_mut();
                    let mut result = parsed.map_err(BridgeError::from)?;
                    if let Some(scene) = result.scene.take() {
                        i.scene = Some(scene);
                        i.scene_gen += 1;
                        clear_view(&mut i);
                    }
                    install_snuffling_result(&mut i, result);
                    Ok(())
                },
            );
            submission
        }

        /// Downloads waveforms from a CORS-enabled FDSN dataselect service and
        /// append them to the pile (browser fetch; pyrocko parses).
        pub fn download_waveforms(&self, params: WaveformDownloadRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::DownloadWaveforms,
                None,
                Vec::new(),
                async move {
                    let scene = client()
                        .download_waveforms(params)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.scene = Some(scene);
                    i.scene_gen += 1;
                    clear_view(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Computes blockwise RMS traces, replacing the previous RMS run.
        pub fn rms_traces(&self, params: SnufflingRequestParams) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let dedup_key = params.encode_to_vec();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::RmsTraces,
                None,
                dedup_key,
                async move {
                    let mut result = client()
                        .rms_traces(params)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    if let Some(scene) = result.scene.take() {
                        i.scene = Some(scene);
                        i.scene_gen += 1;
                        clear_view(&mut i);
                    }
                    install_snuffling_result(&mut i, result);
                    Ok(())
                },
            );
            submission
        }

        /// Search a CORS-enabled FDSN event service; results become event markers.
        /// The largest-magnitude hit becomes the active event, so this forces a
        /// re-dispatch for the distance/azimuth sort to recompute against it.
        pub fn catalog_search(&self, params: CatalogRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::CatalogSearch,
                None,
                Vec::new(),
                async move {
                    let result = client()
                        .catalog_search(params)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    install_snuffling_result(&mut i, result);
                    invalidate_view_request(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Let the user pick a custom `.nd` model file ("Add Model"); the picked
        /// name is stored for the frontend to add to the Cake Phase dropdown.
        pub fn add_cake_model(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::AddCakeModel,
                None,
                Vec::new(),
                async move {
                    let job = require_registered_job(Operation::AddCakeModel, "addCakeModel")
                        .map_err(CallError::from)?;
                    let value = js_add_cake_model(job.id().get()).await.map_err(|error| {
                        BridgeError::browser("adding an earth model", jsval_to_string(error))
                    })?;
                    if let Some(name) = value.as_string()
                        && !name.is_empty()
                    {
                        work_inner.borrow_mut().pending_cake_model = Some(name);
                    }
                    Ok(())
                },
            );
            submission
        }

        /// Mount a Green's-functions store by fetching its files from an HTTP base
        /// URL into the Pyodide filesystem (no directory picker) and register it as
        /// `GF Store: <id>`. Errors surface in the snuffling text window.
        pub fn load_gf_store_url(&self, url: String) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadGfStoreUrl,
                Some(url.clone()),
                Vec::new(),
                async move {
                    let label = client()
                        .load_gf_store_url(pb::StringRequest { value: url })
                        .await
                        .map_err(BridgeError::from)?;
                    if !label.value.is_empty() {
                        work_inner.borrow_mut().pending_cake_model = Some(label.value);
                    }
                    Ok(())
                },
            );
            submission
        }

        /// Mount a Green's-functions store directory (`NativeFS`, via the File
        /// System Access API) and add its embedded 1D earth model to the Cake
        /// Phase dropdown as `GF Store: <id>`. Errors (unsupported browser, not a
        /// store) surface in the snuffling text window.
        #[cfg(feature = "gf")]
        pub fn add_gf_store(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::MountGfStore,
                None,
                Vec::new(),
                async move {
                    let job = require_registered_job(Operation::MountGfStore, "addGfStore")
                        .map_err(CallError::from)?;
                    let value = js_add_gf_store(job.id().get()).await.map_err(|error| {
                        BridgeError::browser("adding a GF store", jsval_to_string(error))
                    })?;
                    if let Some(name) = value.as_string()
                        && !name.is_empty()
                    {
                        work_inner.borrow_mut().pending_cake_model = Some(name);
                    }
                    Ok(())
                },
            );
            submission
        }

        /// Execute a ':' command line (goto / hide / unhide / reduce / scaling). A
        /// goto stores a pending time the session jumps to; a visibility change swaps
        /// the scene in place (no refit) and forces the view to re-dispatch. Either
        /// way the outcome lands in `command_outcome` for the command line to act on.
        pub fn run_command(&self, request: CommandRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::RunCommand,
                Some(request.line.clone()),
                Vec::new(),
                async move {
                    let result = client()
                        .run_command(request)
                        .await
                        .map_err(BridgeError::from)?;
                    // Scoped so the borrow ends before `request_repaint()`: `Inner` is
                    // the same `RefCell` the render loop reads through its own `Rc`, and
                    // this is an async block, so a guard still alive at the repaint — or
                    // at any `.await` a later edit puts below — is a `BorrowMutError`
                    // panic on whichever frame or bridge result lands next.
                    {
                        let mut i = work_inner.borrow_mut();
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
                    Ok(())
                },
            );
            submission
        }

        /// Fetch station coordinates (FDSN station service) to enable distance /
        /// azimuth sorting; forces the view to re-dispatch so the sort recomputes.
        pub fn fetch_stations(&self, params: StationRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::FetchStations,
                None,
                Vec::new(),
                async move {
                    let result = client()
                        .fetch_stations(params)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    install_snuffling_result(&mut i, result);
                    invalidate_view_request(&mut i); // re-dispatch so distance/azimuth sort recomputes
                    Ok(())
                },
            );
            submission
        }

        /// Fetch instrument responses (`StationXML`) so Deconvolution can run;
        /// forces a re-dispatch so the deconvolved view recomputes.
        pub fn fetch_responses(&self, params: StationRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::FetchResponses,
                None,
                Vec::new(),
                async move {
                    let result = client()
                        .fetch_responses(params)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    install_snuffling_result(&mut i, result);
                    invalidate_view_request(&mut i); // re-dispatch so deconvolution applies
                    Ok(())
                },
            );
            submission
        }

        /// Reports whether a live `SeedLink` socket is active.
        #[must_use]
        pub fn seedlink_active(&self) -> bool {
            let _scene_gen = self.inner.borrow().scene_gen;
            LIVE_SEEDLINK.with(|slot| slot.borrow().is_some())
        }

        /// Starts a live `SeedLink` source and replaces the current pile.
        pub fn start_seedlink(&self, params: SeedlinkParams) {
            let repaint = self.repaint.clone();
            let session = retire_live_source(&self.inner);
            let stream = {
                let mut state = self.inner.borrow_mut();
                state.next_stream_id = state.next_stream_id.wrapping_add(1).max(1);
                let stream = StreamId(state.next_stream_id);
                state.live_ingest = LiveIngestState::Opening { stream, session };
                stream
            };
            let started_at = js_sys::Date::now() / 1000.0;
            let follow_s = if params.follow_s > 0.0 {
                params.follow_s
            } else {
                LIVE_FOLLOW_SECONDS
            };
            match LiveSeedlinkHandle::connect(
                self.inner.clone(),
                repaint.clone(),
                params,
                started_at,
                stream,
                session,
            ) {
                Ok(handle) => {
                    js_clear_open_sources();
                    LIVE_SEEDLINK.with(|slot| {
                        *slot.borrow_mut() = Some(handle);
                    });
                    let mut i = self.inner.borrow_mut();
                    i.live_seedlink_started_at = Some(started_at);
                    i.live_seedlink_follow_s = follow_s;
                    i.scene = Some(SceneMeta {
                        tmin: started_at,
                        tmax: started_at + follow_s,
                        tracks: Vec::new(),
                    });
                    i.scene_gen += 1;
                    i.diagnostic = None;
                    i.snuffling = None;
                    clear_event_state(&mut i);
                    i.station_dists.clear();
                    i.station_dists_gen = None;
                    clear_view(&mut i);
                }
                Err(err) => {
                    let mut i = self.inner.borrow_mut();
                    i.live_seedlink_started_at = None;
                    i.live_ingest = LiveIngestState::Closed;
                    i.diagnostic = Some(err.into());
                }
            }
            repaint.request_repaint();
        }

        /// Stops live `SeedLink` acquisition while retaining received samples.
        pub fn stop_seedlink(&self) {
            retire_live_source(&self.inner);
            set_snuffling_text(&self.inner, "SeedLink stopped".to_owned(), false);
        }

        /// Open local waveform files via a browser picker and append them.
        pub fn open_files(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadBytes,
                Some("selected files".to_owned()),
                Vec::new(),
                async move {
                    let manifest =
                        browser_manifest(js_open_files().await, "open_files", "opening files")?;
                    apply_file_manifest(work_inner, manifest, false).await
                },
            );
            submission
        }

        /// Recursively load every waveform file under a picked directory
        /// (Chromium File System Access); appends them to the pile.
        pub fn open_directory(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadBytes,
                Some("selected directory".to_owned()),
                Vec::new(),
                async move {
                    let manifest = browser_manifest(
                        js_open_directory().await,
                        "open_directory",
                        "opening a directory",
                    )?;
                    apply_file_manifest(work_inner, manifest, false).await
                },
            );
            submission
        }

        /// Reload the Python snuffling code (bridge.py) in the worker, preserving
        /// the loaded pile (snuffler's Shift+R). Forces a view re-fetch since the
        /// reload resets Python's view cache; the scene (track list) is unchanged.
        pub fn reload_snufflings(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ReloadSnufflings,
                None,
                Vec::new(),
                async move {
                    let job =
                        require_registered_job(Operation::ReloadSnufflings, "reloadSnufflings")
                            .map_err(CallError::from)?;
                    js_reload_snufflings(job.id().get())
                        .await
                        .map_err(|error| {
                            BridgeError::browser("reloading snufflings", jsval_to_string(error))
                        })?;
                    invalidate_view_request(&mut work_inner.borrow_mut());
                    Ok(())
                },
            );
            submission
        }

        /// Re-read any opened files whose on-disk mtime changed and replace their
        /// traces (snuffler's "r").
        ///
        /// Successful sources are installed even when another source in the same
        /// batch fails.
        /// Those failures remain visible through the recoverable diagnostic slot.
        pub fn reload_files(&self) -> Submission {
            let inner = self.inner.clone();
            inner.borrow_mut().diagnostic = None;
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadBytes,
                Some("modified files".to_owned()),
                Vec::new(),
                async move {
                    let manifest = browser_manifest(
                        js_reload_files().await,
                        "reload_files",
                        "reloading files",
                    )?;
                    apply_file_manifest(work_inner, manifest, true).await
                },
            );
            submission
        }

        /// Qt's download Save trigger: the loaded waveforms as one `MiniSEED`
        /// download plus the station table as a pyrocko station file.
        /// A refusal remains a typed recoverable diagnostic, so either successful
        /// half still downloads and the viewer stays operational.
        pub fn save_waveforms_and_stations(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ExportWaveforms,
                Some("waveforms and stations".to_owned()),
                Vec::new(),
                async move {
                    // Both halves run even when the first fails: one refused file
                    // must not cost the user the other download.
                    // The first failure takes the diagnostic slot.
                    let failed = [
                        download_blob(client().export_waveforms().await, "downloading waveforms")
                            .await,
                        download_blob(client().export_stations().await, "downloading stations")
                            .await,
                    ]
                    .into_iter()
                    .find_map(Result::err);
                    work_inner.borrow_mut().diagnostic = failed.clone();
                    if let Some(error) = failed {
                        return Err(error);
                    }
                    Ok(())
                },
            );
            submission
        }

        /// The waveform half alone — the live window's "save the buffer as
        /// `MiniSEED`" (a stream rarely has a station table to save with it).
        /// A refusal surfaces as a recoverable diagnostic.
        pub fn save_waveforms(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ExportWaveforms,
                Some("waveforms".to_owned()),
                Vec::new(),
                async move {
                    let result =
                        download_blob(client().export_waveforms().await, "downloading waveforms")
                            .await;
                    match result {
                        Ok(()) => {
                            work_inner.borrow_mut().diagnostic = None;
                            Ok(())
                        }
                        Err(error) => {
                            work_inner.borrow_mut().diagnostic = Some(error.clone());
                            Err(error)
                        }
                    }
                },
            );
            submission
        }

        /// Serializes markers through pyrocko and downloads the resulting text.
        /// A failure surfaces as a typed recoverable diagnostic.
        pub fn save_markers(&self, markers: MarkerFile) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::MarkersToText,
                Some("markers.txt".to_owned()),
                Vec::new(),
                async move {
                    let result = match client().markers_to_text(markers).await {
                        Ok(text) => {
                            download_bytes(
                                text.value.into_bytes(),
                                "markers.txt".to_owned(),
                                "text/plain".to_owned(),
                                "downloading markers",
                            )
                            .await
                        }
                        Err(error) => Err(error.into()),
                    };
                    match result {
                        Ok(()) => {
                            work_inner.borrow_mut().diagnostic = None;
                            Ok(())
                        }
                        Err(error) => {
                            work_inner.borrow_mut().diagnostic = Some(error.clone());
                            Err(error)
                        }
                    }
                },
            );
            submission
        }

        /// Trigger a browser download of encoded image bytes (PNG export).
        pub fn save_image_png(&self, bytes: Vec<u8>, filename: String) -> Submission {
            let detail = Some(filename.clone());
            let inner = self.inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ExportPng,
                detail,
                Vec::new(),
                async move {
                    js_save_png(bytes, filename).await.map_err(|error| {
                        BridgeError::browser("saving a PNG", jsval_to_string(error))
                    })?;
                    Ok(())
                },
            );
            submission
        }

        /// Trigger a browser download of an SVG document (vector image export).
        pub fn save_image_svg(&self, svg: String, filename: String) -> Submission {
            let detail = Some(filename.clone());
            let inner = self.inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::ExportSvg,
                detail,
                Vec::new(),
                async move {
                    js_save_svg(svg, filename).await.map_err(|error| {
                        BridgeError::browser("saving an SVG", jsval_to_string(error))
                    })?;
                    Ok(())
                },
            );
            submission
        }

        /// Load a marker file via a browser picker; results land as markers.
        pub fn load_markers(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadMarkers,
                None,
                Vec::new(),
                async move {
                    let texts = manifest_texts(browser_manifest(
                        js_load_markers().await,
                        "markers",
                        "loading markers",
                    )?)?;
                    let Some(text) = texts.into_iter().next() else {
                        return Ok(());
                    };
                    let loaded = client()
                        .load_markers(pb::LoadMarkersRequest {
                            text,
                            marker_budget: Some(marker_budget()),
                        })
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.diagnostic = None;
                    let n = loaded.markers.len();
                    install_snuffling_result(
                        &mut i,
                        SnufflingResult {
                            show_window: true,
                            text: format!("loaded {n} marker(s)"),
                            markers: loaded.markers,
                            scene: None,
                            origin: String::new(),
                            event_state: loaded.event_state,
                            markers_dropped: loaded.markers_dropped,
                            marker_budget: loaded.marker_budget,
                            nslc_table: loaded.nslc_table,
                        },
                    );
                    Ok(())
                },
            );
            submission
        }

        /// Pick and merge local `StationXML` files (instrument responses for
        /// deconvolution + station coordinates for sorting) using the same parsing
        /// as fetched `FDSN` responses.
        pub fn load_station_xml(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadStationXml,
                None,
                Vec::new(),
                async move {
                    let texts = manifest_texts(browser_manifest(
                        js_load_station_xml().await,
                        "stationxml",
                        "loading StationXML",
                    )?)?;
                    if texts.is_empty() {
                        return Ok(());
                    }
                    let result = client()
                        .load_station_xml(pb::LoadStationXmlRequest { texts })
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.diagnostic = None;
                    install_snuffling_result(&mut i, result);
                    Ok(())
                },
            );
            submission
        }

        /// Pick a local pyrocko station file and load its station coordinates
        /// (distance/azimuth sorting, cake, map) via `model.load_stations`.
        pub fn load_stations(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadStations,
                None,
                Vec::new(),
                async move {
                    let texts = manifest_texts(browser_manifest(
                        js_load_stations().await,
                        "stations",
                        "loading stations",
                    )?)?;
                    if texts.is_empty() {
                        return Ok(());
                    }
                    let result = client()
                        .load_stations(pb::LoadStationsRequest {
                            text: texts.join("\n"),
                        })
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.diagnostic = None;
                    install_snuffling_result(&mut i, result);
                    Ok(())
                },
            );
            submission
        }

        /// Pick a local pyrocko event file and load it: event markers + the active
        /// event (largest magnitude) for distance/azimuth sorting, which forces a
        /// re-dispatch so that sort recomputes against the new active event.
        pub fn load_events(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::LoadEvents,
                None,
                Vec::new(),
                async move {
                    let texts = manifest_texts(browser_manifest(
                        js_load_events().await,
                        "events",
                        "loading events",
                    )?)?;
                    let Some(text) = texts.into_iter().next() else {
                        return Ok(());
                    };
                    let result = client()
                        .load_events(pb::LoadEventsRequest {
                            text,
                            marker_budget: Some(marker_budget()),
                        })
                        .await
                        .map_err(BridgeError::from)?;
                    let mut i = work_inner.borrow_mut();
                    i.diagnostic = None;
                    install_snuffling_result(&mut i, result);
                    invalidate_view_request(&mut i);
                    Ok(())
                },
            );
            submission
        }

        /// Fetch station + event coordinates for the Map overlay.
        pub fn fetch_map_data(&self, request: MapRequest) -> Submission {
            let inner = self.inner.clone();
            let request_gen = {
                let mut i = inner.borrow_mut();
                if let Some(job) = i.map_job
                    && i.map_request.as_ref() == Some(&request)
                {
                    return Submission::Joined(job);
                }
                let changed = i.map_request.as_ref() != Some(&request);
                i.map_request = Some(request.clone());
                i.map_request_gen = i.map_request_gen.wrapping_add(1);
                if changed {
                    i.map = None;
                }
                i.map_request_gen
            };
            let work_inner = inner.clone();
            let dedup_key = request.encode_to_vec();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::MapData,
                None,
                dedup_key,
                async move {
                    let result = client().map_data(request).await;
                    let mut i = work_inner.borrow_mut();
                    if i.map_request_gen == request_gen {
                        i.map_job = None;
                        i.map = Some(result.map_err(BridgeError::from)?);
                    }
                    Ok(())
                },
            );
            if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
                self.inner.borrow_mut().map_job = Some(handle);
            }
            submission
        }

        /// Ensure the per-station distances are current for the given snuffling
        /// generation (the active event changes with each catalog/event load). A
        /// no-op if already fetched for this generation or a fetch is in flight.
        pub fn ensure_station_distances(&self, snuffling_gen: u64) -> Option<Submission> {
            let token;
            {
                let i = self.inner.borrow();
                token = (snuffling_gen, i.active_event_gen);
                if let Some(job) = i.station_dists_job {
                    return Some(Submission::Joined(job));
                }
                if i.station_dists_gen == Some(token) {
                    return None;
                }
            }
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::StationDistances,
                None,
                format!("{}:{}", token.0, token.1).into_bytes(),
                async move {
                    let parsed = client().station_distances().await;
                    let mut i = work_inner.borrow_mut();
                    i.station_dists_job = None;
                    let list = parsed.map_err(BridgeError::from)?;
                    i.station_dists_gen = Some(token);
                    i.station_dists.clear();
                    for station in list.stations {
                        i.station_dists
                            .insert((station.net, station.sta), station.distance_deg);
                    }
                    Ok(())
                },
            );
            if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
                self.inner.borrow_mut().station_dists_job = Some(handle);
            }
            Some(submission)
        }

        /// Polls watched files without showing a loading status.
        ///
        /// Successful changes install immediately; source failures still surface
        /// because otherwise their earlier batch peers would change invisibly.
        pub fn watch_poll(&self) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::WatchPoll,
                None,
                Vec::new(),
                async move {
                    let manifest =
                        browser_manifest(js_reload_files().await, "watch_poll", "watching files")?;
                    apply_file_manifest(work_inner, manifest, true).await
                },
            );
            submission
        }

        /// Sets or clears the active event and installs Python's canonical registry.
        pub fn set_active_event(&self, req: SetActiveEventRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::SetActiveEvent,
                None,
                Vec::new(),
                async move {
                    let state = client()
                        .set_active_event(req)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut current = work_inner.borrow_mut();
                    current.diagnostic = None;
                    install_event_state(&mut current, state);
                    Ok(())
                },
            );
            submission
        }

        /// Replaces one canonical event after an inline marker-table edit.
        pub fn update_event(&self, event: EventSnapshot) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::UpdateEvent,
                None,
                Vec::new(),
                async move {
                    let state = client()
                        .update_event(event)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut current = work_inner.borrow_mut();
                    current.diagnostic = None;
                    install_event_state(&mut current, state);
                    Ok(())
                },
            );
            submission
        }

        /// Removes canonical events whose final frontend marker was deleted.
        pub fn remove_events(&self, request: RemoveEventsRequest) -> Submission {
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let submission = submit_job(
                inner,
                self.repaint.clone(),
                Operation::RemoveEvents,
                None,
                Vec::new(),
                async move {
                    let state = client()
                        .remove_events(request)
                        .await
                        .map_err(BridgeError::from)?;
                    let mut current = work_inner.borrow_mut();
                    current.diagnostic = None;
                    install_event_state(&mut current, state);
                    Ok(())
                },
            );
            submission
        }

        /// Submit every tile the map wants on screen right now.
        ///
        /// This replaces the pending queue rather than adding to it, which is what
        /// makes a zoom or a pan cheap: tiles the view has moved away from and
        /// that no batch has started yet are simply dropped, and the survivors are
        /// re-ordered by the priority the view just scored them with. Tiles
        /// already fetched, in flight, or known to have failed are skipped, so
        /// calling this every frame costs a filter over a screenful of keys.
        pub fn request_tiles(&self, wanted: Vec<TileRequest>) {
            {
                let mut i = self.inner.borrow_mut();
                let mut queue: Vec<TileRequest> = wanted
                    .into_iter()
                    .filter(|t| !i.tiles.contains_key(&(t.z, t.x, t.y)))
                    .collect();
                queue.sort_by_key(|t| t.priority);
                i.tile_queue = queue;
            }
            self.pump_tiles();
        }

        /// Start batches until the queue empties or the in-flight cap is reached.
        fn pump_tiles(&self) {
            loop {
                let batch = {
                    let mut i = self.inner.borrow_mut();
                    if i.tile_batches >= TILE_BATCHES_IN_FLIGHT || i.tile_queue.is_empty() {
                        return;
                    }
                    let take = i.tile_queue.len().min(TILE_BATCH);
                    let batch: Vec<TileRequest> = i.tile_queue.drain(..take).collect();
                    for tile in &batch {
                        i.tiles
                            .insert((tile.z, tile.x, tile.y), TileState::InFlight);
                    }
                    i.tile_batches += 1;
                    batch
                };
                self.spawn_tile_batch(batch);
            }
        }

        /// Fetch and decode one batch in the browser, filing each tile as it lands.
        fn spawn_tile_batch(&self, batch: Vec<TileRequest>) {
            let inner = self.inner.clone();
            let repaint = self.repaint.clone();
            let me = self.clone();
            let epoch = self.inner.borrow().tile_epoch;
            let keys: Vec<(u8, u32, u32)> = batch.iter().map(|t| (t.z, t.x, t.y)).collect();
            let urls = js_sys::Array::new();
            for tile in &batch {
                urls.push(&JsValue::from_str(&tile.url));
            }

            spawn_local(async move {
                let on_tile = {
                    let inner = inner.clone();
                    let repaint = repaint.clone();
                    let keys = keys.clone();
                    // Tile dimensions arrive as JS numbers that are small
                    // non-negative pixel counts.
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "tile width/height are small non-negative integers"
                    )]
                    Closure::<dyn FnMut(f64, f64, f64, js_sys::Uint8Array)>::new(
                        move |index: f64, width: f64, height: f64, rgba: js_sys::Uint8Array| {
                            let Some(&key) = keys.get(index as usize) else {
                                return;
                            };
                            let (width, height) = (width as u32, height as u32);
                            let bytes = rgba.to_vec();
                            let decoded = width > 0
                                && height > 0
                                && bytes.len() == (width as usize) * (height as usize) * 4;
                            {
                                let mut i = inner.borrow_mut();
                                // The basemap changed while this tile was in
                                // flight; it now holds the wrong provider's
                                // imagery for its key.
                                if i.tile_epoch != epoch {
                                    return;
                                }
                                i.tiles.insert(
                                    key,
                                    if decoded {
                                        TileState::Ready(width, height, bytes)
                                    } else {
                                        TileState::Failed
                                    },
                                );
                            }
                            repaint.request_repaint();
                        },
                    )
                };
                let delivered = js_fetch_tiles(
                    urls,
                    on_tile.as_ref().unchecked_ref::<js_sys::Function>().clone(),
                )
                .await;
                drop(on_tile);
                {
                    let mut i = inner.borrow_mut();
                    i.tile_batches = i.tile_batches.saturating_sub(1);
                    // The batch as a whole can reject before every callback fired
                    // (the module failed to load, say). Anything still marked
                    // in-flight has no other way to be retired, and would block
                    // its key from ever being requested again.
                    if i.tile_epoch == epoch && delivered.is_err() {
                        for key in keys {
                            if matches!(i.tiles.get(&key), Some(TileState::InFlight)) {
                                i.tiles.insert(key, TileState::Failed);
                            }
                        }
                    }
                }
                repaint.request_repaint();
                me.pump_tiles();
            });
        }

        /// Request a view for the given params, coalescing rapid changes so that
        /// at most one Python call is in flight at a time.
        pub fn ensure_view(&self, params: ViewParams) -> Option<Submission> {
            let repaint = self.repaint.clone();
            {
                let mut i = self.inner.borrow_mut();
                if i.applied.as_ref() == Some(&params) {
                    if i.view_job.is_none() {
                        i.desired = None;
                    } else {
                        i.desired = Some(params);
                        repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    }
                    return i.view_job.as_ref().map(|(job, _)| Submission::Joined(*job));
                }
                let joined = i
                    .view_job
                    .as_ref()
                    .filter(|(_, active)| active == &params)
                    .map(|(job, _)| *job);
                if let Some(job) = joined {
                    i.desired = None;
                    repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    return Some(Submission::Joined(job));
                }
                i.desired = Some(params);
                if let Some((job, _)) = i.view_job {
                    repaint.request_repaint_after(std::time::Duration::from_millis(17));
                    return Some(Submission::Joined(job));
                }
                let now_s = js_sys::Date::now() / 1000.0;
                if now_s < i.view_retry_after_s {
                    repaint.request_repaint_after(std::time::Duration::from_millis(500));
                    return None;
                }
            }
            self.dispatch_view()
        }

        fn dispatch_view(&self) -> Option<Submission> {
            let repaint = self.repaint.clone();
            let (params, canonical_params, request_gen) = {
                let mut i = self.inner.borrow_mut();
                if let Some((job, _)) = i.view_job {
                    return Some(Submission::Joined(job));
                }
                let Some(mut params) = i.desired.take() else {
                    return None;
                };
                if i.applied.as_ref() == Some(&params) {
                    return None;
                }
                let canonical_params = params.clone();
                i.next_view_sequence = i.next_view_sequence.wrapping_add(1).max(1);
                params.view_sequence = i.next_view_sequence;
                (params, canonical_params, i.view_request_gen)
            };
            repaint.request_repaint_after(std::time::Duration::from_millis(17));
            let inner = self.inner.clone();
            let work_inner = inner.clone();
            let me = self.clone();
            let work_params = params.clone();
            let work_canonical_params = canonical_params.clone();
            let submission = submit_job(
                inner,
                repaint,
                Operation::GetView,
                None,
                params.encode_to_vec(),
                async move {
                    let t0 = js_sys::Date::now();
                    let parsed = client()
                        .get_view(work_params.clone())
                        .await
                        .map(|(meta, ys)| ViewData { meta, ys });
                    let request_ms = js_sys::Date::now() - t0;
                    let outcome = {
                        let mut i = work_inner.borrow_mut();
                        if i.view_request_gen == request_gen {
                            match parsed {
                                Ok(vd) => {
                                    if vd.meta.view_sequence != work_params.view_sequence
                                        || vd.meta.session_generation < i.python_session_generation
                                    {
                                        i.desired = Some(work_canonical_params.clone());
                                        Err(BridgeError::browser(
                                            "applying a waveform view",
                                            "stale view reply",
                                        ))
                                    } else {
                                        log_view_debug(&work_params, &vd, request_ms);
                                        i.python_session_generation = vd.meta.session_generation;
                                        i.view_last_error = None;
                                        i.view_retry_after_s = 0.0;
                                        if i.desired
                                            .as_ref()
                                            .is_none_or(|desired| desired == &work_canonical_params)
                                        {
                                            i.view = Some(vd);
                                            i.view_epoch += 1;
                                            i.applied = Some(work_canonical_params.clone());
                                            if i.desired.as_ref() == Some(&work_canonical_params) {
                                                i.desired = None;
                                            }
                                        }
                                        Ok(())
                                    }
                                }
                                Err(error) => {
                                    let error = BridgeError::from(error);
                                    i.view_last_error = Some(error.clone());
                                    i.view_retry_after_s = js_sys::Date::now() / 1000.0 + 0.5;
                                    i.desired = Some(work_canonical_params.clone());
                                    Err(error)
                                }
                            }
                        } else {
                            Ok(())
                        }
                    };
                    if work_inner
                        .borrow()
                        .view_job
                        .as_ref()
                        .is_some_and(|(_, active)| active == &work_canonical_params)
                    {
                        work_inner.borrow_mut().view_job = None;
                    }
                    let should_warm = outcome.is_ok();
                    me.maybe_redispatch();
                    if should_warm {
                        let _submission = me.warm_deferred();
                    }
                    outcome
                },
            );
            if let Submission::Accepted(handle) | Submission::Joined(handle) = submission {
                self.inner.borrow_mut().view_job = Some((handle, canonical_params));
            }
            Some(submission)
        }

        fn maybe_redispatch(&self) {
            let need = {
                let mut i = self.inner.borrow_mut();
                if i.view_job.is_some() {
                    false
                } else if js_sys::Date::now() / 1000.0 < i.view_retry_after_s {
                    self.repaint
                        .request_repaint_after(std::time::Duration::from_millis(500));
                    false
                } else if i.desired.is_some() && i.applied.as_ref() != i.desired.as_ref() {
                    true
                } else {
                    i.desired = None;
                    false
                }
            };
            if need {
                let _submission = self.dispatch_view();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BundleAbout, BundleView, Inner, install_bundle_presentation};

    #[test]
    fn bundle_presentation_uses_the_installed_scene_generation() {
        let mut inner = Inner {
            scene_gen: 7,
            ..Default::default()
        };

        install_bundle_presentation(
            &mut inner,
            Some(BundleView {
                sort: "distance".to_owned(),
                subsort: "grouped".to_owned(),
            }),
            Some(BundleAbout {
                title: "Changed bundle".to_owned(),
                body: "Updated note".to_owned(),
            }),
        );

        assert_eq!(
            inner.pending_view.as_ref().map(|view| view.sort.as_str()),
            Some("distance")
        );
        assert!(inner.dataset_note_unseen);
        assert_eq!(
            inner
                .dataset_note
                .as_ref()
                .map(|(generation, about)| (*generation, about.title.as_str())),
            Some((7, "Changed bundle"))
        );
    }
}
