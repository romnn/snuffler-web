//! Marker edits the canvas's keys and clicks make: selection, navigation, kind,
//! phase, polarity, and the active event.

use std::cmp::Ordering;

use snuffler_web_core::bridge::ViewTrack;
use snuffler_web_core::markers::Marker;
use snuffler_web_core::session::{TimeJump, TimeWindow, good_looking_time_range};
use snuffler_web_core::state::{ActiveEventRequest, AppState};
use snuffler_web_render::paint::marker_matches_track;

/// Clears the marker selection.
pub(super) fn deselect_all(state: &mut AppState) {
    for marker in &mut state.markers {
        marker.selected = false;
    }
}

/// Snuffler's `a` and `A` (`pile_viewer.py:2381-2392`); both skip hidden kinds.
///
/// `A` (`additive`) only ever *sets* selected.
/// `a` is absolute, keeping exactly the markers with an endpoint inside the view,
/// so a span wider than the screen is dropped, as Qt drops it.
pub(super) fn select_all(state: &mut AppState, additive: bool) {
    let kinds = state.visible_marker_kinds.clone();
    let (tmin, tmax) = (state.tmin, state.tmax);
    for marker in &mut state.markers {
        if additive {
            if kinds.contains(&marker.kind) {
                marker.selected = true;
            }
        } else {
            let endpoint_in_view = (tmin <= marker.tmin && marker.tmin <= tmax)
                || (tmin <= marker.tmax && marker.tmax <= tmax);
            marker.selected = endpoint_in_view && kinds.contains(&marker.kind);
        }
    }
}

/// Converts a plain marker to an event, as Qt's `convert_to_event_marker` does
/// (`marker.py:495`).
///
/// The span collapses to its start and the channel scope is dropped, since an
/// event is an instant that belongs to no track.
fn convert_to_event(marker: &mut Marker) {
    marker.is_event = true;
    marker.phase = None;
    marker.uncertainty = None;
    marker.polarity = None;
    marker.tmax = marker.tmin;
    marker.nslc_ids.clear();
}

/// `E`: deactivates every event, and asks Python to drop the active event too.
pub(super) fn deactivate_events(state: &mut AppState) {
    for marker in &mut state.markers {
        marker.active = false;
    }
    state.set_active_event_request = Some(ActiveEventRequest {
        deactivate: true,
        ..Default::default()
    });
}

/// `e`: makes the single selected marker the *active* event (snuffler's
/// `set_active_event_marker`).
///
/// The active event is the distance, Kagan, cake and seismosizer reference.
/// The marker converts to an event if needed, any other event is deactivated, the
/// selected phase picks are associated with it, and Python is asked to recompute
/// distances and Kagan angles from it.
/// Several selected plain markers convert to events without activating.
/// The bridge call is deferred to the App.
pub(super) fn activate_selected_event(state: &mut AppState) {
    // Candidates for activation are selected markers that are already events or
    // plain markers that can convert, never a selected phase pick.
    let candidates: Vec<usize> = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| marker.selected && (marker.is_event || marker.phase.is_none()))
        .map(|(index, _)| index)
        .collect();
    let &[index] = candidates.as_slice() else {
        for &index in &candidates {
            if let Some(marker) = state.markers.get_mut(index) {
                convert_to_event(marker);
            }
        }
        return;
    };

    // Qt snapshots the marker's station before conversion removes its track scope,
    // then uses that station to locate the event.
    // It does so only when the marker has exactly one channel (`len(nslcs) == 1`,
    // pile_viewer.py:2416).
    let station_nslc = state.markers.get(index).and_then(|marker| {
        (marker.nslc_ids.len() == 1)
            .then(|| marker.nslc_ids.first().cloned())
            .flatten()
    });
    for marker in &mut state.markers {
        marker.active = false;
    }
    if let Some(marker) = state.markers.get_mut(index) {
        if !marker.is_event {
            convert_to_event(marker);
        }
        // Every Qt event has an identity (`get_event_hash` falls back to hashing
        // the event's own fields), and the picks' association rides on it.
        // So a locally created event synthesizes one from its origin time.
        if marker.event_hash.is_none() {
            marker.event_hash = Some(format!("local:{:x}", marker.tmin.to_bits()));
        }
        marker.active = true;
    }

    let (event_hash, event_time) = state.markers.get(index).map_or((None, 0.0), |marker| {
        (marker.event_hash.clone(), marker.tmin)
    });
    for marker in &mut state.markers {
        if marker.selected && marker.phase.is_some() {
            marker.event_hash.clone_from(&event_hash);
            marker.event_time = Some(event_time);
        }
    }
    state.set_active_event_request = Some(ActiveEventRequest {
        deactivate: false,
        event_hash,
        time: event_time,
        nslc: station_nslc,
    });
}

/// Up/Down first-motion polarity, Qt's exact gate (`pile_viewer.py:2233`).
///
/// Only *phase* markers take one.
/// A repeated press in the same direction clears it again, and Shift
/// (`undecided`) forces "undecided".
/// The floating pick takes precedence over the selection
/// (`floating_or_selected_markers`), so a polarity chosen mid-pick carries into the
/// next batch pick.
pub(super) fn set_polarity(state: &mut AppState, up: bool, undecided: bool) {
    let toggle = |current: Option<i8>| -> Option<i8> {
        if undecided {
            Some(0)
        } else {
            let wanted = if up { 1 } else { -1 };
            if current == Some(wanted) {
                None
            } else {
                Some(wanted)
            }
        }
    };
    if let Some(pick) = state.picking.as_mut() {
        if pick.phase.is_some() {
            pick.polarity = toggle(pick.polarity);
        }
        return;
    }
    for marker in &mut state.markers {
        if marker.selected && marker.phase.is_some() {
            marker.polarity = toggle(marker.polarity);
        }
    }
}

/// The digit keys: the marker kind of the floating pick, or of the selection.
///
/// While picking, the key retargets the floating pick so the choice carries into
/// the next re-armed pick (snuffler's `floating_or_selected_markers`).
/// A digit never latches a kind for future picks: Qt's fresh picks are always
/// kind 0.
pub(super) fn set_kind(state: &mut AppState, kind: u8) {
    if let Some(pick) = state.picking.as_mut() {
        pick.kind = kind;
        return;
    }
    for marker in &mut state.markers {
        if marker.selected {
            marker.kind = kind;
        }
    }
}

/// F1-F9: names the pick and associates it with the active event.
///
/// That is `set_event(self.get_active_event())` on a phase pick, and
/// `convert_to_phase_marker(event, name, None, False)` on a plain marker, which also
/// resets its polarity.
/// Events are never given a phase (`pile_viewer.py:2654`).
pub(super) fn set_phase(state: &mut AppState, phase: &str) {
    let active_event = state.markers.iter().find(|marker| marker.active);
    let event_hash = active_event.and_then(|event| event.event_hash.clone());
    let event_time = active_event.map(|event| event.tmin);
    if let Some(pick) = state.picking.as_mut() {
        if pick.phase.is_none() {
            pick.polarity = None;
        }
        pick.phase = Some(phase.to_owned());
        pick.event_hash = event_hash;
        pick.event_time = event_time;
        return;
    }
    for marker in &mut state.markers {
        if marker.selected && !marker.is_event {
            if marker.phase.is_none() {
                marker.polarity = None;
            }
            marker.phase = Some(phase.to_owned());
            marker.event_hash.clone_from(&event_hash);
            marker.event_time = event_time;
        }
    }
}

/// F10: demotes phase picks back to plain markers (Qt's `convert_to_marker` in
/// `set_phase_kind`, `pile_viewer.py:2654`).
///
/// It drops the name, polarity, uncertainty and the event association.
/// Plain markers and events pass through untouched.
pub(super) fn clear_phase(state: &mut AppState) {
    if let Some(pick) = state.picking.as_mut() {
        pick.phase = None;
        pick.polarity = None;
        pick.event_hash = None;
        pick.event_time = None;
        return;
    }
    for marker in &mut state.markers {
        if marker.selected && marker.phase.is_some() {
            marker.phase = None;
            marker.polarity = None;
            marker.uncertainty = None;
            marker.event_hash = None;
            marker.event_time = None;
        }
    }
}

/// Moves the selected non-event markers by `seconds`.
pub(super) fn nudge_selected(state: &mut AppState, seconds: f64) {
    for marker in &mut state.markers {
        if marker.selected && !marker.is_event {
            marker.tmin += seconds;
            marker.tmax += seconds;
        }
    }
}

/// Recentres the view on the selected markers, or on the full scene if nothing is
/// selected (snuffler's `go_to_selection` fallback).
pub(super) fn go_to_selection(
    state: &mut AppState,
    tight: bool,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) {
    let selected: Vec<&Marker> = state
        .markers
        .iter()
        .filter(|marker| marker.selected)
        .collect();
    let (tmin, tmax) = if !selected.is_empty() {
        (
            selected
                .iter()
                .map(|marker| marker.tmin)
                .fold(f64::INFINITY, f64::min),
            selected
                .iter()
                .map(|marker| marker.tmax)
                .fold(f64::NEG_INFINITY, f64::max),
        )
    } else if tight {
        // Snuffler's tight case with nothing selected zooms *in* to one
        // trace-visibility length around the view centre (pile_viewer.py:2814).
        let center = f64::midpoint(state.tmin, state.tmax);
        (center, center)
    } else {
        // With nothing selected, `g` shows the whole pile (pile_viewer.py:2817).
        let Some(range) = scene_range else {
            return;
        };
        range
    };
    let window = good_looking_time_range(TimeJump {
        target: TimeWindow { tmin, tmax },
        view: TimeWindow {
            tmin: state.tmin,
            tmax: state.tmax,
        },
        deltatmin,
        visible_length_samples: state.visible_length.jump_samples(),
        tight,
    });
    state.tmin = window.tmin;
    state.tmax = window.tmax;
}

/// Centres the view on `time`, keeping its duration.
fn center_view_on(state: &mut AppState, time: f64) {
    let duration = (state.tmax - state.tmin).max(1e-9);
    state.tmin = time - duration * 0.5;
    state.tmax = time + duration * 0.5;
}

/// Selects the next or previous marker by time and recentres (snuffler's `n`/`p`,
/// or `N`/`P` to step through event markers only), skipping hidden kinds.
pub(super) fn select_relative(state: &mut AppState, forward: bool, events_only: bool) {
    // Qt's reference quirk (pile_viewer.py:2325): only lowercase `n` takes the
    // *maximum* selected time.
    // `p`, `P`, and `N` (whose guard is lowercase-only upstream) take the minimum.
    // That is an upstream slip, reproduced deliberately: the same keystrokes land
    // on the same marker on both sides.
    let take_max = forward && !events_only;
    let selected_reference = state
        .markers
        .iter()
        .filter(|marker| marker.selected)
        .map(|marker| marker.tmin)
        .reduce(|reference, time| {
            if take_max {
                reference.max(time)
            } else {
                reference.min(time)
            }
        });
    let reference = selected_reference.unwrap_or_else(|| f64::midpoint(state.tmin, state.tmax));

    let next = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| state.marker_kind_visible(marker.kind))
        .filter(|(_, marker)| !events_only || marker.is_event)
        .filter(|(_, marker)| {
            if forward {
                marker.tmin > reference
            } else {
                marker.tmin < reference
            }
        })
        .min_by(|(_, a), (_, b)| {
            let (key_a, key_b) = if forward {
                (a.tmin, b.tmin)
            } else {
                (-a.tmin, -b.tmin)
            };
            key_a.partial_cmp(&key_b).unwrap_or(Ordering::Equal)
        })
        .map(|(index, marker)| (index, marker.tmin));

    // With a selection but no further marker, Qt still re-centres on the reference
    // time: `tgo` was already set before the search.
    let mut center_on = selected_reference;
    if let Some((target, time)) = next {
        for (index, marker) in state.markers.iter_mut().enumerate() {
            marker.selected = index == target;
        }
        center_on = Some(time);
    }
    if let Some(time) = center_on {
        center_view_on(state, time);
    }
}

/// One phase pick of the active event, in Tab order.
struct TabStop {
    marker_index: usize,
    /// Qt's `marker_to_itrack`: the row the pick's channel gathers into, or `None`
    /// when it has none (an unscoped pick, or a channel with no row).
    row: Option<usize>,
    mid_time: f64,
}

/// Tab and Backtab walk *the phase picks of the active event* and nothing else
/// (`pile_viewer.py:2258`).
///
/// Kind-visible picks are ordered by track row, then mid-time (Backtab reverses),
/// and selected one at a time.
/// The view recentres when the target sits off screen, and the track stack scrolls
/// to show its row.
/// With no active event the keys do nothing; a pick whose channel has no row falls
/// back to [`go_to_selection`].
pub(super) fn cycle_selection(
    state: &mut AppState,
    forward: bool,
    tracks: &[ViewTrack],
    rows_shown: f32,
    scene_range: Option<(f64, f64)>,
    deltatmin: f64,
) {
    let Some(active_hash) = state
        .markers
        .iter()
        .find(|marker| marker.active)
        .and_then(|marker| marker.event_hash.clone())
    else {
        return;
    };
    let row_of = |marker: &Marker| {
        // Qt's `one_nslc()` raises unless the pick has exactly one channel, and the
        // walk treats that as "no row".
        let [nslc] = marker.nslc_ids.as_slice() else {
            return None;
        };
        tracks
            .iter()
            .position(|track| marker_matches_track(nslc, track))
    };
    let mut stops: Vec<TabStop> = state
        .markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| {
            marker.phase.is_some()
                && marker.event_hash.as_deref() == Some(active_hash.as_str())
                && state.marker_kind_visible(marker.kind)
        })
        .map(|(marker_index, marker)| TabStop {
            marker_index,
            row: row_of(marker),
            mid_time: f64::midpoint(marker.tmin, marker.tmax),
        })
        .collect();
    // `None` sorts first, as Qt's -1 does.
    stops.sort_by(|a, b| {
        a.row.cmp(&b.row).then(
            a.mid_time
                .partial_cmp(&b.mid_time)
                .unwrap_or(Ordering::Equal),
        )
    });
    if !forward {
        stops.reverse();
    }
    // Qt steps past the highest-placed selected pick, or starts at the first.
    let last_selected = stops.iter().rposition(|stop| {
        state
            .markers
            .get(stop.marker_index)
            .is_some_and(|marker| marker.selected)
    });
    let next = last_selected.map_or(0, |position| position + 1);
    let Some(&TabStop {
        marker_index: target,
        row,
        ..
    }) = stops.get(next)
    else {
        return;
    };

    let mut target_time = None;
    for (index, marker) in state.markers.iter_mut().enumerate() {
        marker.selected = index == target;
        if marker.selected {
            target_time = Some(marker.tmin);
        }
    }
    if let Some(time) = target_time
        && !(state.tmin < time && time < state.tmax)
    {
        center_view_on(state, time);
    }

    let Some(row) = row else {
        go_to_selection(state, false, scene_range, deltatmin);
        return;
    };
    // Scroll the stack until the pick's row is on screen (Qt's `scroll_tracks`
    // calls): up to put it first, down to put it last.
    #[expect(
        clippy::cast_precision_loss,
        reason = "a row index is far below f32's exact integer range"
    )]
    let row = row as f32;
    let first_shown = state.track_scroll.floor();
    if row < first_shown {
        state.track_scroll = row;
    } else if row >= first_shown + rows_shown {
        state.track_scroll = (row + 1.0 - rows_shown).max(0.0);
    }
}
