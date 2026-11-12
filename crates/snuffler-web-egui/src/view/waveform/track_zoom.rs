//! Track zoom in visible rows, anchored at the pointer.

use snuffler_web_core::state::AppState;

/// Ctrl+wheel changes the visible count while keeping the track under the cursor fixed.
///
/// `anchor` is the cursor's position down the track area, `0..=1`.
/// Positive `notches` (wheel down) reveal more tracks.
pub(super) fn zoom_tracks(state: &mut AppState, track_count: usize, anchor: f32, notches: f32) {
    let rows_shown = state.visible_track_count(track_count);
    // Qt changes the shown count by max(1, shown/5) per notch (pile_viewer.py:2622).
    let step = (rows_shown / 5.0).max(1.0);
    state.tracks_per_page = Some((rows_shown + step * notches).max(1.0));
    let new_rows_shown = state.visible_track_count(track_count);
    state.tracks_per_page = Some(new_rows_shown);
    let anchor = anchor.clamp(0.0, 1.0);
    let row_at_cursor = state.track_scroll + anchor * rows_shown;
    state.track_scroll = (row_at_cursor - anchor * new_rows_shown).max(0.0);
}
