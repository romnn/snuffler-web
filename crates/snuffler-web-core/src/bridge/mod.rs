//! The bridge data model plus the wasm transport between a frontend and pyrocko.
//!
//! On the web, this calls into `web/bridge.js`, which forwards protobuf request
//! bytes to a Pyodide Web Worker (`CPython` + numpy + scipy + pyrocko, all
//! compiled to WASM) and returns protobuf metadata plus raw sample buffers.
//!
//! `proto/snuffler_web_proto/*.proto` is the boundary contract.
//! The decoded protobuf messages are the app-facing model; there is no parallel
//! hand-written layer.
//!
//! Layout:
//!
//! - this module: [`Bridge`], its shared state, and the snapshots the frontend polls;
//! - `install`: how each Python reply becomes bridge state;
//! - `web`: the wasm-only transport and every operation that submits Python work.
#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "host builds exercise pure protocol code while the bridge runtime is only active in wasm"
    )
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::activity::{
    ActivityLedger, CancelRequest, JobError, JobHandle, JobRecord, Operation, Outcome,
    RuntimeEpoch, Submission,
};
#[cfg(target_arch = "wasm32")]
use crate::activity::{JobState, Progress, RuntimeState};

mod error;
mod ingest;
mod install;
mod live;
mod proto;
mod results;
mod tiles;

pub use error::{
    BridgeError, MarshalError, ReloadError, ReloadFailureReason, ReloadSourceError, SeedlinkError,
};
#[cfg(target_arch = "wasm32")]
use ingest::{BatchAction, LiveIngestCompletion};
#[cfg(target_arch = "wasm32")]
use live::LiveStream;
pub use live::{LIVE_FOLLOW_SECONDS, LIVE_RETAIN_SECONDS, LiveSeedlinkSession, SeedlinkParams};
use live::{LiveIngestState, LiveSessionEpoch, LiveState, StreamId};
use results::FigureSlot;
pub use results::{
    ActivitySnapshot, CommandOutcome, FigureData, GotoTarget, SeisbenchProgress, SeisbenchStatus,
    ViewData,
};
pub use snuffler_web_proto::{CallError, RemoteError, TransportError};
use tiles::TileCache;
pub use tiles::{ReadyTile, TileRequest};
#[cfg(target_arch = "wasm32")]
use tiles::{TILE_BATCH_SIZE, TILE_BATCHES_IN_FLIGHT, TileKey, TileState};

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
// Wire shapes only the wasm bridge implementation touches.
#[cfg(target_arch = "wasm32")]
pub(crate) use proto::pb::{
    BrowserFileLoadPage, BrowserSourceAck, BrowserSourceAckList, IngestMiniSeedRequest,
};

/// Local-file header of a zip archive, which is how a dataset bundle is
/// recognized wherever one can arrive.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// The bridge's shared state: the activity ledger plus the latest result of every kind
/// of Python work, each with the generation counter its consumer polls.
#[derive(Default)]
struct Inner {
    activity: ActivityLedger,
    /// The last runtime epoch observed by the per-frame sync.
    /// A change with no RPC in flight is the only evidence of an idle worker death, so
    /// the sync compares against this to report the loss exactly once.
    runtime_epoch: Option<RuntimeEpoch>,
    /// The record snapshot handed to the per-frame activity UI, keyed by the
    /// ledger revision it was built from, so idle frames share one allocation.
    activity_cache: Option<ActivityCache>,
    /// Latest recoverable failure that should remain visible without making the
    /// bridge operationally unavailable.
    diagnostic: Option<BridgeError>,

    scene: Option<SceneMeta>,
    /// Bumped every time a new scene is loaded, so the frontend can reset its view
    /// window when (and only when) the data actually changes.
    scene_generation: u64,
    view: Option<Arc<ViewData>>,
    /// Bumped whenever new view data lands, for texture caching.
    view_epoch: u64,
    view_requests: ViewRequests,
    /// Whether the first drawn view has already asked Python to import the
    /// scientific stack, which every later analysis needs.
    scientific_tools_requested: bool,

    /// Latest render of each open figure window (amplitude spectrum, cake, …), keyed by
    /// figure kind.
    figures: HashMap<String, FigureSlot>,
    snuffling: Option<SnufflingResult>,
    snuffling_generation: u64,
    /// A custom model name the user just uploaded ("Add Model"), for the frontend to
    /// add to the Cake Phase dropdown and select.
    pending_cake_model: Option<String>,
    /// A pending `goto <time>` target the session should jump the view to.
    pending_goto: Option<GotoTarget>,
    /// How the last ':' command dispatched to pyrocko went, for the command line
    /// to close itself or show the error.
    command_outcome: Option<CommandOutcome>,

    /// Python's canonical event registry.
    event_state: Option<ActiveEventResult>,
    /// Bumped whenever Python returns a new event registry.
    active_event_generation: u64,
    map: Option<MapData>,
    map_request: Option<MapRequest>,
    /// Bumped per map request, so a superseded reply is dropped.
    map_request_generation: u64,
    map_job: Option<JobHandle>,
    tiles: TileCache,

    live: LiveState,
    /// Bumped at the moment the user picks what the pile shows: a load, a download, a
    /// live stream, or `Close all`.
    /// A load that only decides once the worker answers, as the boot example does,
    /// compares it with the value it took and yields when it has moved.
    source_choice: u64,

    /// The datasets `File ▸ Examples` offers, fetched the first time that menu is opened
    /// rather than compiled in, so a deployment can gain an example without a new wasm
    /// build.
    /// `None` until it has been asked for.
    examples: Option<Result<Vec<ExampleDataset>, BridgeError>>,
    examples_job: Option<JobHandle>,
    /// The view a just-loaded bundle asked for, for the frontend to apply.
    /// The bridge does not own the sort state, so it parks it here the way a fetched
    /// cake model waits in `pending_cake_model`.
    pending_view: Option<BundleView>,
    /// What the loaded dataset says about itself.
    /// Unlike the view, this is not spent on arrival: `Help ▸ About this dataset` shows
    /// it again later.
    dataset_note: Option<DatasetNote>,
    /// Whether that note has yet to be put on screen.
    /// A dataset introduces itself once, when it arrives, which is a fact known here
    /// rather than something the frontend should infer from a generation change.
    dataset_note_unseen: bool,
}

impl Inner {
    /// Records that the user picked a new source for the pile, and returns that pick.
    fn choose_source(&mut self) -> u64 {
        self.source_choice = self.source_choice.wrapping_add(1);
        self.source_choice
    }

    /// Whether `choice`, from [`Self::choose_source`], is still the user's latest pick.
    fn is_latest_source_choice(&self, choice: u64) -> bool {
        self.source_choice == choice
    }

    /// Settles an `open_example` request whose listing has arrived.
    ///
    /// It keeps the listing for the examples menu and returns the example to load, or
    /// `None` when a source the user picked after `choice` supersedes it.
    fn example_to_open(
        &mut self,
        choice: u64,
        listed: Vec<ExampleDataset>,
        slug: String,
    ) -> Result<Option<ExampleDataset>, BridgeError> {
        let found = listed.iter().find(|example| example.slug == slug).cloned();
        self.examples = Some(Ok(listed));
        let example = found.ok_or(BridgeError::UnknownExample { slug })?;
        Ok(self.is_latest_source_choice(choice).then_some(example))
    }

    /// The live session current now, which a retirement decided later is fenced to.
    fn live_session_fence(&self) -> LiveSessionEpoch {
        LiveSessionEpoch(self.live.session_epoch)
    }

    /// Whether the live session is still the one `fence` saw, so retiring it cannot end
    /// a stream started since.
    fn is_same_live_session(&self, fence: LiveSessionEpoch) -> bool {
        LiveSessionEpoch(self.live.session_epoch) == fence
    }

    /// Makes a new live stream the pile's source, retiring any earlier session.
    fn begin_live_session(&mut self, stream: StreamId, session: LiveSessionEpoch, follow_s: f64) {
        self.choose_source();
        self.live.retire();
        self.live.session_epoch = session.0;
        self.live.last_stream_id = stream.0;
        self.live.ingest = LiveIngestState::Opening { stream, session };
        self.live.follow_s = follow_s;
        self.diagnostic = None;
    }
}

/// The shared activity snapshot and the ledger revision it was built from.
struct ActivityCache {
    revision: u64,
    jobs: Rc<Vec<(JobHandle, JobRecord)>>,
}

/// What the loaded dataset says about itself, stamped with the scene it describes.
///
/// Any later load bumps the scene generation, so prose about the previous pile stops
/// being shown without a single call site having to clear it.
struct DatasetNote {
    scene_generation: u64,
    about: BundleAbout,
}

/// The coalescing state of the view lane: at most one `GetView` is in flight, and the
/// newest wanted window waits in `desired_params` until it returns.
#[derive(Default)]
struct ViewRequests {
    /// The request in flight, with the params it was issued for.
    in_flight: Option<InFlightView>,
    /// The window the frontend wants next; `None` once it is applied or in flight.
    desired_params: Option<ViewParams>,
    /// The params of the view on screen.
    applied_params: Option<ViewParams>,
    /// Bumped by every invalidation, so a reply to a forgotten request is dropped.
    generation: u64,
    /// The sequence number of the last issued request; Python echoes it back.
    last_sequence: u64,
    /// The newest Python session generation a reply carried.
    /// An older one means the reply predates a reload and is stale.
    python_session_generation: u64,
    /// Browser time before which a failed request is not retried, in epoch seconds.
    retry_after_s: f64,
}

/// The one `GetView` the view lane runs.
struct InFlightView {
    job: JobHandle,
    /// The canonical params, without the per-request sequence number.
    params: ViewParams,
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

/// Browser time in epoch seconds; 0.0 on the host, where nothing runs.
fn now_s() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
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
    pub fn activity_snapshot(&self) -> ActivitySnapshot {
        #[cfg(target_arch = "wasm32")]
        web::sync_runtime(&self.inner);

        let mut borrow = self.inner.borrow_mut();
        let state = &mut *borrow;
        let revision = state.activity.revision();
        let cached_jobs = state
            .activity_cache
            .as_ref()
            .filter(|cache| cache.revision == revision)
            .map(|cache| Rc::clone(&cache.jobs));
        let jobs = cached_jobs.unwrap_or_else(|| {
            let jobs: Rc<Vec<_>> = Rc::new(
                state
                    .activity
                    .records()
                    .map(|(handle, record)| (handle, record.clone()))
                    .collect(),
            );
            state.activity_cache = Some(ActivityCache {
                revision,
                jobs: Rc::clone(&jobs),
            });
            jobs
        });
        ActivitySnapshot {
            runtime: state.activity.runtime().clone(),
            jobs,
        }
    }

    /// Whether any queued or executing job will replace marker output when it
    /// commits: the window in which a destructive marker edit would appear to
    /// be undone by the commit landing after it.
    #[must_use]
    pub fn marker_update_active(&self) -> bool {
        self.inner.borrow().activity.records().any(|(_, record)| {
            record.operation().activity().may_update_markers && !record.state().is_terminal()
        })
    }

    /// Registers executor-owned local work before it leaves the render loop.
    pub fn begin_local(&self, operation: Operation, detail: Option<String>) -> Submission {
        self.inner
            .borrow_mut()
            .activity
            .submit_local(operation, detail, now_s())
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
        use proto::pb::CancelDisposition;

        let request = self
            .inner
            .borrow_mut()
            .activity
            .request_cancel(handle, now_s());
        let disposition = match request {
            CancelRequest::Cancelled => CancelDisposition::Queued,
            // Running work: ask the worker to interrupt it
            CancelRequest::Interrupt | CancelRequest::AlreadyCancelling => {
                #[cfg(target_arch = "wasm32")]
                {
                    web::cancel_job(handle).unwrap_or_else(|error| {
                        self.inner.borrow_mut().diagnostic = Some(error);
                        CancelDisposition::Cancelling
                    })
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    CancelDisposition::Cancelling
                }
            }
            CancelRequest::TooLate => CancelDisposition::TooLate,
            CancelRequest::NotFound => CancelDisposition::NotFound,
            CancelRequest::NotCancellable => CancelDisposition::NotCancellable,
        };
        if disposition == CancelDisposition::TooLate {
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

    /// Returns the current inference device and the running `SeisBench` picker's progress.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn seisbench_status(&self) -> SeisbenchStatus {
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
        SeisbenchStatus {
            device: web::seisbench_device(),
            progress,
        }
    }

    /// Returns the latest scene metadata snapshot.
    #[must_use]
    pub fn scene(&self) -> Option<SceneMeta> {
        self.inner.borrow().scene
    }

    /// Generation counter that increments on each new scene load.
    #[must_use]
    pub fn scene_generation(&self) -> u64 {
        self.inner.borrow().scene_generation
    }

    /// Current `SeedLink` session metadata, if live acquisition is active.
    #[must_use]
    pub fn live_seedlink_session(&self) -> Option<LiveSeedlinkSession> {
        let inner = self.inner.borrow();
        let follow_seconds = if inner.live.follow_s > 0.0 {
            inner.live.follow_s
        } else {
            LIVE_FOLLOW_SECONDS
        };
        inner.live.started_at.map(|started_at| LiveSeedlinkSession {
            started_at,
            follow_seconds,
        })
    }

    /// Takes a pending `goto` set by a ':' command for the session to apply.
    #[must_use]
    pub fn take_goto(&self) -> Option<GotoTarget> {
        self.inner.borrow_mut().pending_goto.take()
    }

    /// Takes how the last ':' command dispatched to pyrocko turned out.
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
    pub fn figure_generation(&self, kind: &str) -> u64 {
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

    /// Generation counter for snuffling results, so the session applies each once.
    #[must_use]
    pub fn snuffling_generation(&self) -> u64 {
        self.inner.borrow().snuffling_generation
    }

    /// Takes the latest marker/text result produced by a snuffling request.
    /// Its sole consumer is gated on [`Self::snuffling_generation`], so moving the
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

    /// Whether the dataset now on screen says anything about itself, for the menu
    /// entry that only needs to exist or not, without cloning the prose.
    #[must_use]
    pub fn has_dataset_note(&self) -> bool {
        let inner = self.inner.borrow();
        inner
            .dataset_note
            .as_ref()
            .is_some_and(|note| note.scene_generation == inner.scene_generation)
    }

    /// What the dataset now on screen says about itself, if it says anything.
    #[must_use]
    pub fn dataset_note(&self) -> Option<BundleAbout> {
        let inner = self.inner.borrow();
        inner
            .dataset_note
            .as_ref()
            .filter(|note| note.scene_generation == inner.scene_generation)
            .map(|note| note.about.clone())
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

    /// Generation counter bumped on each active-event change.
    #[must_use]
    pub fn active_event_generation(&self) -> u64 {
        self.inner.borrow().active_event_generation
    }

    /// Returns Python's latest canonical event-registry projection.
    #[must_use]
    pub fn event_state(&self) -> Option<ActiveEventResult> {
        self.inner.borrow().event_state.clone()
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
        let inner = self.inner.borrow();
        inner.view.is_some()
            && inner.view_requests.desired_params.is_none()
            && inner.view_requests.in_flight.is_none()
    }
}

// Web target: drive Pyodide through web/bridge.js.
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use color_eyre::eyre::{self, ensure};

    use super::proto::pb;
    use super::{
        Bridge, BridgeError, BundleAbout, BundleView, ExampleDataset, Inner, LIVE_FOLLOW_SECONDS,
        LiveSessionEpoch, Operation, Outcome, ReloadError, ReloadFailureReason, ReloadSourceError,
        Repaint, SeedlinkError, StreamId, ViewData,
    };

    /// A watched-file poll that finds nothing to do, or reloads a changed file, keeps
    /// the reason a live stream stopped on screen instead of falling back to "Ready".
    #[test]
    fn a_watched_file_poll_keeps_a_live_stream_diagnostic() -> eyre::Result<()> {
        let mut inner = Inner {
            diagnostic: Some(
                SeedlinkError::Closed {
                    code: 1006,
                    reason: String::new(),
                }
                .into(),
            ),
            ..Default::default()
        };

        // A poll that reloads one changed file, then one that finds no change
        inner.install_waveform_content(pb::LoadBytesResult::default());
        let changed = inner.finish_file_load(Vec::new(), 1)?;
        let unchanged = inner.finish_file_load(Vec::new(), 0)?;

        ensure!(
            matches!(
                (changed, unchanged),
                (Outcome::Succeeded, Outcome::Succeeded)
            ),
            "both polls succeed"
        );
        ensure!(
            matches!(
                inner.diagnostic,
                Some(BridgeError::Seedlink(SeedlinkError::Closed {
                    code: 1006,
                    ..
                }))
            ),
            "the live stream's diagnostic stays, got {:?}",
            inner.diagnostic
        );
        Ok(())
    }

    /// A file load that succeeds clears the failure an earlier file load left, which it
    /// resolves.
    #[test]
    fn a_successful_file_load_clears_an_earlier_file_load_failure() -> eyre::Result<()> {
        let mut inner = Inner {
            diagnostic: Some(
                ReloadError {
                    failures: vec![ReloadSourceError {
                        source_id: "a".to_owned(),
                        name: "a.mseed".to_owned(),
                        reason: ReloadFailureReason::BrowserRead {
                            detail: "gone".to_owned(),
                        },
                    }],
                }
                .into(),
            ),
            ..Default::default()
        };

        inner.finish_file_load(Vec::new(), 1)?;

        ensure!(inner.diagnostic.is_none(), "the old failure is resolved");
        Ok(())
    }

    /// A bundle retires the live session that was current when it was asked for, but
    /// not one the user started while the file was still being read.
    #[test]
    fn a_bundle_retires_only_the_live_session_it_was_asked_for_under() {
        let mut inner = Inner::default();
        inner.begin_live_session(StreamId(1), LiveSessionEpoch(1), LIVE_FOLLOW_SECONDS);

        // The session current at the pick is the one the bundle replaces.
        let fence = inner.live_session_fence();
        assert!(inner.is_same_live_session(fence));

        // A stream started after the pick survives the bundle.
        inner.begin_live_session(StreamId(2), LiveSessionEpoch(2), LIVE_FOLLOW_SECONDS);
        assert!(!inner.is_same_live_session(fence));
    }

    const BOOT_SLUG: &str = "sumatra-2004-12-26-m9.1-surface-waves";

    fn listing() -> Vec<ExampleDataset> {
        vec![ExampleDataset {
            slug: BOOT_SLUG.to_owned(),
            url: "examples/sumatra.zip".to_owned(),
            ..ExampleDataset::default()
        }]
    }

    /// The boot example loads when the user has picked nothing while the worker booted.
    #[test]
    fn boot_example_loads_when_nothing_was_picked_meanwhile() -> eyre::Result<()> {
        let mut inner = Inner::default();
        let boot_example = inner.choose_source();

        let example = inner.example_to_open(boot_example, listing(), BOOT_SLUG.to_owned())?;

        ensure!(
            example.is_some_and(|example| example.slug == BOOT_SLUG),
            "the boot example loads"
        );
        Ok(())
    }

    /// A live stream started while the boot example waits for the worker supersedes it,
    /// so the example cannot replace the stream once the listing arrives.
    #[test]
    fn boot_example_yields_to_a_live_stream_started_meanwhile() -> eyre::Result<()> {
        let mut inner = Inner::default();
        let boot_example = inner.choose_source();

        inner.begin_live_session(StreamId(1), LiveSessionEpoch(1), LIVE_FOLLOW_SECONDS);
        let example = inner.example_to_open(boot_example, listing(), BOOT_SLUG.to_owned())?;

        ensure!(example.is_none(), "the live stream keeps the pile");
        // The listing still reaches the examples menu.
        ensure!(
            matches!(&inner.examples, Some(Ok(examples)) if examples.len() == 1),
            "the listing is kept"
        );
        Ok(())
    }

    /// Files asked for while the boot listing holds the job lane queue behind it; the
    /// request alone supersedes the example, which would otherwise be admitted behind
    /// the picker and replace the files it loads.
    #[test]
    fn boot_example_yields_to_files_asked_for_behind_its_listing() -> eyre::Result<()> {
        let mut inner = Inner::default();
        let boot_example = inner.choose_source();

        // What `Bridge::open_files` records before it queues the picker.
        inner.choose_source();
        let example = inner.example_to_open(boot_example, listing(), BOOT_SLUG.to_owned())?;

        ensure!(example.is_none(), "the picked files keep the pile");
        Ok(())
    }

    #[test]
    fn bundle_presentation_uses_the_installed_scene_generation() {
        let mut inner = Inner {
            scene_generation: 7,
            ..Default::default()
        };

        inner.install_bundle_presentation(
            Some(BundleView {
                sort: "distance".to_owned(),
                subsort: "grouped".to_owned(),
            }),
            Some(BundleAbout {
                title: "Changed bundle".to_owned(),
                body: "Updated note".to_owned(),
            }),
        );

        // The view waits for the frontend, and the note is stamped with the scene
        // it arrived with.
        assert_eq!(
            inner.pending_view.as_ref().map(|view| view.sort.as_str()),
            Some("distance")
        );
        assert!(inner.dataset_note_unseen);
        assert_eq!(
            inner
                .dataset_note
                .as_ref()
                .map(|note| (note.scene_generation, note.about.title.as_str())),
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
