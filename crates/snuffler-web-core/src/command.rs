//! The ':' command line, mirroring pyrocko's snuffler (`pile_viewer.py`'s
//! `LineEditWithAbort` + `PileViewer.inputline_*`).
//!
//! This is the frontend-independent half: visibility, the edited line, the
//! recalled history, and what happens to the input once a command has run. A
//! frontend draws [`CommandLine::text`], forwards the four bindings snuffler has
//! (Return, Escape, Up, Down) and honours [`CommandLine::focus_request`].
//!
//! Snuffler decides the post-run behaviour synchronously, from the
//! `(clearit, hideit, error)` triple `inputline_finished` returns. Here the
//! commands that reach pyrocko are asynchronous, so a submitted line stays
//! pending — and the input stays open — until [`CommandLine::resolve`] applies
//! the same rules to the answer.

/// Commands whose input line snuffler clears once they have run (`clearit`), so
/// the blacklist edit does not stay in the box ready to be re-submitted.
const CLEARING_COMMANDS: [&str; 2] = ["hide", "unhide"];

/// Quick-filter commands, which reduce the shown traces to one NSLC element.
const QUICK_FILTER_COMMANDS: [&str; 4] = ["n", "s", "l", "c"];

/// Longest history snuffler keeps (`save_inputline_history` writes the last 100).
const MAX_HISTORY: usize = 100;

/// Hint shown in an empty input, verbatim from snuffler. The arrows need a font
/// that carries them — the egui frontend draws this line in `JetBrains Mono` for
/// exactly that reason.
pub const PLACEHOLDER: &str =
    "Quick commands: e.g. 'c HH?' to select channels. Use ↑ or ↓ to navigate.";

/// Direction of a history walk (snuffler's `history_up` / `history_down`, wired
/// to the Up and Down keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryStep {
    /// Towards the oldest entry (Up).
    Older,
    /// Towards the newest entry and the empty slot past it (Down).
    Newer,
}

/// A pending focus grab the frontend performs on its next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusRequest {
    /// Take the keyboard and select the whole line, so typing replaces it
    /// (snuffler's `setFocus` + `selectAll` when the input is shown).
    SelectAll,
    /// Take the keyboard and leave the selection alone (the line is still being
    /// edited; the widget only lost focus because Return was pressed).
    Keep,
}

/// Visibility, contents, and history of the ':' command line.
#[derive(Default)]
pub struct CommandLine {
    /// Whether the input is on screen; the frontend draws nothing when false.
    pub visible: bool,
    /// The line being edited.
    pub text: String,
    /// Error from the last command, shown under the input until the line is
    /// edited again.
    pub error: Option<String>,
    /// The submitted line waiting for its result. While this is set the input
    /// stays open, matching snuffler's synchronous "nothing moves until the
    /// command has run".
    pending: Option<String>,
    /// Accepted lines, oldest first.
    history: Vec<String>,
    /// Cursor into `history` for Up/Down. `history.len()` is the empty slot past
    /// the newest entry, which is where a fresh input starts.
    hist_ind: usize,
    /// Whether the stored history has been read yet (snuffler loads the history
    /// file the first time the input is shown).
    history_loaded: bool,
    /// The line that installed the active quick filter, restored into the input
    /// after a clearing command (snuffler keeps it in `quick_filter_patterns`).
    quick_filter_line: Option<String>,
    focus_request: Option<FocusRequest>,
}

impl CommandLine {
    /// Shows the input and focuses it with the current line selected (snuffler's
    /// `inputline_show`, reached with ':' or the menu bar's ':' button).
    pub fn show(&mut self) {
        if !self.history_loaded {
            self.history = load_history();
            self.history_loaded = true;
            self.hist_ind = self.history.len();
        }
        self.visible = true;
        self.focus_request = Some(FocusRequest::SelectAll);
    }

    /// Hides the input and rewinds the history cursor (snuffler's
    /// `inputline_aborted`, reached with Escape).
    pub fn hide(&mut self) {
        self.visible = false;
        self.pending = None;
        self.focus_request = None;
        self.hist_ind = self.history.len();
    }

    /// Toggles the input (the menu bar's ':' button).
    pub fn toggle(&mut self) {
        if self.visible {
            self.hide();
        } else {
            self.show();
        }
    }

    /// Whether a submitted line is still waiting for its result.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Takes the pending focus grab, if any. The frontend calls this once per
    /// frame while drawing the input.
    pub fn take_focus_request(&mut self) -> Option<FocusRequest> {
        self.focus_request.take()
    }

    /// Clears the error after the line was edited (snuffler's
    /// `inputline_clear_error`, wired to `textEdited`).
    pub fn edited(&mut self) {
        self.error = None;
    }

    /// Steps one entry through the history and replaces the line with the entry
    /// landed on (snuffler's `step_through_history`).
    ///
    /// The walk wraps around `history.len() + 1` slots: the extra one past the
    /// newest entry holds an empty line, so stepping off either end clears the
    /// input rather than sticking to the oldest or newest command.
    pub fn step_history(&mut self, step: HistoryStep) {
        let slots = self.history.len() + 1;
        self.hist_ind = match step {
            HistoryStep::Older => (self.hist_ind + slots - 1) % slots,
            HistoryStep::Newer => (self.hist_ind + 1) % slots,
        };
        self.text = self.history.get(self.hist_ind).cloned().unwrap_or_default();
    }

    /// Accepts the line for execution and returns it (snuffler's Return).
    ///
    /// The input stays open and keeps the keyboard until [`Self::resolve`]
    /// reports how the command went. An empty line runs nothing and just closes
    /// the input, as it does in snuffler.
    pub fn submit(&mut self) -> Option<String> {
        let line = self.text.clone();
        if line.trim().is_empty() {
            self.hide();
            return None;
        }
        self.pending = Some(line.clone());
        self.focus_request = Some(FocusRequest::Keep);
        Some(line)
    }

    /// Applies snuffler's post-run rules to the submitted line: a failed command
    /// keeps the input open showing the error, a successful one is remembered in
    /// the history and closes the input.
    pub fn resolve(&mut self, error: Option<String>) {
        let Some(line) = self.pending.take() else {
            return;
        };
        let command = command_name(&line);

        if error.is_none() {
            self.remember(&line);
            if QUICK_FILTER_COMMANDS.contains(&command.as_str()) {
                // A quick filter with no pattern turns the filter off; with one,
                // this line becomes what a later clearing command restores.
                self.quick_filter_line = has_argument(&line).then(|| line.clone());
            }
        }
        // snuffler applies `clearit` even when the command failed — an `unhide`
        // that names no blacklisted pattern both errors and empties the input.
        if CLEARING_COMMANDS.contains(&command.as_str()) {
            self.text = self.quick_filter_line.clone().unwrap_or_default();
        }

        self.hist_ind = self.history.len();
        match error {
            Some(message) => {
                self.error = Some(message);
                self.visible = true;
                self.focus_request = Some(FocusRequest::SelectAll);
            }
            None => self.hide(),
        }
    }

    /// Appends an accepted line to the history, skipping an immediate repeat
    /// (snuffler only appends when the line differs from the previous one).
    fn remember(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || self.history.last().is_some_and(|last| last == line) {
            return;
        }
        self.history.push(line.to_owned());
        if self.history.len() > MAX_HISTORY {
            let excess = self.history.len() - MAX_HISTORY;
            self.history.drain(..excess);
        }
        save_history(&self.history);
    }
}

/// The lower-cased command word of a line (snuffler lower-cases `toks[0]`).
fn command_name(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

/// Whether a line carries anything after its command word.
fn has_argument(line: &str) -> bool {
    line.split_whitespace().nth(1).is_some()
}

/// Where the browser keeps the command history. Snuffler writes
/// `$HOME/.pyrocko/.snuffler_history`; `localStorage` is its counterpart for an
/// app with no filesystem of its own.
#[cfg(target_arch = "wasm32")]
const HISTORY_KEY: &str = "snuffler.command_history";

#[cfg(target_arch = "wasm32")]
fn history_storage() -> Option<web_sys::Storage> {
    // Private-mode browsers and sandboxed frames refuse storage access; the
    // command line still works, it just forgets between reloads.
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn load_history() -> Vec<String> {
    let Some(stored) = history_storage().and_then(|storage| storage.get_item(HISTORY_KEY).ok())
    else {
        return Vec::new();
    };
    stored
        .into_iter()
        .flat_map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn save_history(history: &[String]) {
    if let Some(storage) = history_storage() {
        let _ = storage.set_item(HISTORY_KEY, &history.join("\n"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_history() -> Vec<String> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
fn save_history(_history: &[String]) {}

#[cfg(test)]
mod tests {
    use super::{CommandLine, FocusRequest, HistoryStep};

    fn submitted(line: &str) -> CommandLine {
        let mut command = CommandLine::default();
        command.show();
        command.text = line.to_owned();
        assert_eq!(command.submit().as_deref(), Some(line));
        command
    }

    #[test]
    fn a_successful_command_closes_the_input_and_is_remembered() {
        let mut command = submitted("c HH?");
        assert!(
            command.visible,
            "the input stays open until the result lands"
        );
        command.resolve(None);

        assert!(!command.visible);
        assert_eq!(command.history, ["c HH?"]);
        assert_eq!(command.text, "c HH?");
    }

    #[test]
    fn a_failed_command_keeps_the_input_open_and_is_not_remembered() {
        let mut command = submitted("goto nowhere");
        command.resolve(Some("no event or time matching: nowhere".to_owned()));

        assert!(command.visible);
        assert_eq!(
            command.error.as_deref(),
            Some("no event or time matching: nowhere")
        );
        assert_eq!(command.take_focus_request(), Some(FocusRequest::SelectAll));
        assert!(command.history.is_empty());
    }

    #[test]
    fn editing_clears_the_error() {
        let mut command = submitted("nope");
        command.resolve(Some("No such command: nope".to_owned()));
        command.edited();

        assert!(command.error.is_none());
    }

    #[test]
    fn a_clearing_command_restores_the_active_quick_filter_line() {
        let mut command = submitted("c HH?");
        command.resolve(None);

        command.show();
        command.text = "hide GE.*.*.*".to_owned();
        command.submit();
        command.resolve(None);
        assert_eq!(command.text, "c HH?", "the quick filter comes back");

        // A quick filter with no pattern turns the filter off, so later clearing
        // commands empty the input instead.
        command.show();
        command.text = "c".to_owned();
        command.submit();
        command.resolve(None);
        command.show();
        command.text = "unhide".to_owned();
        command.submit();
        command.resolve(None);
        assert_eq!(command.text, "");
    }

    #[test]
    fn a_failed_clearing_command_still_clears_the_input() {
        let mut command = submitted("unhide GE.*.*.*");
        command.resolve(Some("Pattern not found in blacklist.".to_owned()));

        assert_eq!(command.text, "");
        assert!(command.error.is_some());
    }

    #[test]
    fn the_history_walk_wraps_through_an_empty_slot() {
        let mut command = CommandLine::default();
        command.show();
        for line in ["c HH?", "hide GE.*.*.*"] {
            command.text = line.to_owned();
            command.submit();
            command.resolve(None);
        }
        command.show();

        command.step_history(HistoryStep::Older);
        assert_eq!(command.text, "hide GE.*.*.*");
        command.step_history(HistoryStep::Older);
        assert_eq!(command.text, "c HH?");
        command.step_history(HistoryStep::Older);
        assert_eq!(command.text, "", "past the oldest entry is the empty slot");
        command.step_history(HistoryStep::Older);
        assert_eq!(command.text, "hide GE.*.*.*", "and then it wraps around");
        command.step_history(HistoryStep::Newer);
        assert_eq!(command.text, "");
    }

    #[test]
    fn an_immediate_repeat_is_not_appended_twice() {
        let mut command = CommandLine::default();
        command.show();
        for _ in 0..2 {
            command.text = "c HH?".to_owned();
            command.submit();
            command.resolve(None);
        }

        assert_eq!(command.history, ["c HH?"]);
    }

    #[test]
    fn an_empty_line_runs_nothing_and_closes_the_input() {
        let mut command = CommandLine::default();
        command.show();
        command.text = "   ".to_owned();

        assert!(command.submit().is_none());
        assert!(!command.visible);
        assert!(command.history.is_empty());
    }

    #[test]
    fn toggling_opens_and_closes_the_input() {
        let mut command = CommandLine::default();
        command.toggle();
        assert!(command.visible);
        command.toggle();
        assert!(!command.visible);
    }
}
