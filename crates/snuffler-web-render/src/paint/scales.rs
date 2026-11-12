//! Per-row scale annotations and zero lines, following `pile_viewer.py:3345-3434`.
//!
//! Show Scale Axes *replaces* Show Scale Ranges when both are on.
//! Neither draws on a row shorter than about four text lines.
//! Each scale steps `uoff` leftward past the one before it.
//! The numbers take the trace colour when Color Traces is on, and the zero line
//! stops short of the y == 0 tick label rather than running under it.

use ecolor::Color32;
use emath::{Align2, pos2};
use snuffler_web_core::bridge::ViewSpan;

use super::colors::TraceColors;
use super::{Frame, Row};
use crate::draw::DrawTarget;

/// Size of the per-track y-range annotations ("Show scale ranges").
const SCALE_PT: f32 = 10.0;

/// Draws the scale annotations and zero lines of every distinct scale on the row.
pub(super) fn draw_scales(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    colors: &TraceColors<'_>,
    row: &Row<'_>,
) {
    let Frame {
        state, pal, lay, ..
    } = *frame;
    let show_scales = state.show_scale_ranges || state.show_scale_axes;
    if !state.show_zero_lines && !show_scales {
        return;
    }
    let lines_available = row.height / target.measure("0", SCALE_PT).y.max(1.0);

    // `uoff` in Qt: how far left of the track area's right edge the next scale's
    // annotations start.
    let mut offset = 0.0_f32;
    for span in distinct_span_scales(&row.track.spans) {
        let color = colors.scale(row.track, span);
        let mut zero_line_end = lay.track_area.right();
        let mut next_offset = offset;
        if show_scales && lines_available > 3.0 {
            let drawn = if state.show_scale_axes {
                draw_scale_axis(target, frame, row, span, offset, color)
            } else {
                draw_scale_range(target, frame, row, span, offset, color)
            };
            next_offset = next_offset.max(drawn.next_offset);
            if let Some(end) = drawn.zero_line_end {
                zero_line_end = end;
            }
        }
        if state.show_zero_lines {
            let zero_y = row.y_of(span, 0.0);
            // Only inside this row, snuffler's own guard (`vmin <= v <= vmax`): a
            // scale whose zero level lies beyond the row's box draws no line rather
            // than a stray one across a neighbour.
            if zero_y >= row.top && zero_y <= row.bottom {
                let zero_color = if show_scales { color } else { pal.zero };
                target.line(
                    pos2(lay.track_area.left(), zero_y),
                    pos2(zero_line_end, zero_y),
                    1.0,
                    zero_color,
                );
            }
        }
        offset = next_offset;
    }
}

/// Where one scale's annotations ended.
struct DrawnScale {
    /// The offset the next scale starts at.
    next_offset: f32,
    /// Where the zero line stops short of the y == 0 label, if one was drawn.
    zero_line_end: Option<f32>,
}

/// Show Scale Axes: a tick and a number per `ytick_step`, the shared exponent on the
/// topmost number.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the tick count is a handful per row, floored and clamped at zero"
)]
fn draw_scale_axis(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    row: &Row<'_>,
    span: &ViewSpan,
    offset: f32,
    color: Color32,
) -> DrawnScale {
    let right = frame.lay.track_area.right();
    let mut drawn = DrawnScale {
        next_offset: offset,
        zero_line_end: None,
    };
    let step = span.ytick_step;
    if step <= 0.0 || !step.is_finite() {
        return drawn;
    }
    let first_value = (span.ymin / step).ceil() * step;
    let tick_count = ((span.ymax / step).floor() - (span.ymin / step).ceil()).max(0.0) as i64 + 1;
    let factor = 10_f64.powi(span.ytick_exponent);
    for tick_index in 0..tick_count {
        let value = first_value + tick_index as f64 * step;
        let y = row.y_of(span, value);
        target.line(
            pos2(right - 10.0 - offset, y),
            pos2(right - offset, y),
            1.0,
            color,
        );
        // The shared exponent rides the topmost number only (pile_viewer.py:3369).
        let exponent = if tick_index == tick_count - 1 && span.ytick_exponent != 0 {
            format!(" ×10^{}", span.ytick_exponent)
        } else {
            String::new()
        };
        let label = format!("{}{exponent}", format_g(value / factor));
        let width = target.measure(&label, SCALE_PT).x;
        target.text(
            pos2(right - 20.0 - offset, y),
            Align2::RIGHT_CENTER,
            &label,
            SCALE_PT,
            color,
        );
        drawn.next_offset = drawn.next_offset.max(offset + width + 30.0);
        if value == 0.0 {
            drawn.zero_line_end = Some(right - 20.0 - width - 10.0 - offset);
        }
    }
    drawn
}

/// Show Scale Ranges: the minimum bottom-right and the maximum top-right, with the
/// little bracket when the boxes are off (`pile_viewer.py:3396`).
fn draw_scale_range(
    target: &mut dyn DrawTarget,
    frame: &Frame<'_>,
    row: &Row<'_>,
    span: &ViewSpan,
    offset: f32,
    color: Color32,
) -> DrawnScale {
    let right = frame.lay.track_area.right();
    let (top, bottom) = (row.top, row.bottom);
    if !frame.boxes_visible {
        let x0 = right - 20.0 - offset;
        let x1 = right - 10.0 - offset;
        target.line(pos2(x0, bottom), pos2(x1, bottom), 1.0, color);
        target.line(pos2(x1, bottom), pos2(x1, top), 1.0, color);
        target.line(pos2(x1, top), pos2(x0, top), 1.0, color);
    }
    let low = format_g(span.ymin);
    let high = format_g(span.ymax);
    let x = right - 15.0 - offset;
    target.text(pos2(x, bottom), Align2::RIGHT_BOTTOM, &low, SCALE_PT, color);
    target.text(pos2(x, top), Align2::RIGHT_TOP, &high, SCALE_PT, color);
    let width = target
        .measure(&high, SCALE_PT)
        .x
        .max(target.measure(&low, SCALE_PT).x);
    DrawnScale {
        next_offset: offset + width + 10.0,
        zero_line_end: None,
    }
}

/// The distinct amplitude scales a row draws against, in span order.
///
/// That is one for most rows, and several only where a location-grouped row
/// carries traces scaled differently.
/// Zero lines and scale annotations are drawn per scale, not per span: the segments
/// of one gapped channel share a scale and must not draw it twice.
/// A row with no data in the window has none, having no projection to draw against.
fn distinct_span_scales(spans: &[ViewSpan]) -> Vec<&ViewSpan> {
    let mut distinct: Vec<&ViewSpan> = Vec::new();
    for span in spans {
        let same_scale = |other: &&ViewSpan| other.scale_key == span.scale_key;
        if !distinct.iter().any(same_scale) {
            distinct.push(span);
        }
    }
    distinct
}

/// C's `%g` at its default precision, plus snuffler's `num_to_html` rewrite of
/// e-notation into `×10^n` (`pile_viewer.py:150`): the format its scale numbers are
/// printed in.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a finite f64's decimal exponent fits i32, and the decimals are clamped at zero"
)]
fn format_g(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if !value.is_finite() {
        return format!("{value}");
    }
    let exponent = value.abs().log10().floor() as i32;
    let trim_zeros = |mut text: String| {
        if text.contains('.') {
            while text.ends_with('0') {
                text.pop();
            }
            if text.ends_with('.') {
                text.pop();
            }
        }
        text
    };
    if (-4..6).contains(&exponent) {
        let decimals = (5 - exponent).max(0) as usize;
        trim_zeros(format!("{value:.decimals$}"))
    } else {
        let mantissa = value / 10_f64.powi(exponent);
        format!("{} ×10^{exponent}", trim_zeros(format!("{mantissa:.5}")))
    }
}

#[cfg(test)]
mod tests {
    use super::{ViewSpan, distinct_span_scales};

    #[test]
    fn equal_numeric_ranges_keep_distinct_sensor_scales() {
        let spans = [
            ViewSpan {
                scale_key: 1,
                ymin: -1.0,
                ymax: 1.0,
                ..ViewSpan::default()
            },
            ViewSpan {
                scale_key: 2,
                ymin: -1.0,
                ymax: 1.0,
                ..ViewSpan::default()
            },
            ViewSpan {
                scale_key: 1,
                ymin: -1.0,
                ymax: 1.0,
                ..ViewSpan::default()
            },
        ];
        let distinct = distinct_span_scales(&spans);
        assert_eq!(
            distinct
                .iter()
                .map(|span| span.scale_key)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
}
