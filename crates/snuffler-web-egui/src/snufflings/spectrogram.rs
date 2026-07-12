pub use snuffler_web_core::snufflings::spectrogram::Config;

use super::Action;
use super::widgets::{optional_freq_row, param_row, run_button};

const TAPERS: [&str; 4] = ["Hanning", "Hamming", "Blackman", "Bartlett"];
const COLOR_SCALES: [&str; 3] = ["log", "sqrt", "lin"];
const COLOR_TABLES: [&str; 2] = ["spectro", "rainbow"];

fn choice_row(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [160.0, 0.0],
            egui::Label::new(label).halign(egui::Align::Max),
        );
        egui::ComboBox::from_id_salt(label)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                for opt in options {
                    ui.selectable_value(value, (*opt).to_owned(), *opt);
                }
            });
    });
}

pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    ui.label("Plot a spectrogram (windowed STFT) for the visible channels.");
    ui.add_space(6.0);
    param_row(ui, "Window length [s]", &mut config.twin, 0.1, 10000.0);
    param_row(ui, "Overlap [%]", &mut config.overlap, 0.0, 99.0);
    choice_row(ui, "Taper function", &mut config.taper, &TAPERS);
    optional_freq_row(ui, "Fmin [Hz]", &mut config.fmin, 0.001, 50.0, true);
    optional_freq_row(ui, "Fmax [Hz]", &mut config.fmax, 0.001, 50.0, false);
    choice_row(ui, "Color scale", &mut config.color_scale, &COLOR_SCALES);
    choice_row(ui, "Color table", &mut config.color_table, &COLOR_TABLES);
    run_button(ui).then_some(Action::Spectrogram)
}
