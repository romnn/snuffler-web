//! Fitting the time window to a newly loaded scene, and following a live stream.

use super::Session;
#[cfg(target_arch = "wasm32")]
use super::TimeWindow;

/// How long an interactive range change suspends live following, snuffler's
/// `interactive_range_change_delay_time` (`pile_viewer.py:1253`).
#[cfg(target_arch = "wasm32")]
const FOLLOW_SUSPEND_S: f64 = 10.0;

impl Session {
    /// Fits the window to a new scene once per scene generation.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn fit_new_scene(&mut self, now_s: f64) {
        let scene_generation = self.bridge.scene_generation();
        if scene_generation == self.last_scene_generation {
            return;
        }
        let Some(scene) = self.bridge.scene() else {
            return;
        };
        let live_session = self.bridge.live_seedlink_session();
        if scene.track_count == 0 && live_session.is_none() {
            self.consume_empty_scene(scene_generation);
            return;
        }

        let bounds = TimeWindow {
            tmin: scene.tmin,
            tmax: scene.tmax,
        };
        if let Some(live) = live_session {
            // The bridge computes scene.tmax as the maximum trace tmax, so it is the
            // newest sample's time.
            self.follow_live_scene(live, bounds.tmax, now_s);
        } else {
            let range_changed = self.last_scene_bounds.is_none_or(|previous| {
                (previous.tmin - bounds.tmin).abs() > 1e-6
                    || (previous.tmax - bounds.tmax).abs() > 1e-6
            });
            if range_changed {
                self.state.tmin = bounds.tmin;
                self.state.tmax = bounds.tmax;
                self.state.track_scroll = 0.0;
                self.state.track_offset = 0;
            }
        }
        self.last_scene_bounds = Some(bounds);
        self.last_scene_generation = scene_generation;
    }

    /// Moves the window along with the newest live sample, unless the user moved it
    /// in the last [`FOLLOW_SUSPEND_S`].
    #[cfg(target_arch = "wasm32")]
    fn follow_live_scene(
        &mut self,
        live: crate::bridge::LiveSeedlinkSession,
        newest_sample_time: f64,
        now_s: f64,
    ) {
        // A fresh live session resets the suspension and snaps the track scroll
        // once (Qt's `follow` zeroes the interrupt clock, pile_viewer.py:4257);
        // later packets leave scrolling alone.
        if self.live_scroll_snapped_for != Some(live.started_at.to_bits()) {
            self.clear_annotations();
            self.live_scroll_snapped_for = Some(live.started_at.to_bits());
            self.state.track_scroll = 0.0;
            self.state.track_offset = 0;
            self.followed_window = None;
            self.follow_interrupted_at = None;
        }

        // Any view movement this session did not make itself since the last
        // packet (a pan, a zoom, a `goto`) re-arms the suspension, as Qt's
        // `interrupt_following` does on every interactive range change
        // (pile_viewer.py:4235).
        let user_moved = self.followed_window.is_some_and(|followed| {
            (followed.tmin - self.state.tmin).abs() > 1e-9
                || (followed.tmax - self.state.tmax).abs() > 1e-9
        });
        if user_moved {
            self.follow_interrupted_at = Some(now_s);
        }
        let suspended = self
            .follow_interrupted_at
            .is_some_and(|at| now_s - at < FOLLOW_SUSPEND_S);

        if !suspended {
            let width = live.follow_seconds.max(1.0);
            if newest_sample_time.is_finite() && newest_sample_time > live.started_at {
                self.state.tmax = newest_sample_time;
                self.state.tmin = (newest_sample_time - width).max(live.started_at);
            } else {
                self.state.tmin = live.started_at;
                self.state.tmax = live.started_at + width;
            }
        }
        self.followed_window = Some(TimeWindow {
            tmin: self.state.tmin,
            tmax: self.state.tmax,
        });
    }

    /// Resets the window state for an empty pile (`File ▸ Close all`).
    pub(super) fn consume_empty_scene(&mut self, scene_generation: u64) {
        self.clear_annotations();
        self.state.track_scroll = 0.0;
        self.state.track_offset = 0;
        self.last_scene_bounds = None;
        self.last_scene_generation = scene_generation;
        self.followed_window = None;
    }
}
