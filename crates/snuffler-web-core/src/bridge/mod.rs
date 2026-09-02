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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

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
    LIVE_FOLLOW_SECONDS, LIVE_RETAIN_SECONDS, LiveSeedlinkSession, SeedlinkParams,
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
    ActiveEventResult, BundleAbout, BundleView, CakeSnufflingParams, CatalogRequest,
    CommandRequest, EventKagan, EventSnapshot, ExampleDataset, FigureRequest, MapData, MapRequest,
    MarkerFile, MarkerFileEntry, MomentTensor, Nslc, PolarizationSnufflingParams,
    RemoveEventsRequest, RmsSnufflingParams, RuntimeInfo, SceneMeta, SeisbenchSnufflingParams,
    SeismosizerSnufflingParams, SetActiveEventRequest, SnufMarker, SnufflingParams,
    SnufflingRequestParams, SnufflingResult, SnufflingSpan, SpectrogramSnufflingParams,
    SpectrumSnufflingParams, StaltaSnufflingParams, StationRequest, TraceBox, ViewMeta, ViewParams,
    ViewSpan, ViewTrack, WaveformDownloadRequest,
};
// Wire shapes only the wasm bridge implementation touches; not part of the
// crate's app-facing surface.
#[cfg(target_arch = "wasm32")]
pub(crate) use proto::pb::{
    BrowserFileLoadPage, BrowserSourceAck, BrowserSourceAckList, IngestMiniSeedRequest,
};

/// Local-file header of a zip archive, which is how a dataset bundle is
/// recognized wherever one can arrive.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// The job list shared with the per-frame activity UI.
type ActivityJobs = Rc<Vec<(JobHandle, JobRecord)>>;

#[derive(Default)]
struct Inner {
    activity: ActivityLedger,
    /// The last runtime epoch observed by [`web::sync_runtime`]. A change with no
    /// RPC in flight is the only evidence of an idle worker death, so the sync
    /// poll compares against this to report the loss exactly once.
    runtime_epoch: Option<crate::activity::RuntimeEpoch>,
    /// The record snapshot handed to the per-frame activity UI, keyed by the
    /// ledger revision it was built from, so idle frames share one allocation.
    activity_cache: Option<(u64, ActivityJobs)>,
    /// Latest recoverable failure that should remain visible without making the
    /// bridge operationally unavailable.
    diagnostic: Option<BridgeError>,
    scene: Option<SceneMeta>,
    /// Incremented every time a new scene is loaded, so the frontend can reset its
    /// view window when (and only when) the data actually changes.
    scene_gen: u64,
    view: Option<Arc<ViewData>>,
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
    scientific_tools_requested: bool,
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

    /// Returns runtime health and a shared snapshot of all visible jobs,
    /// rebuilt only when the ledger revision moved since the previous frame.
    #[must_use]
    pub fn activity_snapshot(&self) -> (RuntimeState, ActivityJobs) {
        #[cfg(target_arch = "wasm32")]
        web::sync_runtime(&self.inner);
        let mut borrow = self.inner.borrow_mut();
        let state = &mut *borrow;
        let revision = state.activity.revision();
        if state
            .activity_cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != revision)
        {
            let jobs: Vec<(JobHandle, JobRecord)> = state
                .activity
                .records()
                .map(|(handle, record)| (handle, record.clone()))
                .collect();
            state.activity_cache = Some((revision, Rc::new(jobs)));
        }
        let jobs = state
            .activity_cache
            .as_ref()
            .map(|(_, jobs)| Rc::clone(jobs))
            .unwrap_or_default();
        (state.activity.runtime().clone(), jobs)
    }

    /// Whether any queued or executing job will replace marker output when it
    /// commits — the window in which a destructive marker edit would appear to
    /// be undone by the commit landing after it.
    #[must_use]
    pub fn marker_update_active(&self) -> bool {
        self.inner.borrow().activity.records().any(|(_, record)| {
            record.operation().activity().may_update_markers && !record.state().is_terminal()
        })
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
    #[must_use]
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
        self.inner.borrow().scene
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

    /// Takes the latest marker/text result produced by a snuffling request.
    /// Its sole consumer is gated on [`Self::snuffling_gen`], so moving the
    /// result out (up to a full marker budget) beats cloning it.
    #[must_use]
    pub fn take_snuffling(&self) -> Option<SnufflingResult> {
        self.inner.borrow_mut().snuffling.take()
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

    /// Returns an owned snapshot of the current view (decimated waveforms).
    ///
    /// The snapshot must not expose the bridge's `RefCell` guard: frontend code
    /// may call back into the bridge while it renders or exports this data.
    #[must_use]
    pub fn view(&self) -> Option<Arc<ViewData>> {
        self.inner.borrow().view.clone()
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
mod web;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        Bridge, BundleAbout, BundleView, Inner, Operation, Repaint, ViewData,
        install_bundle_presentation,
    };

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

    #[test]
    fn view_snapshot_outlives_public_bridge_mutation() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}

        assert_send_sync_static::<Arc<ViewData>>();
        let bridge = Bridge::new(Repaint::new(|| {}, |_| {}));
        bridge.inner.borrow_mut().view = Some(Arc::new(ViewData::default()));

        let snapshot: Option<Arc<ViewData>> = bridge.view();
        let submission = bridge.begin_local(Operation::ExportSvg, None);
        bridge.finish_local(submission.handle(), Ok(()));

        assert!(snapshot.is_some());
        assert!(bridge.view().is_some());
    }
}
