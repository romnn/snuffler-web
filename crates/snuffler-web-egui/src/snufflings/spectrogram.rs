pub use snuffler_web_core::snufflings::spectrogram::Config;

use super::Action;
use super::widgets::{choice_row, optional_freq_row, param_row, run_button};

const TAPERS: [&str; 4] = ["Hanning", "Hamming", "Blackman", "Bartlett"];
const COLOR_SCALES: [&str; 3] = ["log", "sqrt", "lin"];
const COLOR_TABLES: [&str; 2] = ["spectro", "rainbow"];

pub fn panel(ui: &mut egui::Ui, config: &mut Config, nyquist: Option<f64>) -> Option<Action> {
    ui.label("Plot a spectrogram (windowed STFT) for the visible channels.");
    ui.add_space(6.0);
    param_row(
        ui,
        "Window length [s]",
        &mut config.window_length_s,
        0.1,
        10000.0,
    );
    param_row(ui, "Overlap [%]", &mut config.overlap_percent, 0.0, 99.0);
    choice_row(ui, "Taper function", &mut config.taper, &TAPERS);
    // Qt re-narrows the corner ranges to the loaded band on every pile change
    // (`adjust_controls`, spectrogram.py:103).
    let fmax = nyquist.unwrap_or(50.0).clamp(0.002, 50.0);
    optional_freq_row(ui, "Fmin [Hz]", &mut config.fmin, 0.001, fmax, true);
    optional_freq_row(ui, "Fmax [Hz]", &mut config.fmax, 0.001, fmax, false);
    choice_row(ui, "Color scale", &mut config.color_scale, &COLOR_SCALES);
    choice_row(ui, "Color table", &mut config.color_table, &COLOR_TABLES);
    run_button(ui).then_some(Action::Spectrogram)
}
