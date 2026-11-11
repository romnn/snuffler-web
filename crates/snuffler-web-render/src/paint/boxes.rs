//! Snuffler's trace boxes, the coloured blocks under the wiggles.

use emath::{Rect, pos2};

use super::{Frame, stroke_rect};
use crate::draw::DrawTarget;

/// Snuffler's trace boxes, under every theme.
///
/// There is one per contiguous run of a channel's data, in the style the pile's
/// `determine_box_styles` numbered it with, so a box's colour means the same thing
/// whatever the palette.
///
/// The boxes come from the pile, not from the fetched spans (`draw_trace_boxes`
/// reads `pile.iter_traces`, `pile_viewer.py:2968`), so a channel
/// `min_deltat_allow` declined to fetch still shows where its data lies.
/// Without that, a view zoomed far beyond the data goes blank with nothing left to
/// navigate back to.
/// Past 500 traces in the window the bridge hands each row merged per-style runs
/// whose gaps close, which is `draw_trace_boxes`' own batching.
pub(super) fn draw_trace_boxes(target: &mut dyn DrawTarget, frame: &Frame<'_>) {
    let Frame { pal, lay, .. } = *frame;
    let track_area = lay.track_area;
    target.push_clip(track_area);
    for (index, track) in lay.visible_tracks.iter().enumerate() {
        // Boxes fill the middle 90 % of the row.
        let row_top = lay.row_top(index);
        let top = row_top + lay.row_height * 0.05;
        let bottom = row_top + lay.row_height * 0.95;
        if bottom <= top {
            continue;
        }
        for trace_box in &track.boxes {
            if trace_box.tmax < lay.view_tmin || trace_box.tmin > lay.view_tmax {
                continue;
            }
            let x0 = lay
                .time_to_x(trace_box.tmin)
                .clamp(track_area.left(), track_area.right());
            // Qt strokes the outline with a cosmetic pen, so a box thinner than a
            // pixel still marks its spot.
            // Flooring the width at one pixel is this painter's equivalent.
            let x1 = lay
                .time_to_x(trace_box.tmax)
                .clamp(track_area.left(), track_area.right())
                .max(x0 + 1.0);
            let style = pal.box_style(usize::try_from(trace_box.style).unwrap_or(0));
            let rect = Rect::from_min_max(pos2(x0, top), pos2(x1, bottom));
            target.fill_rect(rect, 0.0, style.fill);
            if let Some(frame_color) = style.frame {
                stroke_rect(target, rect, 1.0, frame_color);
            }
        }
    }
    target.pop_clip();
}
