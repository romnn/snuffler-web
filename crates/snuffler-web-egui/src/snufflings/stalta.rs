use egui::Align;

pub use snuffler_web_core::snufflings::stalta::Config;

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
    param_row(
        ui,
        "Short Window [s]",
        &mut config.short_window_s,
        0.01,
        7200.0,
    );
    param_row(
        ui,
        "Ratio",
        &mut config.long_to_short_window_ratio,
        1.1,
        20.0,
    );
    param_row(ui, "Level", &mut config.trigger_level, 0.0, 1.0);
    checkbox_row(
        ui,
        &mut config.show_level_traces,
        "Show trigger level traces",
    );
    ui.horizontal(|ui| {
        let caption = ui.add_sized([160.0, 0.0], egui::Label::new("Variant").halign(Align::Max));
        ui.selectable_value(&mut config.centered_variant, true, "centered")
            .labelled_by(caption.id);
        ui.selectable_value(&mut config.centered_variant, false, "right")
            .labelled_by(caption.id);
    });
    ui.horizontal(|ui| {
        let caption = ui.add_sized(
            [160.0, 0.0],
            egui::Label::new("Scaling/Normalization").halign(Align::Max),
        );
        egui::ComboBox::from_id_salt("stalta_scaling_method")
            .selected_text(scaling_label(config.scaling_method))
            .show_ui(ui, |ui| {
                for (idx, label) in ["[0-1]", "[-1/ratio,1]", "[-1/ratio,1] clipped to [0,1]"]
                    .iter()
                    .enumerate()
                {
                    ui.selectable_value(&mut config.scaling_method, idx, *label);
                }
            })
            .response
            .labelled_by(caption.id);
    });
    checkbox_row(ui, &mut config.apply_to_sum, "Detect on sum trace");
    copy_passband_row(
        ui,
        (&mut config.highpass, &mut config.lowpass),
        (main_highpass, main_lowpass),
    );
    run_button(ui).then_some(Action::Run("stalta"))
}

fn scaling_label(idx: usize) -> &'static str {
    match idx {
        1 => "[-1/ratio,1]",
        2 => "[-1/ratio,1] clipped to [0,1]",
        _ => "[0-1]",
    }
}
