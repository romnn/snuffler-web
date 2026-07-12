pub use snuffler_web_core::snufflings::rms::Config;

use super::Action;
use super::widgets::{optional_freq_row, param_row, run_button};

pub fn panel(
    ui: &mut egui::Ui,
    config: &mut Config,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
) -> Option<Action> {
    optional_freq_row(
        ui,
        "Highpass [Hz]",
        &mut config.highpass,
        0.001,
        1000.0,
        true,
    );
    optional_freq_row(
        ui,
        "Lowpass [Hz]",
        &mut config.lowpass,
        0.001,
        1000.0,
        false,
    );
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
