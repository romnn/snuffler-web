pub use snuffler_web_core::snufflings::rms::Config;

use super::Action;
use super::widgets::{optional_freq_row, param_row, run_button};

pub fn panel(
    ui: &mut egui::Ui,
    config: &mut Config,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
    nyquist: Option<f64>,
) -> Option<Action> {
    // Qt re-narrows the corner ranges to the loaded band on every pile change
    // (`adjust_controls`): a corner above Nyquist is silently discarded
    // downstream, so the slider must not offer one.
    let fmax = nyquist.unwrap_or(1000.0).clamp(0.002, 1000.0);
    optional_freq_row(ui, "Highpass [Hz]", &mut config.highpass, 0.001, fmax, true);
    optional_freq_row(ui, "Lowpass [Hz]", &mut config.lowpass, 0.001, fmax, false);
    param_row(ui, "Block Length [s]", &mut config.block, 0.1, 3600.0);
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.checkbox(&mut config.group, "Group channels");
    });
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.checkbox(&mut config.log, "Log RMS");
    });
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        if ui.button("Copy passband from Main").clicked() {
            config.highpass = main_highpass;
            config.lowpass = main_lowpass;
        }
    });
    run_button(ui).then_some(Action::Rms)
}
