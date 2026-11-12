//! Pointer input on the canvas: wheel scrolling and track zoom, marker clicks, the
//! pan/zoom drag, the uncertainty drag, and arming a pick by double-click.

use egui::PointerButton;
use snuffler_web_core::markers::{MarkerCandidates, MarkerId};
use snuffler_web_core::state::{AppState, PickingState, ViewDrag};
use snuffler_web_render::layout::Layout;

use super::marker_edits::deselect_all;
use super::{picking, track_zoom};

/// Maximum endpoint candidates inspected for one pointer position.
const MAX_HIT_CANDIDATES: usize = 256;

/// How close to a marker endpoint a pointer hits it, in pixels.
const CLICK_TOLERANCE_PX: f64 = 5.0;

/// What lies under the pointer.
#[derive(Clone, Copy, Debug, Default)]
pub(super) enum MarkerHit {
    #[default]
    None,
    Marker(MarkerId),
    /// Too many marker endpoints lie within reach to tell them apart; the count.
    Overloaded(usize),
}

impl MarkerHit {
    pub(super) fn marker(self) -> Option<MarkerId> {
        match self {
            Self::Marker(id) => Some(id),
            Self::None | Self::Overloaded(_) => None,
        }
    }
}

/// Finds the topmost visible marker under a pointer position.
///
/// This is Qt's `marker_under_cursor` (`pile_viewer.py:2146`): a hit lands within
/// `click_tolerance` (5 px worth of time) of either *endpoint*, never across the
/// span between them, so a wide span marker can be clicked through to reach what
/// lies behind it.
/// The first marker in list order wins, as Qt's loop returns it.
/// The row under the pointer must be one the marker's patterns take.
pub(super) fn marker_hit_at(state: &AppState, lay: &Layout<'_>, position: egui::Pos2) -> MarkerHit {
    if !lay.track_area.contains(position) {
        return MarkerHit::None;
    }
    let canvas_width = f64::from(lay.rect.width().max(1.0));
    let time = lay.view_tmin
        + f64::from((position.x - lay.rect.left()) / lay.rect.width().max(1.0))
            * lay.window_duration;
    let tolerance = CLICK_TOLERANCE_PX * lay.window_duration / canvas_width;
    let ids = match state
        .markers
        .candidates_near(time, tolerance, MAX_HIT_CANDIDATES)
    {
        MarkerCandidates::Exact(ids) => ids,
        MarkerCandidates::Overloaded { count } => return MarkerHit::Overloaded(count),
    };
    let Some(track) = row_at(lay, position.y) else {
        return MarkerHit::None;
    };
    let on_track = state.markers.for_track(track);
    ids.into_iter()
        .find(|&id| {
            state.markers.by_id(id).is_some_and(|marker| {
                state.marker_kind_visible(marker.kind) && on_track.contains(id)
            })
        })
        .map_or(MarkerHit::None, MarkerHit::Marker)
}

/// The visible track at canvas height `y`, if any.
fn row_at<'a>(lay: &Layout<'a>, y: f32) -> Option<&'a snuffler_web_core::bridge::ViewTrack> {
    let offset = y - lay.first_row_top;
    if offset < 0.0 || lay.row_height <= 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "both operands are positive, and truncation to the containing row is the mapping"
    )]
    let index = (offset / lay.row_height) as usize;
    lay.visible_tracks.get(index)
}

/// Every channel gathered onto the row at canvas height `y`.
///
/// This is what Qt's `get_nslc_ids_for_track` hands a pick, so a pick on a grouped
/// row binds to each concrete location on it.
/// A row with no data in the window falls back to its identity pattern.
pub(super) fn row_channels(lay: &Layout<'_>, y: f32) -> Vec<(String, String, String, String)> {
    let Some(track) = row_at(lay, y) else {
        return Vec::new();
    };
    let mut channels: Vec<_> = track
        .spans
        .iter()
        .map(|span| {
            (
                track.net.clone(),
                track.sta.clone(),
                span.loc.clone(),
                track.cha.clone(),
            )
        })
        .collect();
    channels.sort();
    channels.dedup();
    if channels.is_empty() {
        channels.push((
            track.net.clone(),
            track.sta.clone(),
            track.loc.clone(),
            track.cha.clone(),
        ));
    }
    channels
}

/// Handles this frame's pointer input on the waveform canvas.
pub(super) fn handle_pointer(
    ui: &egui::Ui,
    response: &egui::Response,
    state: &mut AppState,
    lay: &Layout<'_>,
    pointer_hit: MarkerHit,
) {
    // The wheel stays live while picking, so stations can scroll mid-batch.
    if response.hovered() {
        scroll_wheel(ui, response, state, lay);
    }

    // Pan, zoom and marker selection are suppressed while picking.
    if state.picking.is_some() {
        picking::update_floating_pick(ui, response, state, lay);
        return;
    }

    let shift = ui.input(|input| input.modifiers.shift);
    let pressed_marker = press_on_marker(ui, state, pointer_hit, shift);
    start_drags(
        response,
        state,
        lay,
        pointer_hit,
        shift,
        pressed_marker.is_some(),
    );
    drag_uncertainty(response, state, lay);
    drag_view(response, state, lay, shift);
    if response.drag_stopped_by(PointerButton::Primary) {
        state.view_drag = None;
        state.uncertainty_drag_last_y = None;
    }
    arm_pick_on_double_click(response, state, lay);
}

/// The wheel: track zoom with Ctrl/⌘ held, track scrolling otherwise.
fn scroll_wheel(ui: &egui::Ui, response: &egui::Response, state: &mut AppState, lay: &Layout<'_>) {
    // Plain wheel scrolling arrives via `smooth_scroll_delta`, but egui re-routes
    // ctrl/⌘+wheel into a zoom gesture (zeroing the scroll delta).
    // Snuffler's ctrl+wheel track zoom is recovered from the raw wheel events,
    // which keep their real modifiers.
    let scroll = ui.input(|input| input.smooth_scroll_delta.y);
    let ctrl_wheel = ui.input(raw_ctrl_wheel);
    let area = lay.track_area;
    let row_height = lay.row_height.max(1.0);

    if ctrl_wheel != 0.0 {
        let anchor = response.hover_pos().map_or(0.5, |position| {
            ((position.y - area.top()) / area.height().max(1.0)).clamp(0.0, 1.0)
        });
        let notches = wheel_notches(state, ctrl_wheel);
        if notches != 0.0 {
            track_zoom::zoom_tracks(state, lay.track_count, anchor, notches);
        }
    } else if scroll != 0.0 {
        if state.snap_tracks {
            // Snapped: a fifth of the shown rows per notch
            let notches = wheel_notches(state, scroll);
            if notches != 0.0 {
                let rows_shown = (area.height() / row_height).max(1.0);
                let step = (rows_shown / 5.0).max(1.0);
                state.track_scroll = (state.track_scroll - step * notches).max(0.0);
            }
        } else {
            // Continuous: scroll by the exact pixel delta, in track units
            state.track_scroll = (state.track_scroll - scroll / row_height).max(0.0);
        }
    }
}

/// Sums the vertical delta (in points) of any ctrl/⌘-held raw wheel events this
/// frame.
///
/// Line and page units convert the way egui itself would on the web: its
/// `InputOptions` are private, but the web `line_scroll_speed` is a fixed 8.0.
fn raw_ctrl_wheel(input: &egui::InputState) -> f32 {
    const LINE_PX: f32 = 8.0;
    let page = input.content_rect().height();
    input
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } if modifiers.ctrl || modifiers.command => Some(match unit {
                egui::MouseWheelUnit::Point => delta.y,
                egui::MouseWheelUnit::Line => LINE_PX * delta.y,
                egui::MouseWheelUnit::Page => page * delta.y,
            }),
            _ => None,
        })
        .sum()
}

/// Accumulates raw wheel motion into Qt-style 120-point notches.
fn wheel_notches(state: &mut AppState, scroll: f32) -> f32 {
    const WHEEL_NOTCH: f32 = 120.0;
    state.wheel_accumulator += scroll;
    let notches = (state.wheel_accumulator / WHEEL_NOTCH).floor();
    state.wheel_accumulator -= notches * WHEEL_NOTCH;
    notches.clamp(-5.0, 5.0)
}

/// Pressing *on* a marker selects it there and then, and returns it.
///
/// Shift adds to the selection instead of replacing it.
fn press_on_marker(
    ui: &egui::Ui,
    state: &mut AppState,
    pointer_hit: MarkerHit,
    shift: bool,
) -> Option<MarkerId> {
    let pressed = ui
        .input(|input| input.pointer.primary_pressed())
        .then(|| pointer_hit.marker())
        .flatten()?;
    if !shift {
        deselect_all(state);
    }
    state.markers.set_selected(pressed, true);
    Some(pressed)
}

/// Latches the drag a primary press starts: a pan/zoom drag, or with Shift an
/// uncertainty drag.
///
/// A press on a marker starts no pan/zoom drag.
/// Qt's press handler is an if/elif chain (`pile_viewer.py:2042`), so a hand tremor
/// while clicking a marker cannot rezoom the view.
fn start_drags(
    response: &egui::Response,
    state: &mut AppState,
    lay: &Layout<'_>,
    pointer_hit: MarkerHit,
    shift: bool,
    pressed_on_marker: bool,
) {
    if !response.drag_started_by(PointerButton::Primary) {
        return;
    }
    let Some(position) = response.interact_pointer_pos() else {
        return;
    };
    if shift {
        // The uncertainty drag is latched at press, so it survives a mid-drag
        // Shift release.
        if let Some(hit) = pointer_hit.marker() {
            state.markers.set_selected(hit, true);
        }
        state.uncertainty_drag_last_y = Some(position.y);
    } else if !pressed_on_marker {
        state.view_drag = Some(ViewDrag {
            x: position.x,
            y: position.y,
            tmin: state.tmin,
            tmax: state.tmax,
            // A drag starting on the time axis pans without zooming.
            pan_only: position.y >= lay.track_area.bottom(),
        });
    }
}

/// Shift+drag adjusts the time uncertainty of the selected phase markers, drawn
/// as ± brackets (snuffler's `set_uncertainty_y`).
///
/// The horizontal seconds-per-pixel is deliberately applied to the vertical drag,
/// exactly like pyrocko.
fn drag_uncertainty(response: &egui::Response, state: &mut AppState, lay: &Layout<'_>) {
    let Some(last_y) = state.uncertainty_drag_last_y else {
        return;
    };
    if !response.dragged_by(PointerButton::Primary) {
        return;
    }
    let Some(position) = response.interact_pointer_pos() else {
        return;
    };
    let seconds_per_px = lay.window_duration / f64::from(lay.rect.width().max(1.0));
    let delta = f64::from(position.y - last_y) * seconds_per_px;
    for marker in state.markers.selected_mut() {
        if marker.phase.is_some() {
            marker.uncertainty = Some((marker.uncertainty.unwrap_or(0.0) + delta).max(0.0));
        }
    }
    state.uncertainty_drag_last_y = Some(position.y);
}

/// The pan/zoom drag: horizontal motion pans, vertical motion zooms about the
/// point the drag started at.
fn drag_view(response: &egui::Response, state: &mut AppState, lay: &Layout<'_>, shift: bool) {
    if !response.dragged_by(PointerButton::Primary)
        || shift
        || state.uncertainty_drag_last_y.is_some()
    {
        return;
    }
    let (Some(drag), Some(position)) = (state.view_drag, response.interact_pointer_pos()) else {
        return;
    };
    let rect = lay.rect;
    let width = rect.width().max(1.0);
    let height = rect.height().max(1.0);
    let start_duration = (drag.tmax - drag.tmin).max(1e-9);
    let pan_fraction = f64::from((position.x - drag.x) / width);
    let zoom_fraction = if drag.pan_only {
        0.0
    } else {
        f64::from((position.y - drag.y) / height)
    };
    let scale = (-zoom_fraction * 5.0).exp();
    let duration_change = scale * start_duration - start_duration;
    let anchor_fraction = f64::from(((drag.x - rect.left()) / width).clamp(0.0, 1.0));
    let shift_seconds = pan_fraction * start_duration * scale;

    state.tmin = drag.tmin - shift_seconds - duration_change * anchor_fraction;
    state.tmax = drag.tmax - shift_seconds + duration_change * (1.0 - anchor_fraction);
}

/// A double-click *arms* single-shot picking (Qt's `mouseDoubleClickEvent` →
/// `start_picking`, `pile_viewer.py:2089`).
///
/// A floating pick follows the cursor and the next press-drag-release commits it,
/// scoped to the track under the cursor, fresh at kind 0 with everything
/// deselected on entry.
/// Qt swallows the release that follows its double-click press; egui reports the
/// double click only after that release, so there is nothing to swallow here.
fn arm_pick_on_double_click(response: &egui::Response, state: &mut AppState, lay: &Layout<'_>) {
    if !response.double_clicked() {
        return;
    }
    let Some(position) = response.interact_pointer_pos() else {
        return;
    };
    deselect_all(state);
    let time = lay.view_tmin
        + f64::from(((position.x - lay.rect.left()) / lay.rect.width()).clamp(0.0, 1.0))
            * lay.window_duration;
    state.picking = Some(PickingState {
        batch_mode: false,
        press_time: None,
        tmin: time,
        tmax: time,
        kind: 0,
        phase: None,
        polarity: None,
        event_hash: None,
        event_time: None,
        nslc_ids: row_channels(lay, position.y),
    });
}
