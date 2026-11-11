//! Frontend-independent session ownership and once-per-frame synchronization.
#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "host builds validate the session API while its asynchronous runtime paths are wasm-only"
    )
)]

use crate::bridge::{
    ActiveEventResult, Bridge, Nslc, Repaint, SnufMarker, SnufflingRequestParams, SnufflingSpan,
    ViewParams,
};
#[cfg(target_arch = "wasm32")]
use crate::bridge::{
    CommandOutcome, CommandRequest, RemoveEventsRequest, SeedlinkParams, SetActiveEventRequest,
};
use crate::marker_store::MarkerBudget;
#[cfg(target_arch = "wasm32")]
use crate::marker_store::{MarkerCommitOutcome, StagedMarkers};
use crate::markers::Marker;
#[cfg(target_arch = "wasm32")]
use crate::markers::{MarkersCommand, parse_markers_command};
use crate::snufflings::SnufflingConfigs;
use crate::state::AppState;

/// Owns the user-visible state and its asynchronous pyrocko bridge.
pub struct Session {
    /// State rendered and edited by the active frontend.
    pub state: AppState,
    /// Transport and latest decoded results from pyrocko.
    pub bridge: Bridge,
    /// Parameters and persistent output-window state for shared snufflings.
    pub snufflings: SnufflingConfigs,
    last_scene_gen: u64,
    last_scene_bounds: Option<(f64, f64)>,
    last_watched_file_check_s: f64,
    last_active_event_gen: u64,
    last_snuffling_gen: u64,
    /// The window follow mode last wrote; any other value means the user (or a
    /// jump) moved the view since, which suspends following.
    follow_window: Option<(f64, f64)>,
    /// When following was last interrupted by an interactive range change.
    follow_interrupted_at: Option<f64>,
    /// `started_at` (as bits, for exact comparison) of the live session whose
    /// first packet already snapped the track scroll, so later packets leave
    /// the user's scrolling alone.
    live_scroll_snapped_for: Option<u64>,
    annotation_cache_reset_pending: bool,
}

/// How long an interactive range change suspends live following, snuffler's
/// `interactive_range_change_delay_time` (`pile_viewer.py:1253`).
const FOLLOW_SUSPEND_S: f64 = 10.0;

fn marker_from_snuffling(marker: &SnufMarker, nslc_table: &[Nslc], origin: &str) -> Marker {
    let mut mapped = Marker::new(
        marker.t,
        marker.tmax.unwrap_or(marker.t),
        u8::try_from(marker.kind).unwrap_or(0),
    );
    mapped.phase.clone_from(&marker.phase);
    mapped.is_event = marker.is_event;
    mapped.nslc_ids = marker
        .nslc_indexes
        .iter()
        .filter_map(|index| {
            let nslc = nslc_table.get(usize::try_from(*index).ok()?)?;
            Some((
                nslc.net.clone(),
                nslc.sta.clone(),
                nslc.loc.clone(),
                nslc.cha.clone(),
            ))
        })
        .collect();
    mapped.polarity = marker.polarity.and_then(|value| i8::try_from(value).ok());
    mapped.moment_tensor = marker.moment_tensor.as_ref().map(|tensor| {
        [
            tensor.mnn, tensor.mee, tensor.mdd, tensor.mne, tensor.mnd, tensor.med,
        ]
    });
    mapped.kagan_deg = marker.kagan_deg;
    mapped.uncertainty = marker.uncertainty;
    mapped.event_hash.clone_from(&marker.event_hash);
    mapped.magnitude = marker.magnitude;
    mapped.depth_km = marker.depth.map(|depth| depth / 1000.0);
    mapped.lat = marker.lat;
    mapped.lon = marker.lon;
    mapped.automatic = marker.automatic;
    mapped.incidence_angle = marker.incidence_angle;
    mapped.takeoff_angle = marker.takeoff_angle;
    mapped.event_name.clone_from(&marker.event_name);
    mapped.region.clone_from(&marker.region);
    mapped.catalog.clone_from(&marker.catalog);
    mapped.event_time = marker.event_time;
    mapped.dist_active_km = marker.dist_active_km;
    mapped.active = marker.active;
    if !origin.is_empty() {
        mapped.origin = Some(origin.to_owned());
    }
    mapped
}

impl Session {
    /// Creates an empty viewer session and installs its frontend repaint scheduler.
    #[must_use]
    pub fn new(repaint: Repaint) -> Self {
        Self {
            state: AppState::default(),
            bridge: Bridge::new(repaint),
            snufflings: SnufflingConfigs::default(),
            last_scene_gen: 0,
            last_scene_bounds: None,
            last_watched_file_check_s: 0.0,
            last_active_event_gen: 0,
            last_snuffling_gen: 0,
            follow_window: None,
            follow_interrupted_at: None,
            live_scroll_snapped_for: None,
            annotation_cache_reset_pending: false,
        }
    }

    /// Runs the line in the ':' command line (the frontend's Return binding).
    ///
    /// A command over Rust-owned markers is answered on the spot; everything else goes to
    /// pyrocko and the input stays open until [`Session::sync`] picks up the answer.
    /// The visible time window rides along, because that is what snuffler's `goto HH:MM`
    /// and `hide empty` read off the viewer.
    #[cfg(target_arch = "wasm32")]
    pub fn submit_command(&mut self) {
        let Some(line) = self.state.command.submit() else {
            return;
        };
        match parse_markers_command(&line) {
            Some(MarkersCommand::Set(kinds)) => {
                self.state.set_visible_marker_kinds(kinds);
                self.state.command.resolve(None);
            }
            Some(MarkersCommand::NoChange) => self.state.command.resolve(None),
            None => {
                let _submission = self.bridge.run_command(CommandRequest {
                    line,
                    visible_tmin: self.state.tmin,
                    visible_tmax: self.state.tmax,
                });
            }
        }
    }

    /// Moves the view to `time`, spanning `length` seconds when the command said
    /// how much to show (snuffler's `go_to_time`).
    ///
    /// A `goto` that names a date opens the window *at* it — `goto 2023-02-06` shows that
    /// day from midnight, not half of the day before it — with snuffler's 10% margin on
    /// either side.
    /// Without a length the window keeps its width and centres on the time instead.
    pub fn go_to_time(&mut self, time: f64, length: Option<f64>) {
        let target_tmax = time + length.unwrap_or(0.0);
        let (tmin, tmax) = good_looking_time_range(
            self.state.visible_length.jump_samples(),
            self.scene_deltatmin(),
            (self.state.tmin, self.state.tmax),
            time,
            target_tmax,
            false,
        );
        self.state.tmin = tmin;
        self.state.tmax = tmax;
    }

    /// The finest sampling interval of the loaded pile, or 0.0 with nothing
    /// loaded (snuffler's `content_deltat_range` floor, which
    /// [`good_looking_time_range`] turns into a 1 s minimum window).
    #[must_use]
    pub fn scene_deltatmin(&self) -> f64 {
        self.bridge.scene().map_or(0.0, |scene| scene.deltat_min)
    }

    /// The loaded pile's sampling-interval band with snuffler's fallbacks
    /// (`content_deltat_range`, `pile_viewer.py:2745`): 1 ms when there is no finest
    /// rate, 1000 s when there is no coarsest.
    /// Feeds the filter controls' corner range exactly as Qt's `adjust_controls` does.
    #[must_use]
    pub fn scene_deltat_range(&self) -> (f64, f64) {
        let (mut dtmin, mut dtmax) = self
            .bridge
            .scene()
            .map_or((0.0, 0.0), |scene| (scene.deltat_min, scene.deltat_max));
        if !dtmin.is_finite() || dtmin == 0.0 {
            dtmin = 0.001;
        }
        if dtmax == 0.0 {
            dtmax = 1000.0;
        }
        (dtmin, dtmax)
    }

    /// Replaces the current source with live `SeedLink` acquisition.
    #[cfg(target_arch = "wasm32")]
    pub fn start_seedlink(&mut self, params: SeedlinkParams) {
        self.bridge.start_seedlink(params);
    }

    /// `File ▸ Close all`: empties the pile and every annotation that describes
    /// it, back to the blank viewer Qt boots into with no arguments.
    #[cfg(target_arch = "wasm32")]
    pub fn close_all(&mut self) {
        let _submission = self.bridge.empty_scene();
    }

    fn clear_annotations(&mut self) {
        self.state.markers.clear();
        self.state.snuffling_text.clear();
        self.state.show_snuffling = false;
        self.state.set_active_event_request = None;
        self.state.remove_event_hashes.clear();
        self.state.picking = None;
        self.state.uncertainty_drag_last_y = None;
        self.annotation_cache_reset_pending = true;
    }

    /// Takes the pending request to clear frontend-owned annotation caches.
    #[cfg(target_arch = "wasm32")]
    pub fn take_annotation_cache_reset(&mut self) -> bool {
        std::mem::take(&mut self.annotation_cache_reset_pending)
    }

    fn consume_empty_scene(&mut self, scene_gen: u64) {
        self.clear_annotations();
        self.state.track_scroll = 0.0;
        self.state.track_offset = 0;
        self.last_scene_bounds = None;
        self.last_scene_gen = scene_gen;
        self.follow_window = None;
    }

    /// Applies bridge results and flushes pending state intents once per frontend frame.
    ///
    /// Call this before building the frame so it draws the latest results, and
    /// [`Session::request_view`] after the frame so the fetch window reflects what
    /// layout and input handling decided.
    #[cfg(target_arch = "wasm32")]
    pub fn sync(&mut self, now_s: f64) {
        self.fit_new_scene(now_s);
        self.apply_snuffling_result();

        if let Some((time, length)) = self.bridge.take_goto() {
            self.go_to_time(time, length);
        }
        if let Some(outcome) = self.bridge.take_command_outcome() {
            self.state.command.resolve(match outcome {
                CommandOutcome::Succeeded => None,
                CommandOutcome::Failed(message) => Some(message),
            });
        }

        if std::mem::take(&mut self.state.reload_requested) {
            let _submission = self.bridge.reload_files();
        }
        if std::mem::take(&mut self.state.reload_snufflings_requested) {
            let _submission = self.bridge.reload_snufflings();
        }
        if self.state.watch_files {
            // 1000 ms, matching snuffler's watch timer (pile_viewer.py:1225).
            if now_s - self.last_watched_file_check_s > 1.0 {
                self.last_watched_file_check_s = now_s;
                let _submission = self.bridge.poll_watched_files();
            }
            self.bridge
                .request_repaint_after(std::time::Duration::from_secs(1));
        }

        if let Some(request) = self.state.set_active_event_request.take() {
            let _submission = self.bridge.set_active_event(SetActiveEventRequest {
                deactivate: request.deactivate,
                event_hash: request.event_hash,
                t: request.t,
                nslc: request
                    .nslc
                    .map(|(net, sta, loc, cha)| Nslc { net, sta, loc, cha }),
            });
        }
        if !self.state.remove_event_hashes.is_empty() {
            let _submission = self.bridge.remove_events(RemoveEventsRequest {
                event_hashes: std::mem::take(&mut self.state.remove_event_hashes),
            });
        }

        let active_event_gen = self.bridge.active_event_gen();
        if active_event_gen != self.last_active_event_gen {
            self.last_active_event_gen = active_event_gen;
            if let Some(event_state) = self.bridge.event_state() {
                Self::apply_event_state(self.state.markers.as_mut_slice(), &event_state);
            }
        }
    }

    fn apply_event_state(markers: &mut [Marker], state: &ActiveEventResult) {
        use std::collections::HashMap;

        let events: HashMap<&str, _> = state
            .events
            .iter()
            .map(|event| (event.event_hash.as_str(), event))
            .collect();
        let metrics: HashMap<&str, _> = state
            .kagan
            .iter()
            .map(|metric| (metric.event_hash.as_str(), metric))
            .collect();
        let active_hash = state
            .has_event
            .then_some(state.active_hash.as_deref())
            .flatten();

        for marker in markers {
            let Some(event_hash) = marker.event_hash.as_deref() else {
                if marker.is_event {
                    marker.active = false;
                }
                continue;
            };
            if let Some(event) = events.get(event_hash) {
                marker.event_time = (!marker.is_event).then_some(event.time);
                if marker.is_event {
                    marker.tmin = event.time;
                    marker.tmax = event.time;
                    marker.lat = Some(event.lat);
                    marker.lon = Some(event.lon);
                    marker.depth_km = Some(event.depth / 1000.0);
                    marker.magnitude = event.magnitude;
                    marker.event_name.clone_from(&event.name);
                    marker.region.clone_from(&event.region);
                    marker.catalog.clone_from(&event.catalog);
                    marker.moment_tensor = event.moment_tensor.as_ref().map(|tensor| {
                        [
                            tensor.mnn, tensor.mee, tensor.mdd, tensor.mne, tensor.mnd, tensor.med,
                        ]
                    });
                }
            }
            marker.active = marker.is_event && active_hash == Some(event_hash);
            if marker.is_event {
                marker.kagan_deg = metrics.get(event_hash).and_then(|metric| metric.kagan_deg);
                marker.dist_active_km = metrics.get(event_hash).and_then(|metric| metric.dist_km);
            } else {
                marker.kagan_deg = None;
                marker.dist_active_km = None;
            }
        }
    }

    /// Keeps the bridge view request current with the on-screen window.
    ///
    /// Call this at the end of the frame, after layout and input handling have
    /// updated the visible time window and track window: issuing the request
    /// earlier would fetch the previous frame's window, and the corrective frame
    /// may never come in a reactive frontend that only repaints on input.
    #[cfg(target_arch = "wasm32")]
    pub fn request_view(&self, visible_bins: u32) {
        let _submission = self.bridge.ensure_view(self.view_params(visible_bins));
    }

    /// Builds the bridge view request for the current state and render budget.
    ///
    /// The request window is the drawn window, exactly as snuffler hands its own
    /// `prepare_cutout2` the on-screen range: the bridge owns fetch widening via
    /// `liberal_fetch` (Qt's power-of-two grid snapping), and a frontend overscan
    /// on top would change the demean/filter extent in a way Qt has no analogue
    /// for.
    #[must_use]
    pub fn view_params(&self, visible_bins: u32) -> ViewParams {
        let state = &self.state;
        ViewParams {
            tmin: state.tmin,
            tmax: state.tmax,
            npx: visible_bins,
            highpass: state.highpass,
            lowpass: state.lowpass,
            sort: state.sort.as_str().to_owned(),
            subsort: state.subsort.as_str().to_owned(),
            rotate: state.rotate_deg,
            remove_response: state.remove_response,
            mode: state.view_mode.as_str().to_owned(),
            demean: state.demean,
            degap: state.degap,
            allow_downsampling: state.allow_downsampling,
            fft_filtering: state.fft_filtering,
            bandpass_lphp: state.bandpass_lphp,
            distances_3d: state.distances_3d,
            scaling_base: state.scaling_base.as_str().to_owned(),
            scaling_group: state.scaling_group.as_str().to_owned(),
            gain: f64::from(state.gain),
            fix_scale_ranges: state.fix_scale_ranges,
            show_scale_ranges: state.show_scale_ranges,
            show_scale_axes: state.show_scale_axes,
            visible_tmin: Some(state.tmin),
            visible_tmax: Some(state.tmax),
            scale_tmin: Some(state.tmin),
            scale_tmax: Some(state.tmax),
            wf_abs: state.wf_abs,
            wf_integrate: state.wf_integrate,
            wf_color_scale: state.wf_color_scale.as_str().to_owned(),
            wf_colormap: state.wf_cmap.label().to_owned(),
            wf_clip_lo: f64::from(state.wf_clip.0),
            wf_clip_hi: f64::from(state.wf_clip.1),
            wf_symmetry: state.wf_symmetry,
            wf_reverse: state.wf_reverse,
            track_offset: state.track_offset as u64,
            track_count: state.ntracks_shown as u64,
            visible_length: state.visible_length.samples(),
            adaptive_visible_length: state.visible_length.is_adaptive(),
            liberal_fetch: state.liberal_fetch,
            view_sequence: 0,
        }
    }

    /// Builds a snuffling request from the current view and shared configurations.
    #[must_use]
    pub fn snuffling_params(&self, visible_bins: u32) -> SnufflingRequestParams {
        SnufflingRequestParams {
            view: Some(self.view_params(visible_bins)),
            snufflings: Some(self.snufflings.params()),
            // Qt runs a snuffling over the selected non-event markers' spans,
            // falling back to the visible window only when nothing is selected
            // (`chopper_selected_traces`, snuffling.py:1000).
            selected_spans: self
                .state
                .markers
                .iter()
                .filter(|m| m.selected && !m.is_event)
                .map(|m| SnufflingSpan {
                    tmin: m.tmin,
                    tmax: m.tmax,
                    nslc_ids: m
                        .nslc_ids
                        .iter()
                        .map(|(net, sta, loc, cha)| Nslc {
                            net: net.clone(),
                            sta: sta.clone(),
                            loc: loc.clone(),
                            cha: cha.clone(),
                        })
                        .collect(),
                })
                .collect(),
            marker_budget: Some(MarkerBudget::standard().into()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_snuffling_result(&mut self) {
        let generation = self.bridge.snuffling_gen();
        if generation == self.last_snuffling_gen {
            return;
        }
        self.last_snuffling_gen = generation;
        let Some(result) = self.bridge.take_snuffling() else {
            return;
        };

        let (mut event_min, mut event_max, mut has_event) =
            (f64::INFINITY, f64::NEG_INFINITY, false);
        let mut staged_markers = Vec::with_capacity(result.markers.len());
        let mut activates_event = false;
        for marker in &result.markers {
            // Python-side auto-activation (largest catalog magnitude) arrives on
            // the marker so the canvas and table *show* the active event rather
            // than silently measuring distances from an invisible one.
            if marker.active {
                activates_event = true;
            }
            if marker.is_event {
                has_event = true;
                event_min = event_min.min(marker.t);
                event_max = event_max.max(marker.t);
            }
            staged_markers.push(marker_from_snuffling(
                marker,
                &result.nslc_table,
                &result.origin,
            ));
        }
        if activates_event {
            for existing in &mut self.state.markers {
                existing.active = false;
            }
        }
        let budget = MarkerBudget::from_wire(result.marker_budget);
        let outcome = self.state.markers.commit(StagedMarkers {
            markers: staged_markers,
            origin: (!result.origin.is_empty()).then(|| result.origin.clone()),
            dropped: result.markers_dropped,
            budget,
        });
        let truncation = match outcome {
            MarkerCommitOutcome::Committed { .. } => None,
            MarkerCommitOutcome::Truncated {
                added,
                dropped,
                budget,
            } => Some(format!(
                "Showing the {added} strongest markers; {dropped} detections were omitted at the {} marker budget.",
                budget.get()
            )),
        };
        self.state.snuffling_text = result.text;
        if let Some(truncation) = truncation {
            if !self.state.snuffling_text.is_empty() {
                self.state.snuffling_text.push_str("\n\n");
            }
            self.state.snuffling_text.push_str(&truncation);
        }
        self.state.show_snuffling = (result.show_window || result.markers_dropped > 0)
            && !self.state.snuffling_text.is_empty();

        if has_event
            && event_max >= event_min
            && (event_max < self.state.tmin || event_min > self.state.tmax)
        {
            let padding = ((event_max - event_min) * 0.1).max(120.0);
            self.state.tmin = event_min - padding;
            self.state.tmax = event_max + padding;
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn fit_new_scene(&mut self, now_s: f64) {
        let scene_gen = self.bridge.scene_gen();
        if scene_gen == self.last_scene_gen {
            return;
        }
        let Some(scene) = self.bridge.scene() else {
            return;
        };
        let live_session = self.bridge.live_seedlink_session();
        if scene.track_count == 0 && live_session.is_none() {
            self.consume_empty_scene(scene_gen);
            return;
        }
        let bounds = (scene.tmin, scene.tmax);
        if let Some(live) = live_session {
            // A fresh live session resets the suspension and snaps the track
            // scroll once (Qt's `follow` zeroes the interrupt clock,
            // pile_viewer.py:4257); later packets leave scrolling alone.
            if self.live_scroll_snapped_for != Some(live.started_at.to_bits()) {
                self.clear_annotations();
                self.live_scroll_snapped_for = Some(live.started_at.to_bits());
                self.state.track_scroll = 0.0;
                self.state.track_offset = 0;
                self.follow_window = None;
                self.follow_interrupted_at = None;
            }
            // Any view movement this session did not make itself since the last
            // packet — a pan, a zoom, a `goto` — re-arms the suspension, as
            // Qt's `interrupt_following` does on every interactive range
            // change (pile_viewer.py:4235).
            let user_moved = self.follow_window.is_some_and(|(t0, t1)| {
                (t0 - self.state.tmin).abs() > 1e-9 || (t1 - self.state.tmax).abs() > 1e-9
            });
            if user_moved {
                self.follow_interrupted_at = Some(now_s);
            }
            let suspended = self
                .follow_interrupted_at
                .is_some_and(|at| now_s - at < FOLLOW_SUSPEND_S);
            if !suspended {
                // The bridge computes scene.tmax as the maximum trace tmax, so
                // it IS the newest sample's time.
                let latest = scene.tmax;
                let window = live.follow_seconds.max(1.0);
                if latest.is_finite() && latest > live.started_at {
                    self.state.tmax = latest;
                    self.state.tmin = (latest - window).max(live.started_at);
                } else {
                    self.state.tmin = live.started_at;
                    self.state.tmax = live.started_at + window;
                }
            }
            self.follow_window = Some((self.state.tmin, self.state.tmax));
            self.last_scene_bounds = Some(bounds);
            self.last_scene_gen = scene_gen;
        } else {
            let range_changed = self.last_scene_bounds.is_none_or(|(old_tmin, old_tmax)| {
                (old_tmin - scene.tmin).abs() > 1e-6 || (old_tmax - scene.tmax).abs() > 1e-6
            });
            if range_changed {
                self.state.tmin = scene.tmin;
                self.state.tmax = scene.tmax;
                self.state.track_scroll = 0.0;
                self.state.track_offset = 0;
            }
            self.last_scene_bounds = Some(bounds);
            self.last_scene_gen = scene_gen;
        }
    }
}

/// Snuffler's `make_good_looking_time_range` (`pile_viewer.py:2760`): pad a target span
/// by 10 % on each side, flooring every jump — `goto`, `g`, `G` — at one trace-visibility
/// length, `deltatmin * visible_length * 0.95`.
/// `view_range` is the current window, which caps the floor the way Qt caps it; the floor
/// becomes one second when nothing is loaded.
#[must_use]
pub fn good_looking_time_range(
    visible_length_samples: f64,
    deltatmin: f64,
    view_range: (f64, f64),
    mut tmin: f64,
    mut tmax: f64,
    tight: bool,
) -> (f64, f64) {
    if tmax < tmin {
        std::mem::swap(&mut tmin, &mut tmax);
    }
    let mut dt = deltatmin * visible_length_samples * 0.95;
    if dt == 0.0 || !dt.is_finite() {
        dt = 1.0;
    }
    if tight {
        // Tight around a real span pads it; tight around a point zooms in to one
        // visibility length (pile_viewer.py:2770).
        if tmax > tmin {
            let dtm = tmax - tmin;
            return (tmin - dtm * 0.1, tmax + dtm * 0.1);
        }
        let tcenter = f64::midpoint(tmin, tmax);
        return (tcenter - 0.5 * dt, tcenter + 0.5 * dt);
    }
    if tmax - tmin < dt {
        let (vmin, vmax) = view_range;
        let dt = dt.min(vmax - vmin);
        let tcenter = f64::midpoint(tmin, tmax);
        let (etmin, etmax) = (tmin, tmax);
        tmin = etmin.min(tcenter - 0.5 * dt);
        tmax = etmax.max(tcenter + 0.5 * dt);
        let dtm = tmax - tmin;
        // The margin goes only to the sides the floor did not already widen.
        if (etmin - tmin).abs() == 0.0 {
            tmin -= dtm * 0.1;
        }
        if (etmax - tmax).abs() == 0.0 {
            tmax += dtm * 0.1;
        }
    } else {
        let dtm = tmax - tmin;
        tmin -= dtm * 0.1;
        tmax += dtm * 0.1;
    }
    (tmin, tmax)
}

#[cfg(test)]
mod tests {
    use crate::bridge::{ActiveEventResult, EventKagan, EventSnapshot, Repaint};
    use crate::markers::Marker;
    use crate::state::{ViewMode, VisibleLength};

    use super::Session;

    fn session() -> Session {
        Session::new(Repaint::new(|| {}, |_| {}))
    }

    // The request window is the drawn window on both view modes: the bridge owns
    // any fetch widening (liberal_fetch), exactly as snuffler hands
    // prepare_cutout2 the on-screen range.
    #[test]
    fn view_request_is_the_drawn_window() {
        let mut session = session();
        session.state.tmin = 101.0;
        session.state.tmax = 111.0;

        let params = session.view_params(50);
        assert!((params.tmin - 101.0).abs() < f64::EPSILON);
        assert!((params.tmax - 111.0).abs() < f64::EPSILON);
        assert_eq!(params.npx, 50);
        assert_eq!(params.visible_tmin, Some(101.0));
        assert_eq!(params.visible_tmax, Some(111.0));
        assert_eq!(params.scale_tmin, Some(101.0));
        assert_eq!(params.scale_tmax, Some(111.0));
    }

    // Snuffler's visible_length is a sample-count target the bridge turns into
    // min_deltat_allow.
    // The boot default asks for no target at all: Adaptive fetches every channel and lets
    // the bridge decimate to the canvas.
    #[test]
    fn view_request_carries_visible_length_and_liberal_fetch() {
        let mut session = session();
        session.state.liberal_fetch = true;

        let params = session.view_params(50);
        assert_eq!(params.visible_length, None);
        assert!(params.adaptive_visible_length);
        assert!(params.liberal_fetch);

        session.state.visible_length = VisibleLength::Short;
        let params = session.view_params(50);
        assert_eq!(params.visible_length, VisibleLength::Short.samples());
        assert!(!params.adaptive_visible_length);
    }

    #[test]
    fn waterfall_request_keeps_the_exact_visible_window() {
        let mut session = session();
        session.state.tmin = 101.0;
        session.state.tmax = 111.0;
        session.state.view_mode = ViewMode::Waterfall;

        let params = session.view_params(50);
        assert!((params.tmin - 101.0).abs() < f64::EPSILON);
        assert!((params.tmax - 111.0).abs() < f64::EPSILON);
        assert_eq!(params.npx, 50);
    }

    #[test]
    fn snuffling_request_uses_session_owned_configuration() {
        let mut session = session();
        session.snufflings.spectrum.smoothing = true;

        let request = session.snuffling_params(50);
        let smoothing = request
            .snufflings
            .and_then(|configs| configs.spectrum)
            .is_some_and(|spectrum| spectrum.spec_smoothing);
        assert!(smoothing);
    }

    #[test]
    fn empty_scene_retires_fit_memory_and_track_offset() {
        let mut session = session();
        session.last_scene_bounds = Some((10.0, 20.0));
        session.state.track_scroll = 5.0;
        session.state.track_offset = 5;

        session.consume_empty_scene(7);

        assert_eq!(session.last_scene_gen, 7);
        assert_eq!(session.last_scene_bounds, None);
        assert!(session.state.track_scroll.abs() < f32::EPSILON);
        assert_eq!(session.state.track_offset, 0);
    }

    #[test]
    fn canonical_event_state_updates_events_metrics_and_phase_fallback_time() {
        let mut event = Marker::new(123.0, 123.0, 0);
        event.is_event = true;
        event.event_hash = Some("local:event".to_owned());
        let mut phase = Marker::new(124.0, 124.0, 0);
        phase.event_hash = Some("local:event".to_owned());
        phase.kagan_deg = Some(99.0);
        let mut markers = vec![event, phase];
        let state = ActiveEventResult {
            has_event: true,
            active_hash: Some("local:event".to_owned()),
            kagan: vec![EventKagan {
                event_hash: "local:event".to_owned(),
                kagan_deg: Some(4.0),
                dist_km: Some(0.0),
            }],
            events: vec![EventSnapshot {
                event_hash: "local:event".to_owned(),
                time: 123.0,
                lat: 12.5,
                lon: 45.25,
                depth: 4200.0,
                magnitude: Some(3.5),
                name: Some("edited".to_owned()),
                region: Some("Eifel".to_owned()),
                catalog: None,
                moment_tensor: None,
            }],
        };

        Session::apply_event_state(&mut markers, &state);

        let [event, phase] = markers.as_slice() else {
            assert_eq!(markers.len(), 2);
            return;
        };
        assert!(event.active);
        assert!(
            event
                .lat
                .is_some_and(|value| (value - 12.5).abs() < f64::EPSILON)
        );
        assert!(
            event
                .lon
                .is_some_and(|value| (value - 45.25).abs() < f64::EPSILON)
        );
        assert!(
            event
                .depth_km
                .is_some_and(|value| (value - 4.2).abs() < f64::EPSILON)
        );
        assert_eq!(event.event_name.as_deref(), Some("edited"));
        assert!(
            event
                .dist_active_km
                .is_some_and(|value| value.abs() < f64::EPSILON)
        );
        assert!(
            phase
                .event_time
                .is_some_and(|value| (value - 123.0).abs() < f64::EPSILON)
        );
        assert_eq!(phase.kagan_deg, None);
    }
}
