//! Backend-neutral waveform painting pipeline.
//!
//! [`paint`] layers one frame the way snuffler's `drawit` does:
//!
//! - [`boxes`]: the pile's trace boxes under everything else.
//! - [`axis`]: the time axis below the track area.
//! - per row, [`traces`] (wiggles or waterfall strips, then the track label) and
//!   [`scales`] (scale annotations and zero lines), coloured by [`colors`].
//! - [`markers`]: the per-track and overview marker layers and the floating pick.

mod axis;
mod boxes;
mod colors;
mod markers;
mod scales;
mod traces;

#[cfg(test)]
mod tests;

use ecolor::Color32;
use emath::{Rect, pos2};
use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};
use snuffler_web_core::state::AppState;

pub use self::axis::station_place_text;
pub use self::markers::marker_matches_track;
use crate::draw::DrawTarget;
use crate::layout::Layout;
use crate::palette::Palette;

/// Track labels and marker labels, a shade larger than the chrome's text and bold
/// like snuffler's.
///
/// Qt paints these with the application's default font at `setBold(True)`, which
/// measures about 7 % wider than 12.5 pt did here.
const LABEL_PT: f32 = 13.5;

/// Everything one paint pass reads, settled once per frame.
struct Frame<'a> {
    state: &'a AppState,
    view: &'a ViewData,
    pal: &'a Palette,
    lay: &'a Layout<'a>,
    /// Whether the pile's trace boxes are on, under the current canvas theme.
    boxes_visible: bool,
}

/// One visible row: its track and vertical extent.
struct Row<'a> {
    /// Position among the visible rows, `0` for the topmost.
    index: usize,
    track: &'a ViewTrack,
    top: f32,
    bottom: f32,
    middle: f32,
    height: f32,
    /// Snuffler's gain slider, which scales every amplitude.
    gain: f32,
}

impl Row<'_> {
    /// Projects a sample value of `span` onto this row's y axis.
    ///
    /// The display scale (units to pixels) comes entirely from pyrocko's `yscale`,
    /// and is per span: snuffler projects per (track, scaling key), so two sensors
    /// sharing a location-grouped row each keep their own amplitude scale.
    /// The projection stays f64 until the final pixel, because a DC offset that
    /// dwarfs the span would quantize visibly at f32.
    fn y_of(&self, span: &ViewSpan, value: f64) -> f32 {
        let half_range = span.yscale.max(1e-12);
        let amplitude = f64::from(self.height) * 0.45 * f64::from(self.gain) / half_range;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "one pixel coordinate at the end of an f64 projection"
        )]
        let y = (f64::from(self.middle) - (value - span.ymean) * amplitude) as f32;
        y
    }
}

/// Renders the persistent waveform content (everything except the transient hover
/// crosshair) to any [`DrawTarget`].
///
/// This is the snuffler `drawit` analogue: the on-screen and SVG export backends
/// drive identical geometry through it.
/// `waterfall` swaps the wiggles for spectrogram strips, and `export` leaves out
/// the screen-only time grid.
pub fn paint(
    target: &mut dyn DrawTarget,
    state: &AppState,
    view: &ViewData,
    pal: &Palette,
    lay: &Layout<'_>,
    waterfall: bool,
    export: bool,
) {
    let frame = Frame {
        state,
        view,
        pal,
        lay,
        boxes_visible: state.boxes_visible(pal.theme),
    };
    let colors = colors::TraceColors::settle(state, view, pal);

    target.fill_rect(lay.rect, 0.0, pal.background);
    if frame.boxes_visible && !waterfall {
        boxes::draw_trace_boxes(target, &frame);
    }
    axis::draw_time_axis(target, &frame, export);

    let label_pt = traces::fitted_label_size(target, lay.row_height);
    // Settled once before the row loop, because the snippet pass inside it walks
    // every marker per span.
    // Under overload it is skipped along with the marker layers instead of paying
    // markers × spans on every repaint first.
    let marker_ids = markers::candidates(state);

    // Clip the rows to the track area, so a partially scrolled first or last row
    // never bleeds over the time axis or the top edge.
    target.push_clip(lay.track_area);
    for (index, track) in lay.visible_tracks.iter().enumerate() {
        let top = lay.row_top(index);
        let row = Row {
            index,
            track,
            top,
            bottom: top + lay.row_height,
            middle: top + lay.row_height * 0.5,
            height: lay.row_height,
            gain: state.gain,
        };
        draw_row_frame(target, &frame, &row, waterfall);
        if waterfall && track.frequency_bin_count > 0 {
            traces::draw_waterfall_strips(target, &frame, &row);
        } else {
            traces::draw_wiggles(target, &frame, &colors, &row, marker_ids.as_deref());
            scales::draw_scales(target, &frame, &colors, &row);
        }
        traces::draw_track_label(target, &frame, &row, label_pt);
    }
    target.pop_clip();

    // Markers are painted without the track clip: Qt sets none here, and the event
    // captions deliberately rise above the track area's top edge.
    // Clip to the widget's own extent so far-offscreen span ends stay put.
    target.push_clip(Rect::from_min_max(
        lay.rect.left_top(),
        pos2(lay.rect.right(), lay.track_area.bottom()),
    ));
    markers::draw_markers(target, &frame, marker_ids.as_deref());
    target.pop_clip();

    target.push_clip(lay.track_area);
    markers::draw_floating_pick(target, &frame);
    target.pop_clip();
}

/// The waterfall cell outline and the separator line above a row.
fn draw_row_frame(target: &mut dyn DrawTarget, frame: &Frame<'_>, row: &Row<'_>, waterfall: bool) {
    let Frame { pal, lay, .. } = *frame;

    // The waterfall has no wiggle to carry the row's identity, so it keeps an
    // outlined cell whichever theme is on.
    if frame.boxes_visible && waterfall {
        let cell = Rect::from_min_max(
            pos2(lay.rect.left() + 0.5, row.top + 1.0),
            pos2(lay.rect.right() - 0.5, row.bottom - 1.0),
        );
        stroke_rect(target, cell, 1.0, pal.separator);
    }

    // Independent of the bands, and most useful alongside them: a band is keyed on
    // station, so the three channels of one station share a colour and the
    // boundaries between them are exactly where nothing else marks a row.
    if row.index > 0 && frame.state.track_separators() {
        target.line(
            pos2(lay.rect.left(), row.top),
            pos2(lay.rect.right(), row.top),
            1.0,
            pal.separator,
        );
    }
}

/// Outlines `rect` with four `width`-wide strokes.
fn stroke_rect(target: &mut dyn DrawTarget, rect: Rect, width: f32, color: Color32) {
    target.line(rect.left_top(), rect.right_top(), width, color);
    target.line(rect.right_top(), rect.right_bottom(), width, color);
    target.line(rect.right_bottom(), rect.left_bottom(), width, color);
    target.line(rect.left_bottom(), rect.left_top(), width, color);
}
