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
    ActiveEventResult, BridgeError, BundleAbout, BundleView, DatasetNote, Inner, SceneMeta,
    SnufflingResult,
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

    /// Installs the scene a successful load returned, clearing the last load's diagnostic.
    pub(super) fn install_loaded_scene(&mut self, scene: Option<SceneMeta>) {
        self.diagnostic = None;
        self.replace_scene(scene.unwrap_or_default());
    }

    pub(super) fn install_waveform_result(&mut self, mut result: pb::LoadBytesResult) {
        self.install_loaded_scene(result.scene.take());
        if let Some(event_state) = result.event_state.take() {
            self.install_event_state(event_state);
        }
    }

    pub(super) fn install_bundle_result(&mut self, mut result: pb::BundleResult) {
        self.install_loaded_scene(result.scene.take());
        self.install_bundle_presentation(result.view.take(), result.about.take());
        if let Some(info) = result.info.take() {
            self.install_snuffling_result(info);
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
