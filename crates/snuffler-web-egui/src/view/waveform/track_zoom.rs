//! Track zoom: how many rows the canvas shows, set as a track height.

use snuffler_web_core::state::{
    AppState, DEFAULT_NTRACKS_SHOWN, MAX_TRACK_HEIGHT_PX, MIN_TRACK_HEIGHT_PX,
};

/// The current resolved track height in pixels, clamped to the slider bounds.
///
/// The sentinel `0.0` auto-fits [`DEFAULT_NTRACKS_SHOWN`] rows.
pub(super) fn resolved_height(state: &AppState, area_height: f32) -> f32 {
    let area = area_height.max(1.0);
    #[expect(
        clippy::cast_precision_loss,
        reason = "the default row count is a small constant"
    )]
    let height = if state.track_height > 0.0 {
        state.track_height
    } else {
        area / DEFAULT_NTRACKS_SHOWN as f32
    };
    height.clamp(MIN_TRACK_HEIGHT_PX, area.max(MIN_TRACK_HEIGHT_PX))
}

/// The tallest track height, which grows with the area.
///
/// Qt clamps the shown *count* to [1, ntracks] (`pile_viewer.py:2717`), so a
/// single-track view must be reachable however tall the canvas is: the cap grows
/// with the track area rather than stopping at [`MAX_TRACK_HEIGHT_PX`].
pub(super) fn max_height(area_height: f32) -> f32 {
    MAX_TRACK_HEIGHT_PX.max(area_height)
}

/// Ctrl+wheel track zoom: changes the track height, and hence how many tracks are
/// shown, while keeping the track under the cursor fixed.
///
/// `anchor` is the cursor's position down the track area, `0..=1`.
/// Positive `notches` (wheel down) shrink rows to reveal more tracks.
pub(super) fn zoom_tracks(state: &mut AppState, area_height: f32, anchor: f32, notches: f32) {
    let area = area_height.max(1.0);
    let rows_shown = (area / resolved_height(state, area)).max(1.0);
    // Qt changes the shown count by max(1, shown/5) per notch (pile_viewer.py:2622).
    let step = (rows_shown / 5.0).max(1.0);
    let new_rows_shown = (rows_shown + step * notches).max(1.0);
    let new_height = (area / new_rows_shown).clamp(MIN_TRACK_HEIGHT_PX, max_height(area));
    let anchor = anchor.clamp(0.0, 1.0);
    let row_at_cursor = state.track_scroll + anchor * rows_shown;
    state.track_scroll = (row_at_cursor - anchor * (area / new_height)).max(0.0);
    state.track_height = new_height;
}
