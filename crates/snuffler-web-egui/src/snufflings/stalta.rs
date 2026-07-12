use egui::Align;

pub use snuffler_web_core::snufflings::stalta::Config;

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
    param_row(
        ui,
        "Short Window [s]",
        &mut config.short_window,
        0.01,
        7200.0,
    );
    param_row(ui, "Ratio", &mut config.ratio, 1.1, 20.0);
    param_row(ui, "Level", &mut config.level, 0.0, 1.0);
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.checkbox(&mut config.show_level_traces, "Show trigger level traces");
    });
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 0.0], egui::Label::new("Variant").halign(Align::Max));
        ui.selectable_value(&mut config.centered_variant, true, "centered");
        ui.selectable_value(&mut config.centered_variant, false, "right");
    });
    ui.horizontal(|ui| {
        ui.add_sized(
            [160.0, 0.0],
            egui::Label::new("Scaling/Normalization").halign(Align::Max),
        );
        egui::ComboBox::from_id_salt("stalta_scalingmethod")
            .selected_text(scaling_label(config.scaling_method))
            .show_ui(ui, |ui| {
                for (idx, label) in ["[0-1]", "[-1/ratio,1]", "[-1/ratio,1] clipped to [0,1]"]
                    .iter()
                    .enumerate()
                {
                    ui.selectable_value(&mut config.scaling_method, idx, *label);
                }
            });
    });
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        ui.checkbox(&mut config.apply_to_sum, "Detect on sum trace");
    });
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        if ui.button("Copy passband from Main").clicked() {
            config.highpass = main_highpass;
            config.lowpass = main_lowpass;
        }
    });
    run_button(ui).then_some(Action::Run("stalta"))
}

fn scaling_label(idx: usize) -> &'static str {
    match idx {
        1 => "[-1/ratio,1]",
        2 => "[-1/ratio,1] clipped to [0,1]",
        _ => "[0-1]",
    }
}
