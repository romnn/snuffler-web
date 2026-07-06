use crate::bridge::SpectrogramSnufflingParams;

use super::Action;
use super::widgets::{optional_freq_row, param_row, run_button};

const TAPERS: [&str; 4] = ["Hanning", "Hamming", "Blackman", "Bartlett"];
const COLOR_SCALES: [&str; 3] = ["log", "sqrt", "lin"];
const COLOR_TABLES: [&str; 2] = ["spectro", "rainbow"];

pub struct Config {
    show_plot: bool,
    twin: f64,
    overlap: f64,
    taper: String,
    fmin: Option<f64>,
    fmax: Option<f64>,
    color_scale: String,
    color_table: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_plot: false,
            twin: 100.0,
            overlap: 75.0,
            taper: "Hanning".to_owned(),
            fmin: None,
            fmax: None,
            color_scale: "log".to_owned(),
            color_table: "spectro".to_owned(),
        }
    }
}

impl Config {
    pub fn params(&self) -> SpectrogramSnufflingParams {
        SpectrogramSnufflingParams {
            spectrogram_twin: self.twin,
            spectrogram_overlap: self.overlap,
            spectrogram_taper: self.taper.clone(),
            spectrogram_fmin: self.fmin,
            spectrogram_fmax: self.fmax,
            spectrogram_color_scale: self.color_scale.clone(),
            spectrogram_color_table: self.color_table.clone(),
        }
    }

    pub fn show_plot(&self) -> bool {
        self.show_plot
    }

    pub fn open_plot(&mut self) {
        self.show_plot = true;
    }

    pub fn close_plot(&mut self) {
        self.show_plot = false;
    }
}

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
