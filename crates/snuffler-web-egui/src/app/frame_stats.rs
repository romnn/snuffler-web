//! The frame-cost readout: the top bar's frame rate, and the browser tests' export.

use std::cell::Cell;

use wasm_bindgen::prelude::wasm_bindgen;

use super::App;

/// How often the displayed frame rate is allowed to change, in seconds.
///
/// The measurement is smoothed every frame; only the number on screen is held
/// still, because one that rewrites itself at 60 Hz is unreadable.
const REFRESH_INTERVAL_S: f64 = 0.25;
/// Weight of the newest frame in the exponential moving average.
const EMA_ALPHA: f64 = 0.2;

thread_local! {
    static EXPORTED_FRAME_MS: Cell<f64> = const { Cell::new(0.0) };
}

/// Returns the current frame-cost estimate for browser performance tests.
#[wasm_bindgen]
#[must_use]
pub fn snuffler_frame_stats() -> String {
    EXPORTED_FRAME_MS.with(|frame_ms| {
        let frame_ms = frame_ms.get();
        let fps = if frame_ms > 0.0 {
            (1000.0 / frame_ms).clamp(0.0, 60.0)
        } else {
            0.0
        };
        format!("{{\"emaMs\":{frame_ms:.3},\"fps\":{fps:.3}}}")
    })
}

impl App {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rounded rate is clamped to 0..=60 before the cast"
    )]
    pub(super) fn update_frame_stats(&mut self, ui: &egui::Ui, frame: &eframe::Frame) {
        let Some(cpu_usage) = frame.info().cpu_usage else {
            return;
        };
        let frame_ms = f64::from(cpu_usage) * 1000.0;
        if !frame_ms.is_finite() || frame_ms < 0.0 {
            return;
        }
        let avg_ms = match self.frame_stats_ms {
            Some(old) => old * (1.0 - EMA_ALPHA) + frame_ms * EMA_ALPHA,
            None => frame_ms,
        };
        self.frame_stats_ms = Some(avg_ms);
        EXPORTED_FRAME_MS.with(|frame_ms| frame_ms.set(avg_ms));

        let now_s = ui.input(|input| input.time);
        if self.frame_stats_shown_fps.is_some()
            && now_s - self.frame_stats_shown_at_s < REFRESH_INTERVAL_S
        {
            return;
        }
        self.frame_stats_shown_at_s = now_s;
        // egui repaints on demand, so elapsed wall time between frames measures
        // idleness, not capacity.
        // What the CPU time per frame says is how many frames per second this
        // machine could sustain, capped at the 60 Hz the display would grant anyway.
        let fps = if avg_ms > 0.0 { 1000.0 / avg_ms } else { 60.0 };
        self.frame_stats_shown_fps = Some(fps.clamp(0.0, 60.0).round() as u32);
    }
}
