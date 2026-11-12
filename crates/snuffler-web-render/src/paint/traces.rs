//! A row's content: its wiggles or waterfall strips, and its track label.

use emath::{Align2, Rect, pos2};

use super::colors::TraceColors;
use super::markers::{self, MarkerSnippet};
use super::{Frame, LABEL_PT, Row};
use crate::draw::DrawTarget;
use snuffler_web_core::markers::MarkerId;

/// Snuffler's `MIN_LABEL_SIZE_PT` (`pile_viewer.py:44`): below this the label is
/// drawn only on the hovered row, at exactly this size.
const MIN_LABEL_SIZE_PT: f32 = 6.0;

/// How far past each canvas edge samples are still drawn, so the crossing segment
/// reaches the edge.
const EDGE_MARGIN_PX: f32 = 4.0;

/// Snuffler's label-size ladder (`pile_viewer.py:3505`): the track-label size for rows
/// of `row_height`.
///
/// The font shrinks until a row fits more than one line of it, stopping at 2 pt.
/// Whether the result clears the 6 pt floor decides between labelling every row and
/// labelling only the hovered one.
pub(super) fn fitted_label_size(target: &dyn DrawTarget, row_height: f32) -> f32 {
    let mut label_pt = LABEL_PT;
    while label_pt > 2.0 {
        if row_height / target.measure("0", label_pt).y.max(1.0) > 1.0 {
            break;
        }
        label_pt -= 1.0;
    }
    label_pt
}

/// One spectrogram strip per span of a waterfall row.
#[expect(
    clippy::cast_precision_loss,
    reason = "a strip's time-bin count is far below f64's exact integer range"
)]
pub(super) fn draw_waterfall_strips(target: &mut dyn DrawTarget, frame: &Frame<'_>, row: &Row<'_>) {
    let lay = frame.lay;
    let track = row.track;
    for span in &track.spans {
        let Some(time_bins) = span.count.checked_div(track.frequency_bin_count) else {
            continue;
        };
        let start = lay.data_tmin + span.relative_start_s;
        let x0 = lay.time_to_x(start);
        let x1 = lay.time_to_x(start + time_bins as f64 * span.deltat);
        let rect = Rect::from_min_max(pos2(x0, row.top + 1.0), pos2(x1, row.bottom - 1.0));
        // Keyed by payload offset, not by row: one strip per segment, and a row can
        // carry several.
        target.waterfall(rect, span.offset);
    }
}

/// The row's wiggles, one polyline per span, each followed by its marker snippets.
///
/// `marker_ids` is `None` under marker overload, which skips the snippets.
pub(super) fn draw_wiggles(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    colors: &TraceColors<'_>,
    row: &Row<'_>,
    marker_ids: Option<&[MarkerId]>,
) {
    let Frame {
        state, view, lay, ..
    } = *frame;
    let track_area = lay.track_area;

    // "Clip traces" clips to the same 90 % inner box the trace is scaled into, so a
    // spike is cut exactly at ymin/ymax as Qt cuts it (pile_viewer.py:3186, :3462).
    // Off, the trace may run across the whole track area.
    let (clip_top, clip_bottom) = if state.clip_traces {
        (
            row.middle - row.height * 0.45,
            row.middle + row.height * 0.45,
        )
    } else {
        (track_area.top(), track_area.bottom())
    };
    target.push_clip(Rect::from_min_max(
        pos2(track_area.left(), clip_top),
        pos2(track_area.right(), clip_bottom),
    ));

    let time_margin = f64::from(EDGE_MARGIN_PX / lay.rect.width().max(1.0)) * lay.window_duration;
    // One polyline per span, never one across them: the segments of a gapped
    // channel share a row but must not be joined by a line drawn through data that
    // is not there.
    for span in &row.track.spans {
        let start = lay.data_tmin + span.relative_start_s;
        let color = colors.wiggle(row.track, span, start);
        // Uniform samples only: the bridge chops exactly as snuffler does and drops
        // anything shorter than two samples (pile_viewer.py:3905).
        if span.deltat <= 0.0 {
            continue;
        }
        let samples = view.samples(span);
        if samples.len() < 2 {
            continue;
        }

        let (first_index, end_index) = sample_range(
            start,
            span.deltat,
            samples.len(),
            lay.view_tmin - time_margin,
            lay.view_tmax + time_margin,
        );
        // Every sample in the widened slice is emitted, outside points included, and
        // the clip rect cuts the crossing segment geometrically (as Qt's painter
        // does).
        // Filtering points by x would drop that segment, so at picking zooms the
        // wiggle would detach from both edges.
        #[expect(
            clippy::cast_precision_loss,
            reason = "a sample index is far below f64's exact integer range"
        )]
        let points: Vec<_> = samples
            .get(first_index..end_index)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(offset, &value)| {
                let time = start + (first_index + offset) as f64 * span.deltat;
                pos2(lay.time_to_x(time), row.y_of(span, f64::from(value)))
            })
            .collect();
        if points.len() >= 2 {
            target.polyline(points, 1.0, color);
        }

        if let Some(marker_ids) = marker_ids {
            markers::draw_marker_snippets(
                target,
                frame,
                MarkerSnippet {
                    marker_ids,
                    track: row.track,
                    span,
                    samples,
                    start,
                },
                &|time, value| Some(pos2(lay.time_to_x(time), row.y_of(span, value))),
            );
        }
    }
    target.pop_clip();
}

/// The half-open range of sample indices to draw for a window from `tmin` to
/// `tmax`, for samples every `dt` from `start`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the indices are clamped to the sample count before the cast"
)]
fn sample_range(start: f64, dt: f64, sample_count: usize, tmin: f64, tmax: f64) -> (usize, usize) {
    let first = ((tmin - start) / dt).floor().max(0.0) as usize;
    // floor + 2, not ceil: the range end is exclusive, and it must reach one sample
    // past the right edge so the crossing segment exists.
    // The floor lands on the last sample at or before the edge, and +2 includes the
    // first one beyond it, whether or not the edge sits exactly on the grid.
    let end = (((tmax - start) / dt).floor() + 2.0).clamp(0.0, sample_count as f64) as usize;
    (first, end)
}

/// Station identity at the left, with optional right-aligned location and distance.
///
/// That is snuffler's space-joined gather key, sort value first (`plabel`,
/// `pile_viewer.py:3521`), anchored middle-left at x = 10 as snuffler anchors it.
/// Station locations use the same size and yield when the left label leaves no room.
/// The font shrinks with the row until it would drop below snuffler's 6 pt floor.
/// Below that only the hovered row keeps a label, drawn at the floor size
/// (`pile_viewer.py:3505`, `:3526`).
pub(super) fn draw_track_label(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    row: &Row<'_>,
    fitted_pt: f32,
) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let hovered = state.hovered_track_row == Some(row.index + lay.first_row);
    let (label_pt, drawn) = if fitted_pt >= MIN_LABEL_SIZE_PT {
        (fitted_pt, true)
    } else {
        (MIN_LABEL_SIZE_PT, hovered)
    };
    if !drawn {
        return;
    }
    let anchor = pos2(lay.track_area.left() + 10.0, row.middle);
    if state.show_station_locations && draw_track_location(target, frame, row, label_pt, anchor) {
        return;
    }
    let label = &row.track.label;
    let size = target.measure(label, label_pt);
    let plate = Rect::from_min_size(pos2(anchor.x, anchor.y - size.y * 0.5), size)
        .expand(pal.label_padding);
    target.fill_rect(plate, pal.label_rounding, pal.label_background);
    target.text(anchor, Align2::LEFT_CENTER, label, label_pt, pal.axis_text);
}

/// Keeps distance at the right edge, with geographic context separated by a small gap.
fn draw_track_location(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    row: &Row<'_>,
    label_pt: f32,
    anchor: emath::Pos2,
) -> bool {
    let station = &row.track.station_label;
    let location = row
        .track
        .place
        .as_ref()
        .map(super::axis::station_place_text)
        .unwrap_or_default();
    let distance = &row.track.distance_label;
    if station.is_empty() || (location.is_empty() && distance.is_empty()) {
        return false;
    }
    let Frame { pal, lay, .. } = *frame;
    let station_size = target.measure(station, label_pt);
    let location_size = target.measure(&location, label_pt);
    let distance_size = target.measure(distance, label_pt);
    let gap = if location.is_empty() || distance.is_empty() {
        0.0
    } else {
        14.0
    };
    let right = pos2(lay.track_area.right() - 10.0, row.middle);
    let width = location_size.x + gap + distance_size.x;
    let height = location_size.y.max(distance_size.y);
    let plate_right = Rect::from_min_size(
        pos2(right.x - width, right.y - height * 0.5),
        emath::vec2(width, height),
    )
    .expand(pal.label_padding);
    let plate_left = Rect::from_min_size(
        pos2(anchor.x, anchor.y - station_size.y * 0.5),
        station_size,
    )
    .expand(pal.label_padding);
    if plate_right.left() < plate_left.right() + 12.0 {
        return false;
    }
    target.fill_rect(plate_left, pal.label_rounding, pal.label_background);
    target.text(
        anchor,
        Align2::LEFT_CENTER,
        station,
        label_pt,
        pal.axis_text,
    );
    target.fill_rect(plate_right, pal.label_rounding, pal.label_background);
    if !distance.is_empty() {
        target.text(
            right,
            Align2::RIGHT_CENTER,
            distance,
            label_pt,
            pal.axis_text,
        );
    }
    if !location.is_empty() {
        target.text(
            pos2(right.x - distance_size.x - gap, right.y),
            Align2::RIGHT_CENTER,
            &location,
            label_pt,
            pal.axis_text,
        );
    }
    true
}
