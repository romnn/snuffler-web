//! The ':' command line, drawn above the traces like snuffler's input area.
//!
//! State and behaviour live in [`snuffler_web_core::command`]; this only draws
//! the input and forwards snuffler's four bindings: Return submits, Escape
//! aborts, Up and Down walk the history.

use snuffler_web_core::command::{FocusRequest, HistoryStep, PLACEHOLDER};
use snuffler_web_core::state::AppState;

/// Background of a rejected input, matching snuffler's error palette
/// (`pyrocko.gui.util.get_err_palette`).
const ERROR_BG: egui::Color32 = egui::Color32::from_rgb(255, 200, 200);

/// The error message and the text over that background.
///
/// Dark enough to stay readable on the tinted field, which keeps its light background in both
/// themes.
const ERROR_FG: egui::Color32 = egui::Color32::from_rgb(0x8B, 0x1A, 0x1A);

/// Draws the command line when it is open, returning true when the user
/// submitted a line for the session to run.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> bool {
    if !state.command.visible {
        return false;
    }
    // Stable, so the input keeps its focus and selection across frames.
    let id = egui::Id::new("snuffler.command_input");
    let mut submitted = false;

    egui::Panel::top("command").show_inside(ui, |ui| {
        // Bound to the open command line rather than to the focused input:
        // egui surrenders focus on Escape before any widget runs, so an input
        // that asked "do I have the keyboard?" here would already have lost it
        // and would let the Escape that closes it fall through to the traces.
        submitted = handle_keys(ui, state);
        if !state.command.visible {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(":").monospace().strong());
            input_line(ui, state, id);
        });
        if let Some(error) = state.command.error.as_deref() {
            ui.colored_label(ERROR_FG, error);
        }
    });
    submitted
}

/// Applies snuffler's input-line bindings, returning true when Return was pressed.
///
/// Each is taken out of the event queue before the input is built: a focused `TextEdit` would
/// otherwise swallow Up/Down as cursor movement, and the traces below would read the Escape
/// that closed the command line as a request to start picking.
fn handle_keys(ui: &egui::Ui, state: &mut AppState) -> bool {
    let (submit, abort, step) = ui.input_mut(|input| {
        let submit = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        let abort = input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        let step = if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            Some(HistoryStep::Older)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            Some(HistoryStep::Newer)
        } else {
            None
        };
        (submit, abort, step)
    });

    if let Some(step) = step {
        state.command.step_history(step);
    }
    if abort {
        state.command.hide();
        return false;
    }
    submit
}

/// Draws the editable line itself.
fn input_line(ui: &mut egui::Ui, state: &mut AppState, id: egui::Id) {
    let mut edit = egui::TextEdit::singleline(&mut state.command.text)
        .id(id)
        .desired_width(f32::INFINITY)
        .font(egui::TextStyle::Monospace)
        .hint_text(PLACEHOLDER);
    if state.command.error.is_some() {
        edit = edit.background_color(ERROR_BG).text_color(ERROR_FG);
    }
    let output = edit.show(ui);

    if output.response.changed() {
        state.command.edited();
    }
    if let Some(focus) = state.command.take_focus_request() {
        output.response.request_focus();
        if focus == FocusRequest::SelectAll {
            select_all(ui, &output);
        }
    }
}

/// Selects the whole line, so the next keystroke replaces it (snuffler selects
/// all whenever it shows or rejects the input).
fn select_all(ui: &egui::Ui, output: &egui::text_edit::TextEditOutput) {
    let mut state = output.state.clone();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::select_all(&output.galley)));
    state.store(ui.ctx(), output.response.id);
}
