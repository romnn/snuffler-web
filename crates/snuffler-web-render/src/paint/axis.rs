//! The time axis below the track area, following `TimeAx.drawit`
//! (`pile_viewer.py:539`).
//!
//! It uses snuffler's own tick choice and calendar anchoring
//! (`timeaxis::make_ticks`, with its `approx_ticks = 7`), the abbreviation ladder
//! measured against the tick spacing, the second and third context rows centred
//! under the ticks where their unit rolls over, and a left-edge fallback row when
//! none did.
//!
//! Both vertical offsets are snuffler's, rearranged rather than guessed.
//! `drawit` gives the tick label a *baseline* at `vmin + rect0.height() + ticklen`,
//! which puts the top of its glyphs exactly `ticklen` below the top of the axis
//! area.
//! Each row under it stacks by the height of the label above.
//! Measuring a digit stands in for `QFontMetrics.boundingRect`.

use emath::{Align2, Rect, pos2};
use snuffler_web_core::bridge::StationPlace;
use snuffler_web_core::timeaxis;

use super::Frame;
use crate::draw::DrawTarget;

/// Size of the time-axis labels, the same as the track labels.
const AXIS_PT: f32 = 13.5;

/// Length of a time-axis tick mark (`ticklen`, `TimeAx.drawit`, `pile_viewer.py:547`).
const TICK_LENGTH_PX: f32 = 10.0;

/// Clearance between an axis label and the canvas's left and right edges.
const EDGE_PADDING_PX: f32 = 10.0;

const SECONDS_PER_DAY: f64 = 86_400.0;

/// One axis tick with everything the label pass needs to place its labels.
struct PlacedTick {
    time: f64,
    x: f32,
    /// How far right of the tick its first-row label is centred.
    label_shift: f32,
    labels: timeaxis::TickLabels,
}

/// The y of each of the axis's three label rows.
struct LabelRows {
    tick: f32,
    first_context: f32,
    second_context: f32,
}

/// What the label pass drew, for the left-edge fallback to fill in.
struct DrawnLabels {
    first_context: u32,
    second_context: u32,
    first_labelled_tick: Option<f64>,
    occupied: Vec<Rect>,
}

struct AxisGap {
    start: f32,
    end: f32,
}

pub(super) fn draw_time_axis(target: &mut dyn DrawTarget, frame: &Frame<'_>, export: bool) {
    let Frame { pal, lay, .. } = *frame;
    let timeaxis::Ticks {
        times: tick_times,
        increment: tick_increment,
    } = timeaxis::make_ticks(lay.view_tmin, lay.view_tmax, 7.0);
    let axis_top = lay.track_area.bottom();
    let label_height = target.measure("0", AXIS_PT).y;
    let tick_row = axis_top + TICK_LENGTH_PX;
    let first_context_row = tick_row + label_height;
    let rows = LabelRows {
        tick: tick_row,
        first_context: first_context_row,
        second_context: first_context_row + label_height,
    };

    let (ticks, abbreviation_level) =
        draw_tick_marks(target, frame, &tick_times, tick_increment, export);
    let mut drawn = draw_tick_labels(target, frame, &ticks, abbreviation_level, &rows);
    draw_fallback_context(target, frame, &mut drawn, tick_increment, &rows);
    draw_epicentre(target, frame, &drawn.occupied, rows.first_context);

    // The axis baseline snuffler closes `drawit` with (pile_viewer.py:668).
    target.line(
        pos2(lay.rect.left(), axis_top),
        pos2(lay.rect.right(), axis_top),
        1.0,
        pal.axis_text,
    );
}

/// Draws the grid lines and tick marks, and works out each tick's labels.
///
/// Also returns the abbreviation level every first-row label is drawn at.
/// Qt computes the level per tick in its first pass, and its second pass reads the
/// loop variable after that pass has finished, so the last tick's level applies to
/// every label.
/// Label widths at one increment are near-equal, which is what makes it work.
fn draw_tick_marks(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    tick_times: &[f64],
    tick_increment: f64,
    export: bool,
) -> (Vec<PlacedTick>, usize) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let axis_top = lay.track_area.bottom();
    let mut ticks = Vec::with_capacity(tick_times.len());
    let mut abbreviation_level = 0_usize;
    for &time in tick_times {
        let x = lay.time_to_x(time);
        // Screen chrome only: snuffler's savesvg carries no grid, so an exported
        // figure must not either.
        if state.show_time_grid && !export {
            target.line(
                pos2(x, lay.track_area.top()),
                pos2(x, lay.track_area.bottom()),
                1.0,
                pal.grid,
            );
        }
        // The tick mark snuffler always draws, grid or no grid: ten pixels down
        // from the top of the axis area (`ticklen`, pile_viewer.py:546).
        target.line(
            pos2(x, axis_top),
            pos2(x, axis_top + TICK_LENGTH_PX),
            1.0,
            pal.axis_text,
        );

        let mut labels = timeaxis::tick_to_labels(time, tick_increment);
        // Qt hides the year at the epoch on sub-day windows, assuming synthetic
        // data (pile_viewer.py:569).
        if time == 0.0 && lay.window_duration < SECONDS_PER_DAY {
            if labels.l2.is_some() {
                labels.l2 = None;
            } else {
                labels.l1 = None;
            }
        }
        let tick_spacing = (lay.time_to_x(time + tick_increment) - x).max(0.0);
        let label_shift = if labels.l0_center {
            tick_spacing * 0.5
        } else {
            0.0
        };
        abbreviation_level = 0;
        for candidate in [labels.l0.as_str(), labels.l0_brief.as_str(), ""] {
            if target.measure(candidate, AXIS_PT).x <= tick_spacing * 0.9 {
                break;
            }
            abbreviation_level += 1;
        }
        ticks.push(PlacedTick {
            time,
            x,
            label_shift,
            labels,
        });
    }
    (ticks, abbreviation_level)
}

/// Draws each tick's labels where they fit between the canvas edges.
fn draw_tick_labels(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    ticks: &[PlacedTick],
    abbreviation_level: usize,
    rows: &LabelRows,
) -> DrawnLabels {
    let Frame { pal, lay, .. } = *frame;
    let fits = |center: f32, width: f32| {
        lay.rect.left() + EDGE_PADDING_PX < center - width * 0.5
            && center + width * 0.5 < lay.rect.right() - EDGE_PADDING_PX
    };
    let mut drawn = DrawnLabels {
        first_context: 0,
        second_context: 0,
        first_labelled_tick: None,
        occupied: Vec::new(),
    };
    for tick in ticks {
        let tick_label = match abbreviation_level {
            0 => tick.labels.l0.as_str(),
            1 => tick.labels.l0_brief.as_str(),
            _ => "",
        };
        let width = target.measure(tick_label, AXIS_PT).x;
        let center = tick.x + tick.label_shift;
        if fits(center, width) {
            drawn.first_labelled_tick.get_or_insert(tick.time);
            target.text(
                pos2(center, rows.tick),
                Align2::CENTER_TOP,
                tick_label,
                AXIS_PT,
                pal.axis_text,
            );
        }

        // The context rows sit centred under the tick itself.
        let context_rows = [
            (
                &tick.labels.l1,
                rows.first_context,
                &mut drawn.first_context,
            ),
            (
                &tick.labels.l2,
                rows.second_context,
                &mut drawn.second_context,
            ),
        ];
        for (label, y, count) in context_rows {
            let Some(label) = label else {
                continue;
            };
            if fits(tick.x, target.measure(label, AXIS_PT).x) {
                let size = target.measure(label, AXIS_PT);
                drawn
                    .occupied
                    .push(Rect::from_min_size(pos2(tick.x - size.x * 0.5, y), size));
                target.text(
                    pos2(tick.x, y),
                    Align2::CENTER_TOP,
                    label,
                    AXIS_PT,
                    pal.axis_text,
                );
                *count += 1;
            }
        }
    }
    drawn
}

/// The context rows the first labelled tick would carry, at the left edge, for the
/// rows no on-screen tick drew (`pile_viewer.py:636`).
fn draw_fallback_context(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    drawn: &mut DrawnLabels,
    tick_increment: f64,
    rows: &LabelRows,
) {
    let Frame { pal, lay, .. } = *frame;
    let fallback_tick = drawn.first_labelled_tick.unwrap_or(lay.view_tmin);
    let (mut first_context, mut second_context) =
        timeaxis::l1_l2_tick(fallback_tick, tick_increment);
    // The epoch-year suppression again, for the fallback (pile_viewer.py:641).
    let near_epoch = -3600.0 * 25.0 < fallback_tick && fallback_tick <= 3600.0 * 25.0;
    if near_epoch && lay.window_duration < SECONDS_PER_DAY {
        if second_context.is_empty() {
            first_context.clear();
        } else {
            second_context.clear();
        }
    }

    let left = lay.rect.left() + EDGE_PADDING_PX;
    if drawn.first_context == 0 && !first_context.is_empty() {
        drawn.occupied.push(Rect::from_min_size(
            pos2(left, rows.first_context),
            target.measure(&first_context, AXIS_PT),
        ));
        target.text(
            pos2(left, rows.first_context),
            Align2::LEFT_TOP,
            &first_context,
            AXIS_PT,
            pal.axis_text,
        );
    }
    if drawn.second_context == 0 && !second_context.is_empty() {
        drawn.occupied.push(Rect::from_min_size(
            pos2(left, rows.second_context),
            target.measure(&second_context, AXIS_PT),
        ));
        target.text(
            pos2(left, rows.second_context),
            Align2::LEFT_TOP,
            &second_context,
            AXIS_PT,
            pal.axis_text,
        );
    }
}

/// Full station place name, shared by the painted readout and its accessibility label.
#[must_use]
pub fn station_place_text(place: &StationPlace) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in [&place.locality, &place.region, &place.country] {
        let part = part.trim();
        if !part.is_empty()
            && !parts
                .iter()
                .any(|previous| previous.eq_ignore_ascii_case(part))
        {
            parts.push(part);
        }
    }
    parts.join(", ")
}

fn draw_epicentre(target: &mut dyn DrawTarget, frame: &Frame<'_>, occupied: &[Rect], y: f32) {
    let Some(text) = frame.state.epicentre_location(&frame.view.meta) else {
        return;
    };
    let height = target.measure("0", AXIS_PT).y;
    if y + height > frame.lay.rect.bottom() {
        return;
    }
    let left = frame.lay.rect.left() + EDGE_PADDING_PX;
    let right = frame.lay.rect.right() - EDGE_PADDING_PX;
    let mut blocks: Vec<Rect> = occupied
        .iter()
        .copied()
        .filter(|rect| rect.min.y < y + height && rect.max.y > y)
        .collect();
    blocks.sort_by(|a, b| a.left().total_cmp(&b.left()));
    let mut cursor = left;
    let mut gaps = Vec::new();
    for block in blocks {
        if block.left() - 12.0 > cursor {
            gaps.push(AxisGap {
                start: cursor,
                end: block.left() - 12.0,
            });
        }
        cursor = cursor.max(block.right() + 12.0);
    }
    if cursor < right {
        gaps.push(AxisGap {
            start: cursor,
            end: right,
        });
    }
    let Some(AxisGap { start, end }) = gaps
        .into_iter()
        .max_by(|a, b| (a.end - a.start).total_cmp(&(b.end - b.start)))
    else {
        return;
    };
    if target.measure(text, AXIS_PT).x <= end - start {
        target.text(
            pos2(start, y),
            Align2::LEFT_TOP,
            text,
            AXIS_PT,
            frame.pal.axis_text,
        );
    }
}
