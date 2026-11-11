//! The vertical scrollbar of the track stack.

use egui::{Color32, Rect, Sense};
use snuffler_web_core::state::AppState;
use snuffler_web_render::palette::Theme;

/// A vertical scrollbar for the track stack, drawn in the reserved right-hand strip.
///
/// Dragging the thumb or clicking the trough moves `track_scroll`.
/// In snapped mode the next frame's layout rounds the result to whole tracks.
pub(super) fn draw_scrollbar(
    ui: &egui::Ui,
    id: egui::Id,
    strip: Rect,
    state: &mut AppState,
    theme: Theme,
    track_count: usize,
    row_height: f32,
) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a track count is far below f32's exact integer range"
    )]
    let track_count = track_count.max(1) as f32;
    let strip_height = strip.height().max(1.0);
    let rows_in_strip = (strip_height / row_height.max(1.0)).max(1.0);
    let max_scroll = (track_count - rows_in_strip).max(0.0);
    if max_scroll <= 0.0 {
        return;
    }

    // Input
    let response = ui.interact(strip, id, Sense::click_and_drag());
    let visible_fraction = (rows_in_strip / track_count).clamp(0.05, 1.0);
    let thumb_height =
        (visible_fraction * strip_height).clamp(24.0_f32.min(strip_height), strip_height);
    let thumb_top = |scroll: f32| {
        let scroll_fraction = (scroll / max_scroll).clamp(0.0, 1.0);
        strip.top() + scroll_fraction * (strip_height - thumb_height)
    };
    if response.dragged() {
        let rows_per_px = max_scroll / (strip_height - thumb_height).max(1.0);
        state.track_scroll =
            (state.track_scroll + response.drag_delta().y * rows_per_px).clamp(0.0, max_scroll);
    } else if response.clicked()
        && let Some(position) = response.interact_pointer_pos()
    {
        // A click on the trough pages towards it.
        let top = thumb_top(state.track_scroll);
        if position.y < top {
            state.track_scroll = (state.track_scroll - rows_in_strip).max(0.0);
        } else if position.y > top + thumb_height {
            state.track_scroll = (state.track_scroll + rows_in_strip).min(max_scroll);
        }
    }

    // Drawing, at the scroll the input just settled
    let top = thumb_top(state.track_scroll);
    let thumb = Rect::from_min_max(
        egui::pos2(strip.left() + 2.0, top),
        egui::pos2(strip.right() - 2.0, top + thumb_height),
    );
    let (trough, thumb_idle, thumb_active) = if theme == Theme::Dark {
        (
            Color32::from_gray(38),
            Color32::from_gray(96),
            Color32::from_gray(140),
        )
    } else {
        (
            Color32::from_gray(222),
            Color32::from_gray(168),
            Color32::from_gray(128),
        )
    };
    let painter = ui.painter();
    painter.rect_filled(strip, 0.0, trough);
    let thumb_color = if response.hovered() || response.dragged() {
        thumb_active
    } else {
        thumb_idle
    };
    painter.rect_filled(thumb, 3.0, thumb_color);
}
