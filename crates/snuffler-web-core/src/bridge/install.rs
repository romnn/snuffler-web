//! How a Python result becomes bridge state.
//!
//! Every reply installs through these, so the invalidation rules live in one place:
//! a new scene clears the view, a new event registry clears the map and re-requests
//! the view, and every replacement bumps the generation its consumer polls.

use std::cell::RefCell;
use std::rc::Rc;

use crate::markers::MarkerBudget;

use super::proto::pb;
use super::{
    ActiveEventResult, BridgeError, BundleAbout, BundleView, DatasetNote, Inner, Outcome,
    ReloadError, ReloadSourceError, SceneMeta, SnufflingResult,
};

/// The marker budget every marker-producing request carries.
pub(super) fn standard_marker_budget() -> pb::MarkerBudget {
    MarkerBudget::standard().into()
}

impl Inner {
    /// Installs a scene and drops the view drawn from the previous one.
    pub(super) fn replace_scene(&mut self, scene: SceneMeta) {
        self.scene = Some(scene);
        self.scene_generation = self.scene_generation.wrapping_add(1);
        self.clear_view();
    }

    pub(super) fn install_waveform_result(&mut self, result: pb::LoadBytesResult) {
        self.diagnostic = None;
        self.install_waveform_content(result);
    }

    /// Installs loaded waveforms without settling the diagnostic, for a file load that
    /// settles it once at its end ([`Self::finish_file_load`]).
    pub(super) fn install_waveform_content(&mut self, mut result: pb::LoadBytesResult) {
        self.replace_scene(result.scene.take().unwrap_or_default());
        if let Some(event_state) = result.event_state.take() {
            self.install_event_state(event_state);
        }
    }

    pub(super) fn install_bundle_result(&mut self, result: pb::BundleResult) {
        self.diagnostic = None;
        self.install_bundle_content(result);
    }

    /// Installs a loaded bundle without settling the diagnostic, as
    /// [`Self::install_waveform_content`] does.
    pub(super) fn install_bundle_content(&mut self, mut result: pb::BundleResult) {
        self.replace_scene(result.scene.take().unwrap_or_default());
        self.install_bundle_presentation(result.view.take(), result.about.take());
        if let Some(info) = result.info.take() {
            self.install_snuffling_result(info);
        }
    }

    /// Settles a finished browser file load, picked, reloaded or polled in the
    /// background, from the sources that failed and how many files it committed.
    ///
    /// Success clears only an earlier file-load failure, which this load resolves.
    /// Any other diagnostic, such as why a live stream stopped, stays on screen, since
    /// a watched-file poll runs every second and would otherwise wipe it at once.
    pub(super) fn finish_file_load(
        &mut self,
        failures: Vec<ReloadSourceError>,
        committed: u32,
    ) -> Result<Outcome, BridgeError> {
        if failures.is_empty() {
            self.clear_file_load_diagnostic();
            return Ok(Outcome::Succeeded);
        }
        let error = BridgeError::from(ReloadError { failures });
        self.diagnostic = Some(error.clone());
        if committed == 0 {
            Err(error)
        } else {
            Ok(Outcome::Succeeded)
        }
    }

    /// Clears the diagnostic when it reports a failed file load, and keeps any other.
    pub(super) fn clear_file_load_diagnostic(&mut self) {
        if matches!(self.diagnostic, Some(BridgeError::Reload(_))) {
            self.diagnostic = None;
        }
    }

    /// Parks what a bundle says about how to show itself, for the frontend to apply.
    ///
    /// The note is stamped with the scene generation it arrived with, so any later
    /// load retires it without a call site having to remember to.
    pub(super) fn install_bundle_presentation(
        &mut self,
        view: Option<BundleView>,
        about: Option<BundleAbout>,
    ) {
        self.pending_view = view;
        self.dataset_note_unseen = about.is_some();
        self.dataset_note = about.map(|about| DatasetNote {
            scene_generation: self.scene_generation,
            about,
        });
    }

    /// Installs a snuffling-shaped result, and the event registry it carries if any.
    pub(super) fn install_snuffling_result(&mut self, mut result: SnufflingResult) {
        if let Some(event_state) = result.event_state.take() {
            self.install_event_state(event_state);
        }
        self.snuffling = Some(result);
        self.snuffling_generation = self.snuffling_generation.wrapping_add(1);
    }

    /// Shows `text` as a snuffling result with no markers.
    pub(super) fn install_snuffling_text(&mut self, text: String, show_window: bool) {
        self.install_snuffling_result(SnufflingResult {
            show_window,
            text,
            markers: Vec::new(),
            scene: None,
            origin: String::new(),
            event_state: None,
            markers_dropped: 0,
            marker_budget: MarkerBudget::standard().get(),
            nslc_table: Vec::new(),
        });
    }

    /// Installs Python's canonical event registry.
    /// Distances and the sort order depend on the active event, so the map and the
    /// view are requested again.
    pub(super) fn install_event_state(&mut self, state: ActiveEventResult) {
        self.event_state = Some(state);
        self.active_event_generation = self.active_event_generation.wrapping_add(1);
        self.clear_map();
        self.invalidate_view_request();
    }

    pub(super) fn clear_event_state(&mut self) {
        self.event_state = None;
        self.active_event_generation = self.active_event_generation.wrapping_add(1);
        self.clear_map();
    }

    pub(super) fn clear_map(&mut self) {
        self.map = None;
        self.map_request = None;
        self.map_job = None;
        self.map_request_generation = self.map_request_generation.wrapping_add(1);
    }

    /// Forgets the requested view so the next frame dispatches a fresh one, while the
    /// drawn view stays on screen until the replacement lands.
    pub(super) fn invalidate_view_request(&mut self) {
        let requests = &mut self.view_requests;
        requests.applied_params = None;
        requests.desired_params = None;
        requests.in_flight = None;
        requests.generation = requests.generation.wrapping_add(1);
    }

    pub(super) fn clear_view(&mut self) {
        self.view = None;
        self.invalidate_view_request();
    }
}

/// Records a recoverable failure for the status line.
pub(super) fn set_diagnostic(inner: &Rc<RefCell<Inner>>, error: BridgeError) {
    inner.borrow_mut().diagnostic = Some(error);
}

/// Settles the diagnostic from one export's result: success clears it, failure takes it.
/// The failure is still returned, so the ledger and the status line agree.
pub(super) fn settle_diagnostic(
    inner: &Rc<RefCell<Inner>>,
    result: Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    match result {
        Ok(()) => {
            inner.borrow_mut().diagnostic = None;
            Ok(())
        }
        Err(error) => {
            inner.borrow_mut().diagnostic = Some(error.clone());
            Err(error)
        }
    }
}
