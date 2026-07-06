use crate::bridge::SpectrumSnufflingParams;

use super::Action;
use super::widgets::{param_row, run_button};

pub struct Config {
    show_plot: bool,
    smoothing: bool,
    smoothing_bandwidth: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_plot: false,
            smoothing: false,
            smoothing_bandwidth: 40.0,
        }
    }
}

impl Config {
    pub fn params(&self) -> SpectrumSnufflingParams {
        SpectrumSnufflingParams {
            spec_smoothing: self.smoothing,
            spec_smoothing_bandwidth: self.smoothing_bandwidth,
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

pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    ui.label("Plot amplitude spectra for the selected/visible traces.");
    ui.add_space(6.0);
    ui.checkbox(&mut config.smoothing, "Smoothing");
    param_row(
        ui,
        "Smoothing band width",
        &mut config.smoothing_bandwidth,
        1.0,
        100.0,
    );
    run_button(ui).then_some(Action::Spectrum)
}
