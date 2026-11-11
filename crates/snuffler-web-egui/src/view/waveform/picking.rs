//! Interactive picking, armed by a double-click (single-shot) or Escape (batch).
//!
//! A floating pick follows the cursor.
//! Press, (drag,) release commits a marker, an instant one if there was no drag.
//! Batch mode then re-arms a new pick inheriting kind, phase and polarity.

use snuffler_web_core::markers::Marker;
use snuffler_web_core::state::{AppState, PickingState};
use snuffler_web_render::layout::Layout;

use super::marker_edits::deselect_all;
use super::pointer::row_channels;

/// Qt clamps the floating pick to the system's working time range
/// (`update_picking`, `pile_viewer.py:4192`); on a modern host that probe lands on
/// years 100..3900.
const WORKING_TMIN: f64 = -59_011_459_200.0;
/// Upper bound of the working time range (see [`WORKING_TMIN`]).
const WORKING_TMAX: f64 = 60_904_915_200.0;

/// Moves the floating pick with the pointer and commits it on release.
pub(super) fn update_floating_pick(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut AppState,
    lay: &Layout<'_>,
) {
    let (pressed, released, press_position) = ui.input(|input| {
        (
            input.pointer.primary_pressed(),
            input.pointer.primary_released(),
            input.pointer.interact_pos(),
        )
    });
    let cursor = response.hover_pos().or(press_position);
    // Unclamped pixel to time: Qt's `time_projection.rev` runs straight through
    // the view's edge, and that overshoot *is* the auto-pan signal.
    let time_at = |x: f32| {
        lay.view_tmin
            + f64::from((x - lay.rect.left()) / lay.rect.width().max(1.0)) * lay.window_duration
    };
    let pick_time_at = |x: f32| time_at(x).clamp(WORKING_TMIN, WORKING_TMAX);

    if let Some(position) = cursor {
        auto_pan(ui, state, time_at(position.x), lay.window_duration);
    }

    if let Some(pick) = state.picking.as_mut() {
        if pressed && let Some(position) = press_position {
            // Press: the pick starts here
            let time = pick_time_at(position.x);
            pick.press_time = Some(time);
            pick.tmin = time;
            pick.tmax = time;
            pick.nslc_ids = row_channels(lay, position.y);
        } else if let Some(press_time) = pick.press_time {
            // Drag: the pick spans from the press to the cursor
            if let Some(position) = cursor {
                let time = pick_time_at(position.x);
                pick.tmin = press_time.min(time);
                pick.tmax = press_time.max(time);
                // Qt re-reads the track under the cursor on every motion
                // (`update_picking` refreshes `nslc_ids`), so a drag that ends on
                // another row scopes the pick to that row.
                pick.nslc_ids = row_channels(lay, position.y);
            }
        } else if let Some(position) = cursor {
            // Hover: the instant pick follows the cursor
            let time = pick_time_at(position.x);
            pick.tmin = time;
            pick.tmax = time;
            pick.nslc_ids = row_channels(lay, position.y);
        }
    }

    if released
        && let Some(pick) = state.picking.clone()
        && pick.press_time.is_some()
    {
        let release_channels = cursor.map(|position| row_channels(lay, position.y));
        commit_pick(state, &pick, release_channels.unwrap_or_default());
    }
}

/// Dragging the pick past the window edge pans the view after it.
///
/// Qt pans by up to a tenth of the window per 50 ms timer tick (`animate_picking`,
/// `pile_viewer.py:4133`).
/// Here it pans per frame, scaled by the frame time so the rate matches.
fn auto_pan(ui: &egui::Ui, state: &mut AppState, cursor_time: f64, window_duration: f64) {
    let overshoot = if cursor_time < state.tmin {
        cursor_time - state.tmin
    } else if cursor_time > state.tmax {
        cursor_time - state.tmax
    } else {
        0.0
    };
    if overshoot == 0.0 {
        return;
    }
    let frame_seconds = f64::from(ui.input(|input| input.stable_dt)).clamp(0.0, 0.2);
    let pan_seconds =
        overshoot.clamp(-window_duration / 10.0, window_duration / 10.0) * (frame_seconds / 0.05);
    state.tmin += pan_seconds;
    state.tmax += pan_seconds;
    // Keep panning while the pointer rests beyond the edge.
    ui.ctx().request_repaint();
}

/// Commits the floating pick as a selected marker, then re-arms it in batch mode.
///
/// The marker is scoped to the channels of the row it was released on, or to the
/// pick's own when it was released off every row.
fn commit_pick(
    state: &mut AppState,
    pick: &PickingState,
    release_channels: Vec<(String, String, String, String)>,
) {
    let nslc_ids = if release_channels.is_empty() {
        pick.nslc_ids.clone()
    } else {
        release_channels
    };
    let mut marker = Marker::new(pick.tmin, pick.tmax, pick.kind);
    marker.phase.clone_from(&pick.phase);
    marker.polarity = pick.polarity;
    marker.event_hash.clone_from(&pick.event_hash);
    marker.event_time = pick.event_time;
    marker.nslc_ids = nslc_ids;
    marker.selected = true;
    deselect_all(state);
    state.markers.insert(marker);

    state.picking = pick.batch_mode.then(|| PickingState {
        batch_mode: true,
        press_time: None,
        tmin: pick.tmin,
        tmax: pick.tmin,
        kind: pick.kind,
        phase: pick.phase.clone(),
        polarity: pick.polarity,
        event_hash: pick.event_hash.clone(),
        event_time: pick.event_time,
        nslc_ids: Vec::new(),
    });
    if state.picking.is_some() {
        // Qt's batch re-arm passes through `start_picking`, whose `deselect_all`
        // covers the marker just committed: only a single-shot pick stays
        // selected.
        deselect_all(state);
    }
}
