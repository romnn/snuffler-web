pub use snuffler_web_core::snufflings::amplitude_spectrum::Config;

use super::Action;
use super::widgets::{param_row, run_button};

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
