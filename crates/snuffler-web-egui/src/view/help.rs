//! The keyboard/mouse/command reference window ("Snuffler controls").

use snuffler_web_core::state::AppState;

const HELP_KEY_ROWS: &[(&str, &str)] = &[
    (
        "r",
        "Reload opened files changed on disk (Chromium file access)",
    ),
    (
        "R",
        "Reload the Python snuffling code (bridge.py), keeping the loaded pile",
    ),
    ("f", "Toggle full screen mode"),
    ("_", "Toggle Demean"),
    (
        "s i, s c, s s",
        "Scaling: individual / common / per station",
    ),
    ("s n, s d", "Sort by names / by distance"),
    ("<ctrl> + o", "Open files"),
    ("<ctrl> + s", "Save markers"),
    ("<ctrl> + <shift> + s", "Save selected markers"),
    ("<ctrl> + e", "Save image (PNG)"),
    ("m", "Toggle marker sidebar"),
    ("c", "Toggle main controls"),
    (":", "Enter command"),
    (
        "<up>, <down> (command line)",
        "Step through the command history",
    ),
    ("<space>", "Forward one page in time"),
    ("b, B", "Backward one page in time"),
    (
        "<left>, <right>",
        "Nudge selected markers by one pixel of time (<shift>: ten)",
    ),
    ("<pagedown>", "Scroll tracks one page down"),
    ("<pageup>", "Scroll tracks one page up"),
    ("+", "Shorter tracks (show one more)"),
    ("-", "Taller tracks (show one less)"),
    ("=", "Reset track height (auto-fit)"),
    ("g", "Go to selection / show all"),
    ("G", "Zoom to selection / zoom to trace visibility"),
    ("n", "Go to next marker"),
    ("p", "Go to previous marker"),
    ("N", "Go to next event marker"),
    ("P", "Go to previous event marker"),
    ("<tab>", "Go to next marker of active event"),
    ("<shift> + <tab>", "Go to previous marker of active event"),
    ("a", "Select all markers currently visible"),
    ("A", "Select all markers"),
    ("d", "Deselect all markers"),
    ("0 ... 7", "Change color of marker"),
    ("<f1> ... <f9>", "Convert to phase marker"),
    ("e", "Convert to event marker / set active event"),
    ("E", "Deactivate event"),
    ("<f10>", "Convert phase marker to normal marker"),
    ("<backspace>, <delete>", "Delete marker"),
    (
        "<up>, <down>",
        "Set first motion polarity on selected marker",
    ),
    (
        "<shift> + <up>, <shift> + <down>",
        "Unset first motion polarity on selected marker",
    ),
    (
        "<escape>",
        "Enter batch-picking mode / abort the current pick",
    ),
    ("?", "Help"),
];

const HELP_MOUSE_ROWS: &[(&str, &str)] = &[
    ("Click and drag", "Zoom and pan"),
    ("Click and drag on time axis", "Pan only"),
    ("Click on marker", "Select marker"),
    ("<shift> + click on marker", "Select additional marker"),
    ("Wheel", "Scroll tracks vertically"),
    ("<ctrl> + wheel", "Zoom track height (more / fewer tracks)"),
    (
        "Right-click",
        "Context menu (File / Data / View / Options / Scaling / Sorting / Snufflings / Help)",
    ),
    (
        "Double-click",
        "Arm picking: a floating pick follows the cursor; press-drag-release places it",
    ),
    (
        "<shift> + drag",
        "Adjust pick uncertainty of the selected phase markers",
    ),
    (
        "Hover near a marker",
        "Highlight it and show its metadata in the status line",
    ),
];

const HELP_COMMAND_ROWS: &[(&str, &str)] = &[
    (
        "n|s|l|c [ <pattern>... ]",
        "Reduce traces to those matching the given patterns; without one, show all again.",
    ),
    (
        "hide <pattern>\nhide n|s|l|c <pattern>...\nhide empty",
        "Hide traces whose network, station, location, and channel IDs match a pattern. \
         'hide empty' hides the channels with no data in the visible window.",
    ),
    (
        "unhide <pattern>\nunhide n|s|l|c <pattern>...\nunhide",
        "Unhide traces previously hidden with the hide command; bare 'unhide' clears all.",
    ),
    (
        "goto YYYY-MM[-DD[ HH[:MM[:SS[.XXX]]]]]\ngoto HH:MM[:SS[.XXX]]\ngoto <eventname>",
        "Jump to a given time or event. A bare time of day is taken on the day in view, \
         and 'today', 'yesterday' and 'now' work too.",
    ),
    (
        "markers [ all | <digits> ]",
        "Show only markers of the given colour kinds (0..7); bare 'markers' hides all.",
    ),
    (
        "scaling [ n.s.l.c ] vmin vmax\nscaling",
        "Pin amplitude scale range(s); 'nan' leaves a bound auto; 'scaling' alone clears.",
    ),
];

/// What each built-in snuffling does — Qt renders every snuffling's docstring
/// under Help ▸ Snuffling Help; this is the same index for the ported set.
const SNUFFLING_HELP: &[(&str, &str)] = &[
    (
        "Amplitude spectrum",
        "Amplitude spectra of the traces in the selected span (or visible \
         window) on log-log axes, with optional Konno-Ohmachi smoothing.",
    ),
    (
        "Spectrogram",
        "Windowed STFT spectrogram per channel on an absolute window grid, \
         with a shared PSD colour scale and colorbar.",
    ),
    (
        "Minimum / Maximum",
        "Raw minimum, maximum and peak-to-peak of each channel over the \
         selected span — unfiltered pile values, as snuffler reports them.",
    ),
    (
        "STA/LTA detector",
        "Short-/long-term-average ratio detection with level-crossing markers; \
         level traces appear in location 'cg' plus a .SUM.cg. mean trace.",
    ),
    (
        "Polarization",
        "Covariance-eigenvalue polarization analysis per 3-component station \
         over the selected markers' windows: hodograms, eigensystems, azimuth \
         and incidence.",
    ),
    (
        "RMS",
        "Blockwise root-mean-square amplitude as derived low-rate traces \
         (channel suffix -RMS), optionally grouped over components.",
    ),
    (
        "Cake Phase",
        "Theoretical arrivals through a 1D earth model (pyrocko.cake) as phase \
         markers with incidence/take-off angles, plus model and ray-path plots.",
    ),
    (
        "Seismosizer",
        "Synthetic seismograms from a pyrocko.gf point or finite source (DC, \
         MT, single-force, rectangular, pseudo-dynamic rupture) through a \
         mounted Green's-function store.",
    ),
    (
        "SeisBench picker",
        "Deep-learning P/S phase picker: exported SeisBench models run \
         in-browser; picks carry their probability on the incidence angle.",
    ),
];

/// Draws the Help ▸ Snuffling Help window, what each built-in snuffling does,
/// when the shared state asks for it.
pub fn show_snuffling_help(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_snuffling_help {
        return;
    }
    let mut open = true;
    egui::Window::new("Snuffling Help")
        .open(&mut open)
        .default_size([520.0, 420.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (name, text) in SNUFFLING_HELP {
                    ui.strong(*name);
                    ui.label(*text);
                    ui.add_space(6.0);
                }
            });
        });
    state.show_snuffling_help = open;
}

/// Draws the controls-help window when the shared state asks for it.
pub fn show(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_help {
        return;
    }
    let mut open = true;
    egui::Window::new("Snuffler controls")
        .open(&mut open)
        .default_size([560.0, 640.0])
        .show(ctx, help_text);
    state.show_help = open;
}

fn help_text(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        help_section(ui, "Key", HELP_KEY_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Mouse", HELP_MOUSE_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Command", HELP_COMMAND_ROWS, true);
    });
}

fn help_section(ui: &mut egui::Ui, title: &str, rows: &[(&str, &str)], monospace_key: bool) {
    ui.heading(title);
    ui.add_space(6.0);
    egui::Grid::new(("help_section", title))
        .num_columns(2)
        .spacing([18.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (key, description) in rows {
                let key_text = egui::RichText::new(*key);
                let key_text = if monospace_key {
                    key_text.monospace().strong()
                } else {
                    key_text.italics().strong()
                };
                // Right-align the shortcut within its column so it sits right
                // next to the description rather than far across a wide column.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(key_text);
                });
                ui.label(*description);
                ui.end_row();
            }
        });
    ui.add_space(4.0);
}
