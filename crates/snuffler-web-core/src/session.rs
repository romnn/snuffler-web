//! Frontend-independent session ownership and once-per-frame synchronization.
#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "host builds validate the session API while its asynchronous runtime paths are wasm-only"
    )
)]

use crate::bridge::{Bridge, Repaint, SnufflingRequestParams, ViewParams};
#[cfg(target_arch = "wasm32")]
use crate::bridge::{Nslc, SeedlinkParams, SetActiveEventRequest};
#[cfg(target_arch = "wasm32")]
use crate::markers::{Marker, MarkersCommand, parse_markers_command};
use crate::snufflings::SnufflingConfigs;
use crate::state::{AppState, ViewMode};

const VIEW_TIME_OVERSCAN: f64 = 2.0;
const VIEW_TIME_PAN_BUCKET_FRACTION: f64 = 0.5;

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
    last_watch_poll: f64,
    last_active_event_gen: u64,
    last_snuffling_gen: u64,
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
            last_watch_poll: 0.0,
            last_active_event_gen: 0,
            last_snuffling_gen: 0,
        }
    }

    /// Handles a command locally when it targets Rust-owned markers, or dispatches it to pyrocko.
    #[cfg(target_arch = "wasm32")]
    pub fn run_command(&mut self, line: String) {
        match parse_markers_command(&line) {
            Some(MarkersCommand::Set(kinds)) => self.state.set_visible_marker_kinds(kinds),
            Some(MarkersCommand::NoChange) => {}
            None => self.bridge.run_command(line),
        }
    }

    /// Replaces the current source with live `SeedLink` acquisition.
    #[cfg(target_arch = "wasm32")]
    pub fn start_seedlink(&mut self, params: SeedlinkParams) {
        self.clear_annotations();
        self.bridge.start_seedlink(params);
    }

    fn clear_annotations(&mut self) {
        self.state.markers.clear();
        self.state.snuffling_text.clear();
        self.state.show_snuffling = false;
        self.state.set_active_event_request = None;
        self.state.pending_span = None;
        self.state.picking = None;
        self.state.uncertainty_drag_last_y = None;
    }

    /// Applies bridge results and flushes pending state intents once per frontend frame.
    ///
    /// Call this before building the frame so it draws the latest results, and
    /// [`Session::request_view`] after the frame so the fetch window reflects what
    /// layout and input handling decided.
    #[cfg(target_arch = "wasm32")]
    pub fn sync(&mut self, now_s: f64) {
        self.fit_new_scene();
        self.bridge.refresh_seisbench_progress(now_s);
        self.apply_snuffling_result();

        if let Some((time, length)) = self.bridge.take_goto() {
            let width = length.unwrap_or_else(|| (self.state.tmax - self.state.tmin).max(1.0));
            self.state.tmin = time - width * 0.5;
            self.state.tmax = time + width * 0.5;
        }
        if let Some(message) = self.bridge.take_command_msg() {
            self.state.command_feedback = message;
        }

        if std::mem::take(&mut self.state.reload_requested) {
            self.bridge.reload_files();
        }
        if std::mem::take(&mut self.state.reload_snufflings_requested) {
            self.bridge.reload_snufflings();
        }
        if self.state.watch_files {
            if now_s - self.last_watch_poll > 2.0 {
                self.last_watch_poll = now_s;
                self.bridge.watch_poll();
            }
            self.bridge
                .request_repaint_after(std::time::Duration::from_secs(2));
        }

        if let Some(request) = self.state.set_active_event_request.take() {
            self.bridge.set_active_event(SetActiveEventRequest {
                deactivate: request.deactivate,
                event_hash: request.event_hash,
                t: request.t,
                nslc: request
                    .nslc
                    .map(|(net, sta, loc, cha)| Nslc { net, sta, loc, cha }),
            });
        }

        let active_event_gen = self.bridge.active_event_gen();
        if active_event_gen != self.last_active_event_gen {
            self.last_active_event_gen = active_event_gen;
            for marker in &mut self.state.markers {
                if let Some(hash) = marker.event_hash.as_deref() {
                    marker.kagan_deg = self.bridge.kagan_for(hash);
                }
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
        self.bridge.ensure_view(self.view_params(visible_bins));
    }

    /// Builds the bridge view request for the current state and render budget.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "overscan is a small constant multiple of an already-clamped render budget"
    )]
    pub fn view_params(&self, visible_bins: u32) -> ViewParams {
        let state = &self.state;
        let mut params = ViewParams {
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
        };

        if state.view_mode != ViewMode::Wiggle {
            return params;
        }

        let (overscan, bucket_fraction) = if state.liberal_fetch {
            (3.0, 1.0)
        } else {
            (VIEW_TIME_OVERSCAN, VIEW_TIME_PAN_BUCKET_FRACTION)
        };
        let visible_window = (state.tmax - state.tmin).max(1e-9);
        let origin = self.bridge.scene().map_or(0.0, |scene| scene.tmin);
        let bucket = visible_window * bucket_fraction;
        let bucket_tmin = bucketed_time_start(origin, state.tmin, bucket);

        params.tmin = bucket_tmin - 0.5 * visible_window;
        params.tmax = params.tmin + visible_window * overscan;
        params.npx = (f64::from(visible_bins) * overscan).ceil() as u32;
        if state.view_drag.is_some() {
            params.visible_tmin = Some(params.tmin);
            params.visible_tmax = Some(params.tmax);
        }
        params
    }

    /// Builds a snuffling request from the current view and shared configurations.
    #[must_use]
    pub fn snuffling_params(&self, visible_bins: u32) -> SnufflingRequestParams {
        SnufflingRequestParams {
            view: Some(self.view_params(visible_bins)),
            snufflings: Some(self.snufflings.params()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_snuffling_result(&mut self) {
        let generation = self.bridge.snuffling_gen();
        if generation == self.last_snuffling_gen {
            return;
        }
        self.last_snuffling_gen = generation;
        let Some(result) = self.bridge.snuffling() else {
            return;
        };

        let (mut event_min, mut event_max, mut has_event) =
            (f64::INFINITY, f64::NEG_INFINITY, false);
        for marker in &result.markers {
            let mut mapped = Marker::new(
                marker.t,
                marker.tmax.unwrap_or(marker.t),
                u8::try_from(marker.kind).unwrap_or(0),
            );
            mapped.phase.clone_from(&marker.phase);
            mapped.is_event = marker.is_event;
            mapped.nslc = marker.nslc.as_ref().map(|nslc| {
                (
                    nslc.net.clone(),
                    nslc.sta.clone(),
                    nslc.loc.clone(),
                    nslc.cha.clone(),
                )
            });
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
            if marker.is_event {
                has_event = true;
                event_min = event_min.min(marker.t);
                event_max = event_max.max(marker.t);
            }
            self.state.markers.push(mapped);
        }
        self.state.snuffling_text = result.text;
        self.state.show_snuffling = result.show_window && !self.state.snuffling_text.is_empty();

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
    fn fit_new_scene(&mut self) {
        let scene_gen = self.bridge.scene_gen();
        if scene_gen == self.last_scene_gen {
            return;
        }
        let Some(scene) = self.bridge.scene() else {
            return;
        };
        let bounds = (scene.tmin, scene.tmax);
        if let Some(live) = self.bridge.live_seedlink_session() {
            let latest = scene
                .tracks
                .iter()
                .map(|track| track.tmax)
                .fold(scene.tmax, f64::max);
            let window = live.follow_seconds.max(1.0);
            if latest.is_finite() && latest > live.started_at {
                self.state.tmax = latest;
                self.state.tmin = (latest - window).max(live.started_at);
            } else {
                self.state.tmin = live.started_at;
                self.state.tmax = live.started_at + window;
            }
            self.state.track_scroll = 0.0;
            self.state.track_offset = 0;
            self.last_scene_bounds = Some(bounds);
            self.last_scene_gen = scene_gen;
        } else if !scene.tracks.is_empty() {
            let range_changed = self.last_scene_bounds.is_none_or(|old| {
                (old.0 - bounds.0).abs() > 1e-6 || (old.1 - bounds.1).abs() > 1e-6
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

fn bucketed_time_start(origin: f64, tmin: f64, bucket: f64) -> f64 {
    if !bucket.is_finite() || bucket <= 0.0 {
        return tmin;
    }
    origin + ((tmin - origin) / bucket).floor() * bucket
}

#[cfg(test)]
mod tests {
    use crate::bridge::Repaint;
    use crate::state::{ViewDrag, ViewMode};

    use super::Session;

    fn session() -> Session {
        Session::new(Repaint::new(|| {}, |_| {}))
    }

    #[test]
    fn view_request_overscans_and_buckets_the_visible_window() {
        let mut session = session();
        session.state.tmin = 101.0;
        session.state.tmax = 111.0;

        let params = session.view_params(50);
        assert!((params.tmin - 95.0).abs() < f64::EPSILON);
        assert!((params.tmax - 115.0).abs() < f64::EPSILON);
        assert_eq!(params.npx, 100);
        assert_eq!(params.visible_tmin, Some(101.0));
        assert_eq!(params.visible_tmax, Some(111.0));
    }

    #[test]
    fn pointer_drag_uses_fetched_bounds_for_autoscaling() {
        let mut session = session();
        session.state.tmin = 101.0;
        session.state.tmax = 111.0;
        session.state.view_drag = Some(ViewDrag {
            x: 0.0,
            y: 0.0,
            tmin: 101.0,
            tmax: 111.0,
            pan_only: false,
        });

        let params = session.view_params(50);
        assert_eq!(params.visible_tmin, Some(params.tmin));
        assert_eq!(params.visible_tmax, Some(params.tmax));
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
}
