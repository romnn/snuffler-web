//! Snuffler's keyboard commands on the waveform canvas.

use egui::{InputState, Key};
use snuffler_web_core::bridge::ViewTrack;
use snuffler_web_core::markers::PHASE_KEYS;
use snuffler_web_core::state::{AppState, PickingState, ScalingGroup, SortMode};

use super::marker_edits::{self, deselect_all};

/// The canvas facts the keyboard commands need besides the app state.
pub(super) struct KeyboardContext {
    pub(super) canvas_width: f32,
    /// The whole pile's time span, for `g` with nothing selected.
    pub(super) scene_range: Option<(f64, f64)>,
    /// The pile's finest sampling interval, which sizes the go-to windows.
    pub(super) deltatmin: f64,
}

/// What the keys of one frame act on, read before any of them acts.
///
/// Several keys in one frame all see the view as the frame began.
struct FrameStart<'a> {
    context: &'a KeyboardContext,
    /// Whether a widget holds the keyboard focus.
    widget_focused: bool,
    /// The view's duration in seconds.
    duration: f64,
    /// How many rows the canvas shows, including a partial row when Snap is off.
    rows_shown: f32,
}

/// The digit keys, which set the marker kind of the same number.
const KIND_KEYS: [Key; 8] = [
    Key::Num0,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
];

/// F1-F9, which name a phase from [`PHASE_KEYS`].
const PHASE_NAME_KEYS: [Key; 9] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
];

/// Handles this frame's keyboard commands.
///
/// `tracks` are every row of the view, which Tab's active-event walk orders by.
pub(super) fn handle_keyboard(
    ui: &egui::Ui,
    state: &mut AppState,
    tracks: &[ViewTrack],
    context: &KeyboardContext,
) {
    // Don't fire single-key shortcuts while a text field (FDSN forms, command
    // line) has the keyboard, or typing "n" would jump markers.
    //
    // Asked of the text edit specifically, not of focus in general:
    // `egui_wants_keyboard_input` is true whenever *any* widget holds focus, so a
    // menu item activated from the keyboard would keep every single-key shortcut
    // dead until a pointer click on the canvas.
    // Snuffler has no such state: closing a menu returns the keyboard to the viewer.
    //
    // An open menu still swallows them, the way a QMenu holds the keyboard while it
    // is up.
    if ui.ctx().text_edit_focused() || ui.ctx().any_popup_open() {
        return;
    }
    // Read before entering the input closure: `ui.input` holds the egui context
    // lock while its closure runs, so asking the context anything from inside
    // (`memory()`, another `input()`) self-deadlocks.
    // On the web that surfaced as an `Instant::now` panic out of the lock's debug
    // timeout.
    let widget_focused = ui.ctx().memory(|memory| memory.focused().is_some());
    let frame = FrameStart {
        context,
        widget_focused,
        duration: (state.tmax - state.tmin).max(1e-9),
        rows_shown: state.visible_track_count(tracks.len()),
    };
    ui.input(|input| {
        // Qt dispatches its letter commands on the typed text, so a Ctrl, Cmd or
        // Alt chord produces a control character and matches nothing
        // (pile_viewer.py:2225); its accelerators are separate QActions.
        // Same split here: the accelerators first, single-key commands only when no
        // chord modifier is down, so Ctrl+P is not the previous-marker key.
        if input.modifiers.ctrl || input.modifiers.command || input.modifiers.alt {
            file_accelerators(input, state);
            return;
        }
        if s_chord(input, state) {
            return;
        }
        view_keys(input, state, tracks.len(), &frame);
        marker_keys(input, state, tracks, &frame);
        app_keys(input, state);
    });
}

/// Snuffler's File accelerators (`pile_viewer.py:874-921`).
///
/// Ctrl+O and Ctrl+S shadow browser chords, deliberately: egui consumes the handled
/// key, and snuffler muscle memory outweighs the host's Open-file and Save-page here.
fn file_accelerators(input: &InputState, state: &mut AppState) {
    if !(input.modifiers.ctrl || input.modifiers.command) || input.modifiers.alt {
        return;
    }
    if input.key_pressed(Key::O) {
        state.open_files_requested = true;
    }
    if input.key_pressed(Key::S) {
        state.save_markers_requested = Some(input.modifiers.shift);
    }
    if input.key_pressed(Key::E) {
        state.export_image_requested = true;
    }
}

/// Snuffler's two-stroke `s` chords, bound there as `QKeySequence`s
/// (`pile_viewer.py:935`, `:987`).
///
/// A bare `s` arms the chord, and the next keypress completes or cancels it,
/// whatever it is: a Qt sequence swallows its second key the same way.
/// Returns whether the chord took this frame's keys.
fn s_chord(input: &InputState, state: &mut AppState) -> bool {
    if state.pending_s_chord {
        let any_key_pressed = input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }));
        if !any_key_pressed {
            return true;
        }
        state.pending_s_chord = false;
        if input.key_pressed(Key::I) {
            state.scaling_group = ScalingGroup::Individual;
        } else if input.key_pressed(Key::C) {
            state.scaling_group = ScalingGroup::Common;
        } else if input.key_pressed(Key::S) {
            state.scaling_group = ScalingGroup::PerStation;
        } else if input.key_pressed(Key::N) {
            state.sort = SortMode::Name;
        } else if input.key_pressed(Key::D) {
            state.sort = SortMode::Distance;
        }
        return true;
    }
    if input.key_pressed(Key::S) && !input.modifiers.shift {
        state.pending_s_chord = true;
        return true;
    }
    false
}

/// Paging through time and tracks, track zoom, fullscreen and demean.
///
/// Arrows and Space are the keys egui's focused widgets also act on: a focused
/// slider plus Left would both move the slider and nudge the selected markers, and
/// a focused checkbox plus Space would toggle and page.
/// Qt's sliders decline those keys (`QSliderNoWheel.keyPressEvent`), so the viewer
/// sees them.
/// Here the widget keeps them while it has focus.
fn view_keys(input: &InputState, state: &mut AppState, track_count: usize, frame: &FrameStart<'_>) {
    let shift = input.modifiers.shift;
    let FrameStart {
        widget_focused,
        duration,
        rows_shown,
        ..
    } = *frame;

    // The browser's Fullscreen API needs a real user gesture and a DOM call
    // (eframe's viewport command is unhandled on the web), so the app performs the
    // toggle.
    // Snuffler binds `f` (View ▸ Fullscreen).
    // Its `q`/`x` Close pair is deliberately not bound: a page cannot close a tab
    // it did not open, and an inert key is worse than an absent one.
    if input.key_pressed(Key::F) {
        state.fullscreen_toggle_requested = true;
    }

    // Time paging
    if input.key_pressed(Key::Space) && !widget_focused {
        state.tmin += duration;
        state.tmax += duration;
    }
    if input.key_pressed(Key::B) {
        state.tmin -= duration;
        state.tmax -= duration;
    }

    // Track zoom: + shows one track more, - one less, = resets to the initial count.
    // Plus is Shift+Equals on most layouts, so accept either for "more".
    // Bare Equals (no shift) resets the preferred count.
    if input.key_pressed(Key::Plus) || (input.key_pressed(Key::Equals) && shift) {
        state.tracks_per_page = Some(rows_shown + 1.0);
    }
    if input.key_pressed(Key::Minus) && !shift {
        state.tracks_per_page = Some((rows_shown - 1.0).max(1.0));
    }
    // Shift+Minus is `_`, snuffler's Demean toggle (its menu shortcut).
    if input.key_pressed(Key::Minus) && shift {
        state.demean = !state.demean;
    }
    if input.key_pressed(Key::Equals) && !shift {
        state.tracks_per_page = None;
    }

    // Track paging
    if input.key_pressed(Key::PageDown) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a track count is far below f32's exact integer range"
        )]
        let max_scroll = (track_count as f32 - rows_shown).max(0.0);
        state.track_scroll = (state.track_scroll + rows_shown).min(max_scroll);
    }
    if input.key_pressed(Key::PageUp) {
        state.track_scroll = (state.track_scroll - rows_shown).max(0.0);
    }
}

/// Marker selection, navigation and editing.
fn marker_keys(
    input: &InputState,
    state: &mut AppState,
    tracks: &[ViewTrack],
    frame: &FrameStart<'_>,
) {
    let shift = input.modifiers.shift;
    let FrameStart {
        widget_focused,
        duration,
        rows_shown,
        ..
    } = *frame;
    let KeyboardContext {
        canvas_width,
        scene_range,
        deltatmin,
        ..
    } = *frame.context;

    if input.key_pressed(Key::Backspace) || input.key_pressed(Key::Delete) {
        state.remove_selected_markers();
    }

    // Selection navigation (snuffler keyPressEvent, pile_viewer.py:2258-2368)
    if input.key_pressed(Key::Tab) {
        marker_edits::cycle_selection(state, !shift, tracks, rows_shown, scene_range, deltatmin);
    }
    // Shift+N and Shift+P step through events only.
    if input.key_pressed(Key::N) {
        marker_edits::select_relative(state, true, shift);
    }
    if input.key_pressed(Key::P) {
        marker_edits::select_relative(state, false, shift);
    }
    if input.key_pressed(Key::A) {
        marker_edits::select_all(state, shift);
    }
    if input.key_pressed(Key::D) {
        deselect_all(state);
    }
    // Shift+G fits the selection tightly.
    if input.key_pressed(Key::G) {
        marker_edits::go_to_selection(state, shift, scene_range, deltatmin);
    }

    // Active event
    if input.key_pressed(Key::E) {
        if shift {
            marker_edits::deactivate_events(state);
        } else {
            marker_edits::activate_selected_event(state);
        }
    }

    // First-motion polarity on the floating pick or the selected phase picks
    // (pile_viewer.py:2233-2251)
    if input.key_pressed(Key::ArrowUp) && !widget_focused {
        marker_edits::set_polarity(state, true, shift);
    }
    if input.key_pressed(Key::ArrowDown) && !widget_focused {
        marker_edits::set_polarity(state, false, shift);
    }

    // Nudge the selected picks by exactly one screen pixel of time, as snuffler
    // quantises it (pile_viewer.py:2533); Shift moves ten.
    if (input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::ArrowRight)) && !widget_focused
    {
        let direction = if input.key_pressed(Key::ArrowLeft) {
            -1.0
        } else {
            1.0
        };
        let pixels = if shift { 10.0 } else { 1.0 };
        marker_edits::nudge_selected(
            state,
            direction * pixels * duration / f64::from(canvas_width.max(1.0)),
        );
    }

    // Escape toggles batch picking (snuffler): it enters batch mode, or aborts an
    // in-progress pick and exits the mode (`take()` discards the floating pick).
    // `start_picking` deselects everything on entry, so nothing but the floating
    // pick can be acted on, and a fresh pick is always kind 0.
    if input.key_pressed(Key::Escape) && state.picking.take().is_none() {
        deselect_all(state);
        state.picking = Some(PickingState {
            batch_mode: true,
            ..Default::default()
        });
    }

    // Marker kind and phase
    for (kind, key) in (0_u8..).zip(KIND_KEYS) {
        if input.key_pressed(key) {
            marker_edits::set_kind(state, kind);
        }
    }
    for (key, phase) in PHASE_NAME_KEYS.into_iter().zip(PHASE_KEYS) {
        if input.key_pressed(key) {
            marker_edits::set_phase(state, phase);
        }
    }
    if input.key_pressed(Key::F10) {
        marker_edits::clear_phase(state);
    }
}

/// Keys that open windows or reload.
fn app_keys(input: &InputState, state: &mut AppState) {
    let shift = input.modifiers.shift;
    if input.key_pressed(Key::M) {
        state.show_marker_editor = !state.show_marker_editor;
    }
    if input.key_pressed(Key::C) {
        state.show_controls = !state.show_controls;
    }
    // `r` reloads modified files from disk, and Shift+R reloads the Python
    // snufflings.
    // The App consumes the flags, since keyboard handlers may not touch the bridge.
    if input.key_pressed(Key::R) {
        if shift {
            state.reload_snufflings_requested = true;
        } else {
            state.reload_files_requested = true;
        }
    }
    if input.key_pressed(Key::Questionmark) || (input.key_pressed(Key::Slash) && shift) {
        state.show_help = true;
    }
    // ':' is Shift+Semicolon on most layouts; egui reports either depending on
    // whether the browser gave it a logical or a physical key.
    if input.key_pressed(Key::Colon) || (input.key_pressed(Key::Semicolon) && shift) {
        state.command.show();
    }
}
