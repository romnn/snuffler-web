pub use snuffler_web_core::snufflings::rms::Config;

use super::Action;
use super::widgets::{checkbox_row, copy_passband_row, param_row, passband_rows, run_button};

pub fn panel(
    ui: &mut egui::Ui,
    config: &mut Config,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
    nyquist: Option<f64>,
) -> Option<Action> {
    passband_rows(ui, &mut config.highpass, &mut config.lowpass, nyquist);
    param_row(ui, "Block Length [s]", &mut config.block, 0.1, 3600.0);
    checkbox_row(ui, &mut config.group, "Group channels");
    checkbox_row(ui, &mut config.log, "Log RMS");
    copy_passband_row(
        ui,
        (&mut config.highpass, &mut config.lowpass),
        (main_highpass, main_lowpass),
    );
    run_button(ui).then_some(Action::Rms)
}
