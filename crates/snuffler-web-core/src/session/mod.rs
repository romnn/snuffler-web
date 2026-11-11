//! Frontend-independent session ownership and once-per-frame synchronization.
//!
//! - this module: [`Session`], the per-frame sync, and the requests it builds;
//! - `scene_fit`: fitting the window to a new scene, and following a live stream;
//! - `snuffling_results`: committing marker results and the event registry;
//! - `time_range`: snuffler's window-jump arithmetic.
#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "host builds validate the session API while its asynchronous runtime paths are wasm-only"
    )
)]

use crate::bridge::{Bridge, Nslc, Repaint, SnufflingRequestParams, SnufflingSpan, ViewParams};
#[cfg(target_arch = "wasm32")]
use crate::bridge::{
    CommandOutcome, CommandRequest, RemoveEventsRequest, SeedlinkParams, SetActiveEventRequest,
};
#[cfg(target_arch = "wasm32")]
use crate::command::{MarkersCommand, parse_markers_command};
use crate::markers::MarkerBudget;
use crate::snufflings::SnufflingConfigs;
use crate::state::AppState;

mod scene_fit;
mod snuffling_results;
mod time_range;

pub use time_range::{TimeJump, TimeWindow, good_looking_time_range};

/// Owns the user-visible state and its asynchronous pyrocko bridge.
pub struct Session {
    /// State rendered and edited by the active frontend.
    pub state: AppState,
    /// Transport and latest decoded results from pyrocko.
    pub bridge: Bridge,
    /// Parameters and persistent output-window state for shared snufflings.
    pub snufflings: SnufflingConfigs,

    // The bridge generations last applied, so each result is applied once
    last_scene_generation: u64,
    last_active_event_generation: u64,
    last_snuffling_generation: u64,
    /// The extent of the last fitted scene; the window refits only when it changes.
    last_scene_bounds: Option<TimeWindow>,
    /// Browser time of the last watched-file poll, in epoch seconds.
    last_watched_file_check_s: f64,

    /// The window live following last wrote; any other value means the user (or a
    /// jump) moved the view since, which suspends following.
    followed_window: Option<TimeWindow>,
    /// When following was last interrupted by an interactive range change.
    follow_interrupted_at: Option<f64>,
    /// `started_at` (as bits, for exact comparison) of the live session whose
    /// first packet already snapped the track scroll, so later packets leave
    /// the user's scrolling alone.
    live_scroll_snapped_for: Option<u64>,
    /// Whether the frontend must drop its annotation caches before the next draw.
    annotation_cache_reset_pending: bool,
}

impl Session {
    /// Creates an empty viewer session and installs its frontend repaint scheduler.
    #[must_use]
    pub fn new(repaint: Repaint) -> Self {
        Self {
            state: AppState::default(),
            bridge: Bridge::new(repaint),
            snufflings: SnufflingConfigs::default(),
            last_scene_generation: 0,
            last_active_event_generation: 0,
            last_snuffling_generation: 0,
            last_scene_bounds: None,
            last_watched_file_check_s: 0.0,
            followed_window: None,
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

    /// Moves the view to `time`, spanning `length_s` seconds when the command said
    /// how much to show (snuffler's `go_to_time`).
    ///
    /// A `goto` that names a date opens the window *at* it (`goto 2023-02-06` shows that
    /// day from midnight, not half of the day before it), with snuffler's 10% margin on
    /// either side.
    /// Without a length the window keeps its width and centres on the time instead.
    pub fn go_to_time(&mut self, time: f64, length_s: Option<f64>) {
        let window = good_looking_time_range(TimeJump {
            target: TimeWindow {
                tmin: time,
                tmax: time + length_s.unwrap_or(0.0),
            },
            view: TimeWindow {
                tmin: self.state.tmin,
                tmax: self.state.tmax,
            },
            deltatmin: self.scene_deltatmin(),
            visible_length_samples: self.state.visible_length.jump_samples(),
            tight: false,
        });
        self.state.tmin = window.tmin;
        self.state.tmax = window.tmax;
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
        let (mut deltat_min, mut deltat_max) = self
            .bridge
            .scene()
            .map_or((0.0, 0.0), |scene| (scene.deltat_min, scene.deltat_max));
        if !deltat_min.is_finite() || deltat_min == 0.0 {
            deltat_min = 0.001;
        }
        if deltat_max == 0.0 {
            deltat_max = 1000.0;
        }
        (deltat_min, deltat_max)
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

    /// Drops every annotation of the previous pile.
    fn clear_annotations(&mut self) {
        self.state.markers.clear();
        self.state.snuffling_text.clear();
        self.state.show_snuffling_text = false;
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

    /// Applies bridge results and flushes pending state intents once per frontend frame.
    ///
    /// Call this before building the frame so it draws the latest results, and
    /// [`Session::request_view`] after the frame so the fetch window reflects what
    /// layout and input handling decided.
    #[cfg(target_arch = "wasm32")]
    pub fn sync(&mut self, now_s: f64) {
        // Apply bridge results
        self.fit_new_scene(now_s);
        self.apply_snuffling_result();
        if let Some(goto) = self.bridge.take_goto() {
            self.go_to_time(goto.time, goto.length_s);
        }
        if let Some(outcome) = self.bridge.take_command_outcome() {
            self.state.command.resolve(match outcome {
                CommandOutcome::Succeeded => None,
                CommandOutcome::Failed(message) => Some(message),
            });
        }

        // Flush reload requests and the watched-file poll
        if std::mem::take(&mut self.state.reload_files_requested) {
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

        // Flush event-registry edits, then mirror the registry onto the markers
        if let Some(request) = self.state.set_active_event_request.take() {
            let _submission = self.bridge.set_active_event(SetActiveEventRequest {
                deactivate: request.deactivate,
                event_hash: request.event_hash,
                time: request.time,
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
        let active_event_generation = self.bridge.active_event_generation();
        if active_event_generation != self.last_active_event_generation {
            self.last_active_event_generation = active_event_generation;
            if let Some(event_state) = self.bridge.event_state() {
                Self::apply_event_state(self.state.markers.as_mut_slice(), &event_state);
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
        let waterfall = &state.waterfall;
        ViewParams {
            tmin: state.tmin,
            tmax: state.tmax,
            bins_per_trace: visible_bins,
            highpass: state.highpass,
            lowpass: state.lowpass,
            sort: state.sort.as_str().to_owned(),
            subsort: state.subsort.as_str().to_owned(),
            rotate_deg: state.rotate_deg,
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
            waterfall_absolute_amplitudes: waterfall.absolute_amplitudes,
            waterfall_integrate: waterfall.integrate,
            waterfall_color_scale: waterfall.color_scale.as_str().to_owned(),
            waterfall_colormap: waterfall.colormap.label().to_owned(),
            waterfall_clip_low: f64::from(waterfall.clip_low),
            waterfall_clip_high: f64::from(waterfall.clip_high),
            waterfall_reverse_colormap: waterfall.reverse_colormap,
            track_offset: state.track_offset as u64,
            track_count: state.ntracks_shown as u64,
            visible_length: state.visible_length.samples(),
            adaptive_visible_length: state.visible_length.is_adaptive(),
            liberal_fetch: state.liberal_fetch,
            // The bridge stamps each dispatched request with its own sequence.
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
                .filter(|marker| marker.selected && !marker.is_event)
                .map(|marker| SnufflingSpan {
                    tmin: marker.tmin,
                    tmax: marker.tmax,
                    nslc_ids: marker
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
}

#[cfg(test)]
mod tests {
    use crate::bridge::{ActiveEventResult, EventKagan, EventSnapshot, Repaint};
    use crate::markers::Marker;
    use crate::state::{ViewMode, VisibleLength};

    use super::{Session, TimeWindow};

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
        assert_eq!(params.bins_per_trace, 50);
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
        assert_eq!(params.bins_per_trace, 50);
    }

    #[test]
    fn snuffling_request_uses_session_owned_configuration() {
        let mut session = session();
        session.snufflings.spectrum.smoothing = true;

        let request = session.snuffling_params(50);
        let smoothing = request
            .snufflings
            .and_then(|configs| configs.spectrum)
            .is_some_and(|spectrum| spectrum.smoothing);
        assert!(smoothing);
    }

    #[test]
    fn empty_scene_retires_fit_memory_and_track_offset() {
        let mut session = session();
        session.last_scene_bounds = Some(TimeWindow {
            tmin: 10.0,
            tmax: 20.0,
        });
        session.state.track_scroll = 5.0;
        session.state.track_offset = 5;

        session.consume_empty_scene(7);

        assert_eq!(session.last_scene_generation, 7);
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
