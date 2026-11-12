//! Markers: which rows they belong on, the snippet highlights inside the trace
//! pass, the per-track and overview layers, and the floating pick.

use ecolor::Color32;
use emath::{Align, Align2, Pos2, Rect, Vec2, pos2};
use snuffler_web_core::bridge::{ViewSpan, ViewTrack};
use snuffler_web_core::markers::{Marker, MarkerCandidates, MarkerId};
use snuffler_web_core::state::AppState;

use super::{Frame, LABEL_PT, stroke_rect};
use crate::draw::DrawTarget;

/// The most markers snuffler draws one by one: the 2000 of `with_key_in_limited`
/// (`pile_viewer.py:3027`).
const MAX_DRAWN_MARKERS: usize = 2000;

/// The markers to draw one by one, or `None` under overload.
///
/// Overload is more markers in the fetch window than snuffler draws one by one,
/// the window widened by the longest marker span (exactly `markers_deltat_max`).
///
/// Qt limits only its overview layer with this: the per-trace loop iterates the
/// unlimited `with_key_in` (`pile_viewer.py:3480`), so above the threshold Qt still
/// pays for, and draws, every snippet and per-track vertical.
/// Here the same verdict silences those passes too.
/// That is a documented divergence (`marker-overload-full-quiet` in the known-gaps
/// register), because the browser repaints the whole view on every interaction
/// frame, and a markers × spans pass per frame is exactly the cost the count
/// readout exists to avoid.
pub(super) fn candidates(state: &AppState) -> Option<Vec<MarkerId>> {
    if state.markers.overload_count(state.tmin, state.tmax) > MAX_DRAWN_MARKERS {
        return None;
    }
    match state
        .markers
        .overlapping(state.tmin, state.tmax, MAX_DRAWN_MARKERS)
    {
        MarkerCandidates::Exact(ids) => Some(ids),
        MarkerCandidates::Overloaded { .. } => Some(Vec::new()),
    }
}

/// Resolves marker ids to the markers they name, skipping any no longer stored.
fn resolve<'a>(
    state: &'a AppState,
    marker_ids: &'a [MarkerId],
) -> impl Iterator<Item = (MarkerId, &'a Marker)> {
    marker_ids
        .iter()
        .filter_map(|&id| state.markers.by_id(id).map(|marker| (id, marker)))
}

/// Qt's visibility filter, applied per marker in every layer: strict window overlap
/// and a kind the user has not hidden (`pile_viewer.py:3067`).
fn is_shown(state: &AppState, marker: &Marker) -> bool {
    marker.tmin < state.tmax && state.tmin < marker.tmax && state.marker_kind_visible(marker.kind)
}

/// The samples of one span, for [`draw_marker_snippets`].
#[derive(Clone, Copy)]
pub(super) struct MarkerSnippet<'a> {
    pub(super) marker_ids: &'a [MarkerId],
    pub(super) track: &'a ViewTrack,
    pub(super) span: &'a ViewSpan,
    pub(super) samples: &'a [f32],
    /// Absolute time of the first sample.
    pub(super) start: f64,
}

/// The snippet-highlight half of Qt's `Marker.draw_trace` (`marker.py:381`).
///
/// Under a span marker the covered samples are re-drawn in the marker's colour at
/// width 1, with outlined 4x4 squares on the first and last covered sample.
/// An instantaneous pick gets one square at its interpolated amplitude.
/// This runs inside the trace pass because that is where the samples and their y
/// projection live, exactly where Qt calls it.
/// `project` maps an absolute time and a sample value to the canvas.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "sample indices are clamped to the snippet before and after the casts"
)]
pub(super) fn draw_marker_snippets(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    snippet: MarkerSnippet<'_>,
    project: &impl Fn(f64, f64) -> Option<Pos2>,
) {
    let Frame { state, pal, .. } = *frame;
    let MarkerSnippet {
        marker_ids,
        track,
        span,
        samples,
        start,
    } = snippet;
    let Some(last) = samples.len().checked_sub(1) else {
        return;
    };
    let sample_time = |index: usize| start + index as f64 * span.deltat;
    let sample_point = |index: usize| {
        let &value = samples.get(index)?;
        project(sample_time(index), f64::from(value))
    };

    // Qt chops with `snap=(ceil, floor)` and `include_last`: the first sample at or
    // after tmin through the last at or before tmax, clipped to the trace.
    // Its `drawpoint(..., clip=True)` squares land on the trace's own end samples
    // when the marker runs past them.
    // A marker that misses the segment entirely raises `NoData` in Qt's chop and
    // draws nothing.
    // Clamping it onto the segment instead would fabricate an endpoint square on the
    // final sample of every earlier segment of a gapped channel.
    let chop = |marker: &Marker| {
        let first = ((marker.tmin - start) / span.deltat).ceil().max(0.0) as usize;
        let last_covered = ((marker.tmax - start) / span.deltat).floor();
        if last_covered < first as f64 || first > last {
            return None;
        }
        Some((first, (last_covered as usize).min(last)))
    };

    let on_trace = state.markers.for_trace(track, &span.loc);
    for (id, marker) in resolve(state, marker_ids) {
        // The same visibility filter the marker layers apply, plus Qt's per-trace
        // pattern match against this concrete trace.
        if marker.is_event || !is_shown(state, marker) || !on_trace.contains(id) {
            continue;
        }
        let color = marker.color_in(
            &pal.markers,
            state.markers.is_selected(id) || state.hovered_marker == Some(id),
        );

        if marker.is_span() {
            // Span marker: re-draw the covered samples, square both ends
            let Some((first, last_covered)) = chop(marker) else {
                continue;
            };
            let points: Vec<_> = (first..=last_covered).filter_map(sample_point).collect();
            if points.len() >= 2 {
                target.polyline(points, 1.0, color);
            }
            for index in [first, last_covered] {
                if let Some(point) = sample_point(index) {
                    draw_point_square(target, point, color);
                }
            }
        } else {
            // Instantaneous pick: `tr.interpolate(tmin)`, linear between the
            // neighbouring samples
            let position = (marker.tmin - start) / span.deltat;
            if position < 0.0 || position > last as f64 {
                continue;
            }
            let below = (position.floor() as usize).min(last.saturating_sub(1));
            let fraction = position - below as f64;
            if let (Some(&low), Some(&high)) =
                (samples.get(below), samples.get((below + 1).min(last)))
            {
                let value = f64::from(low) * (1.0 - fraction) + f64::from(high) * fraction;
                if let Some(point) = project(marker.tmin, value) {
                    draw_point_square(target, point, color);
                }
            }
        }
    }
}

/// Qt's `drawpoint`: an unfilled 4x4 rect under a width-2 pen.
fn draw_point_square(target: &mut dyn DrawTarget, center: Pos2, color: Color32) {
    let rect = Rect::from_center_size(center, Vec2::splat(4.0));
    stroke_rect(target, rect, 2.0, color);
}

/// Draws a straight dashed line by emitting the on-segments one by one.
///
/// The draw targets have no dash primitive, and Qt's marker guides need the
/// selected dash (`setDashPattern([5, 3])`, `marker.py:344`) to read as selected.
/// `dash` and `gap` are in pixels; Qt specifies its patterns in units of the pen
/// width, so callers scale by `width` first.
fn dashed_line(
    target: &mut dyn DrawTarget,
    from: Pos2,
    to: Pos2,
    width: f32,
    color: Color32,
    dash: f32,
    gap: f32,
) {
    let delta = to - from;
    let length = delta.length();
    if length <= f32::EPSILON || dash <= 0.0 {
        target.line(from, to, width, color);
        return;
    }
    let direction = delta / length;
    let mut dash_start = 0.0;
    while dash_start < length {
        let dash_end = (dash_start + dash).min(length);
        target.line(
            from + direction * dash_start,
            from + direction * dash_end,
            width,
            color,
        );
        dash_start = dash_end + gap;
    }
}

/// How [`draw_marker_label`] draws one label.
struct MarkerLabel<'a> {
    anchor: Pos2,
    align: Align2,
    text: &'a str,
    color: Color32,
    /// For the active event or its picks: the plate widened and outlined.
    outlined: bool,
}

/// Draws a marker label the way `gui_util.draw_label` does.
///
/// The text is in the marker's colour on a plate
/// ([`crate::palette::Palette::marker_label_background`], Qt's white under the Qt
/// theme).
/// For the active event or its picks, the plate is widened by 2 px each side and
/// outlined instead of merely filled.
fn draw_marker_label(target: &mut dyn DrawTarget, plate_color: Color32, label: &MarkerLabel<'_>) {
    let MarkerLabel {
        anchor,
        align,
        text,
        color,
        outlined,
    } = *label;
    let size = target.measure(text, LABEL_PT);
    let left = match align.x() {
        Align::Min => anchor.x,
        Align::Center => anchor.x - size.x * 0.5,
        Align::Max => anchor.x - size.x,
    };
    let top = match align.y() {
        Align::Min => anchor.y,
        Align::Center => anchor.y - size.y * 0.5,
        Align::Max => anchor.y - size.y,
    };
    let mut plate = Rect::from_min_size(pos2(left, top), size);
    if outlined {
        // Qt widens the outlined plate (`rect.adjust(-2, 0, 2, 0)`) and strokes it
        // with the marker pen.
        plate = Rect::from_min_max(
            pos2(plate.left() - 2.0, plate.top()),
            pos2(plate.right() + 2.0, plate.bottom()),
        );
        target.fill_rect(plate, 0.0, plate_color);
        stroke_rect(target, plate, 1.0, color);
    } else {
        target.fill_rect(plate, 0.0, plate_color);
    }
    target.text(anchor, align, text, LABEL_PT, color);
}

/// The label a phase marker carries, composed as Qt composes it with no separators
/// (`PhaseMarker.get_label`, `marker.py:707`): phase name, then the polarity arrow.
///
/// Events label themselves separately (`EventMarker.label`).
fn phase_label(marker: &Marker) -> String {
    let mut label = marker.phase.clone().unwrap_or_default();
    label.push_str(marker.polarity_symbol());
    if marker.automatic == Some(true) {
        label.push('@');
    }
    label
}

/// Draws every visible marker the way snuffler layers them (`drawit`,
/// `pile_viewer.py:3476`).
///
/// First come the per-track parts each marker paints inside its matching rows
/// ([`draw_per_track_layer`]), then the overview parts painted across the whole
/// track area ([`draw_overview_layer`]).
/// The snippet-highlight half of `draw_trace` lives in the trace pass, which is
/// where the samples and their projection are.
///
/// `marker_ids` is the [`candidates`] verdict: under overload (`None`) both layers
/// give way to the dotted overview line and the count readout.
pub(super) fn draw_markers(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    marker_ids: Option<&[MarkerId]>,
) {
    match marker_ids {
        None => draw_overload_readout(target, frame),
        Some(marker_ids) => {
            draw_per_track_layer(target, frame, marker_ids);
            draw_overview_layer(target, frame, marker_ids);
        }
    }
}

/// Qt's `TooMany` fallback (`pile_viewer.py:3030`): a dotted overview line plus a
/// count.
///
/// A catalog load or a picker run over a long window reaches it easily.
fn draw_overload_readout(target: &mut dyn DrawTarget, frame: &Frame<'_>) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let area = lay.track_area;
    let summary = state.markers.summary();
    let total = state.markers.len();
    let selected = summary.selected;
    let suffix = if selected == total {
        " (all selected)".to_owned()
    } else if selected > 0 {
        format!(" ({selected} selected)")
    } else {
        String::new()
    };
    // Qt takes the first marker's tmin and the last's tmax.
    // Its list is sorted by tmin, so that is the earliest start and the end of the
    // latest-starting marker.
    let first_x = lay
        .time_to_x(summary.earliest_marker_start)
        .max(area.left());
    let last_x = lay
        .time_to_x(summary.latest_starting_marker_end)
        .min(area.right());
    // DotLine on a width-2 pen: 2 px dots, 4 px gaps.
    dashed_line(
        target,
        pos2(first_x, area.top()),
        pos2(last_x, area.top()),
        2.0,
        pal.axis_text,
        2.0,
        4.0,
    );
    draw_marker_label(
        target,
        pal.marker_label_background,
        &MarkerLabel {
            anchor: pos2(first_x + 10.0, area.top() - 10.0),
            align: Align2::LEFT_BOTTOM,
            text: &format!("{total} Markers{suffix}"),
            color: pal.axis_text,
            outlined: false,
        },
    );
}

/// The per-track layer (`Marker.draw_trace`): uncertainty boxes, verticals at both
/// ends and the label, on every row the marker matches.
///
/// Events draw nothing here (`EventMarker.draw_trace` is a pass).
/// An unscoped marker matches every trace, so it paints these on every row, exactly
/// as Qt does.
fn draw_per_track_layer(target: &mut dyn DrawTarget, frame: &Frame<'_>, marker_ids: &[MarkerId]) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let active_event_hash = state.markers.summary().active_hash;
    let row_markers: Vec<_> = lay
        .visible_tracks
        .iter()
        .map(|track| state.markers.for_track(track))
        .collect();
    for (id, marker) in resolve(state, marker_ids) {
        if marker.is_event || !is_shown(state, marker) {
            continue;
        }
        let color = marker.color_in(
            &pal.markers,
            state.markers.is_selected(id) || state.hovered_marker == Some(id),
        );
        let x_min = lay.time_to_x(marker.tmin);
        let x_max = lay.time_to_x(marker.tmax);
        let label = phase_label(marker);
        // A pick made on the active event outlines its label and shifts it two
        // pixels further left (marker.py:444).
        let outlined = marker.event_hash.is_some() && marker.event_hash == active_event_hash;
        let label_shift = if outlined { -7.0 } else { -5.0 };

        let matching_rows = row_markers
            .iter()
            .enumerate()
            .filter(|(_, members)| members.contains(id))
            .map(|(index, _)| index);
        for index in matching_rows {
            let top = lay.row_top(index);
            let bottom = top + lay.row_height;
            // Phase-pick uncertainty as translucent boxes under everything else
            // (`PhaseMarker.draw_trace`, marker.py:687): side bands at alpha 50, the
            // span itself at 35, full row height.
            if let Some(uncertainty) = marker.uncertainty
                && uncertainty > 0.0
            {
                let mut band = |from: f32, to: f32, alpha: u8| {
                    target.fill_rect(
                        Rect::from_min_max(pos2(from.min(to), top), pos2(from.max(to), bottom)),
                        0.0,
                        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                    );
                };
                band(lay.time_to_x(marker.tmin - uncertainty), x_min, 50);
                band(x_max, lay.time_to_x(marker.tmax + uncertainty), 50);
                band(x_min, x_max, 35);
            }
            // Row-height verticals at *both* ends (marker.py:403): the tmax line is
            // the only thing showing where a span marker ends.
            target.line(pos2(x_min, top), pos2(x_min, bottom), 2.0, color);
            target.line(pos2(x_max, top), pos2(x_max, bottom), 2.0, color);
            if !label.is_empty() {
                draw_marker_label(
                    target,
                    pal.marker_label_background,
                    &MarkerLabel {
                        anchor: pos2(x_min + label_shift, top),
                        align: Align2::RIGHT_TOP,
                        text: &label,
                        color,
                        outlined,
                    },
                );
            }
        }
    }
}

/// The overview layer (`Marker.draw` via `draw_visible_markers`), painted on top.
///
/// That is the short span line at the top edge, the full-height guides of
/// selected, hovered and unscoped markers, and the event triangles and captions.
fn draw_overview_layer(target: &mut dyn DrawTarget, frame: &Frame<'_>, marker_ids: &[MarkerId]) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let top = lay.track_area.top();
    let bottom = lay.track_area.bottom();
    for (id, marker) in resolve(state, marker_ids) {
        if !is_shown(state, marker) {
            continue;
        }
        let hovered = state.hovered_marker == Some(id);
        let color = marker.color_in(&pal.markers, state.markers.is_selected(id) || hovered);
        let x_min = lay.time_to_x(marker.tmin);
        let x_max = lay.time_to_x(marker.tmax);
        // The short span line every marker gets at the top of the track area
        // (marker.py:335), one pixel proud of each end.
        target.line(pos2(x_min - 1.0, top), pos2(x_max + 1.0, top), 2.0, color);

        let selected = state.markers.is_selected(id);
        if selected || hovered || marker.nslc_ids.is_empty() {
            // Full-height guides at both ends: dashed 5-3 (in pen widths) when
            // selected or hovered, grey when hovered but unselected.
            // The dash is what shows which markers the next keystroke acts on.
            // An event draws them only when selected or hovered (`draw_line=False`).
            let dashed = selected || hovered;
            let guide_color = if hovered && !selected {
                Color32::from_rgb(150, 150, 150)
            } else {
                color
            };
            if !marker.is_event || dashed {
                for x in [x_min, x_max] {
                    if dashed {
                        dashed_line(
                            target,
                            pos2(x, top),
                            pos2(x, bottom),
                            2.0,
                            guide_color,
                            10.0,
                            6.0,
                        );
                    } else {
                        target.line(pos2(x, top), pos2(x, bottom), 2.0, guide_color);
                    }
                }
            }
            if marker.is_event {
                draw_event_triangles(target, x_min, top, bottom, color);
            }
        }

        if marker.is_event {
            // The event caption, centred above the top edge with its baseline 10 px
            // up (`EventMarker.draw_label`, marker.py:561), outlined for the active
            // event.
            draw_marker_label(
                target,
                pal.marker_label_background,
                &MarkerLabel {
                    anchor: pos2(x_min, top - 10.0),
                    align: Align2::CENTER_BOTTOM,
                    text: &marker.event_label(),
                    color,
                    outlined: marker.active,
                },
            );
        }
    }
}

/// Qt's paired event triangles at tmin (marker.py:349): 9 px tall with 0.577·s
/// half-width, hanging from the top edge and standing on the bottom one.
fn draw_event_triangles(
    target: &mut dyn DrawTarget,
    x: f32,
    top: f32,
    bottom: f32,
    color: Color32,
) {
    let size = 9.0;
    let half_width = 0.577 * size;
    target.polygon(
        vec![
            pos2(x - half_width, top),
            pos2(x, top + size),
            pos2(x + half_width, top),
        ],
        color,
    );
    target.polygon(
        vec![
            pos2(x - half_width, bottom),
            pos2(x, bottom - size),
            pos2(x + half_width, bottom),
        ],
        color,
    );
}

/// The floating pick of batch and interactive picking: a vertical line (instant) or
/// a span rect (drag), coloured by the pick's kind.
///
/// The x-range guard skips the un-hovered initial state, whose tmin of 0 maps far
/// off-screen.
pub(super) fn draw_floating_pick(target: &mut dyn DrawTarget, frame: &Frame<'_>) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let Some(pick) = &state.picking else {
        return;
    };
    let area = lay.track_area;
    let x0 = lay.time_to_x(pick.tmin);
    if !(area.left() - 2.0..=area.right() + 2.0).contains(&x0) {
        return;
    }
    let color = pal.markers.kind(pick.kind, true);
    if pick.tmax > pick.tmin {
        let x1 = lay.time_to_x(pick.tmax);
        target.fill_rect(
            Rect::from_min_max(pos2(x0, area.top()), pos2(x1, area.bottom())),
            0.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40),
        );
    }
    target.line(pos2(x0, area.top()), pos2(x0, area.bottom()), 2.0, color);
}
