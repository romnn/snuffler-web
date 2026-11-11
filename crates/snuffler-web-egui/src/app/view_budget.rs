//! How finely the next view request asks pyrocko to bin each trace.

use snuffler_web_core::state::ViewMode;

use super::App;

// `bins_per_trace` is a per-trace bin budget for Python's min/max envelope.
// The display-scale bin budget keeps transport and renderer work bounded when
// zoomed far out.
//
// One bin per physical pixel is what makes the envelope invisible: the two
// extremes of a bin land in the pixel column they came from, so an Adaptive
// window draws the same trace a fixed visible length draws from raw samples.
// The total-points cap is a backstop for a very tall stack, not the usual
// governor: set below what a screenful of rows at full width costs, it would
// coarsen every trace to a fraction of the canvas's resolution.
const MIN_VIEW_SAMPLE_BINS: f32 = 64.0;
const MAX_VIEW_SAMPLE_BINS_PER_TRACE: f32 = 8192.0;
const MAX_VISIBLE_WIGGLE_POINTS: f32 = 160_000.0;
const MAX_VISIBLE_WATERFALL_CELLS: f32 = 400_000.0;
/// The envelope emits a bin's minimum and its maximum, in the order they occur.
const WIGGLE_POINTS_PER_BIN: f32 = 2.0;
const WATERFALL_CELLS_PER_BIN: f32 = 2.0;

impl App {
    /// The per-trace bin budget of the next view request.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "display dimensions and track counts are small positive UI values; the rounded integer is a rendering budget"
    )]
    pub(super) fn view_sample_bins(&self, ui: &egui::Ui) -> u32 {
        let points_per_pixel = ui.ctx().pixels_per_point();
        let points_per_pixel = if points_per_pixel.is_finite() && points_per_pixel > 0.0 {
            points_per_pixel
        } else {
            1.0
        };
        let visible_tracks = self.session.state.ntracks_shown.max(1) as f32;
        let physical_width = ui.available_width().max(MIN_VIEW_SAMPLE_BINS) * points_per_pixel;
        let (emitted_per_bin, visible_cap) = match self.session.state.view_mode {
            ViewMode::Wiggle => (WIGGLE_POINTS_PER_BIN, MAX_VISIBLE_WIGGLE_POINTS),
            ViewMode::Waterfall => (WATERFALL_CELLS_PER_BIN, MAX_VISIBLE_WATERFALL_CELLS),
        };
        let per_trace_visible_cap = visible_cap / (visible_tracks * emitted_per_bin);
        let bins = physical_width
            .min(MAX_VIEW_SAMPLE_BINS_PER_TRACE)
            .min(per_trace_visible_cap.max(MIN_VIEW_SAMPLE_BINS))
            .max(MIN_VIEW_SAMPLE_BINS);
        bins.ceil() as u32
    }
}
