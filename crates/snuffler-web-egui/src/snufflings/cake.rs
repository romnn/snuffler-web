use egui::Align;

pub use snuffler_web_core::snufflings::cake::Config;

use super::Action;
use super::widgets::{checkbox_row, param_row};

const LABEL_WIDTH_PX: f32 = 160.0;

pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    let mut action = None;
    ui.label("Theoretical phase arrivals through a 1D earth model (pyrocko.cake).");
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        let caption = ui.add_sized(
            [LABEL_WIDTH_PX, 0.0],
            egui::Label::new("Model").halign(Align::Max),
        );
        egui::ComboBox::from_id_salt("cake_model")
            .selected_text(config.model.clone())
            .show_ui(ui, |ui| {
                let available_models = config.available_models.clone();
                for model in &available_models {
                    // Qt prefixes the shipped models "Cake builtin: <name>"
                    // (cake_phase.py), distinguishing them from GF-store and
                    // user-loaded models over an otherwise identical set.
                    let label =
                        if model.starts_with("GF Store:") || model.starts_with("Cake builtin:") {
                            model.clone()
                        } else {
                            format!("Cake builtin: {model}")
                        };
                    ui.selectable_value(&mut config.model, model.clone(), label);
                }
            })
            .response
            .labelled_by(caption.id);
        if ui.button("Add Model…").clicked() {
            action = Some(Action::AddCakeModel);
        }
        #[cfg(feature = "gf")]
        if ui
            .button("Add GF Store…")
            .on_hover_text(
                "Mount a Green's-functions store directory and use its embedded \
                 1D earth model (pyrocko.gf).",
            )
            .clicked()
        {
            action = Some(Action::AddGfStore);
        }
    });

    param_row(
        ui,
        "Global shift [s]",
        &mut config.time_shift_s,
        -20.0,
        20.0,
    );
    checkbox_row(ui, &mut config.use_station_depth, "Use station depth");

    ui.add_space(6.0);
    ui.label("Phases:");
    egui::ScrollArea::vertical()
        .max_height(170.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, on) in &mut config.phases {
                    ui.checkbox(on, name.as_str());
                }
            });
        });

    ui.horizontal(|ui| {
        ui.add_sized(
            [LABEL_WIDTH_PX, 0.0],
            egui::Label::new("Add phase").halign(Align::Max),
        );
        let response = ui.text_edit_singleline(&mut config.new_phase);
        let submit = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || submit) && !config.new_phase.trim().is_empty() {
            config
                .phases
                .push((config.new_phase.trim().to_owned(), true));
            config.new_phase.clear();
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(LABEL_WIDTH_PX);
        if ui.button("Plot Model").clicked() {
            action = Some(Action::CakePlotModel);
        }
        if ui.button("Plot Rays").clicked() {
            action = Some(Action::CakePlotRays);
        }
        if ui.button("Run").clicked() {
            action = Some(Action::Run("cake"));
        }
    });

    action
}
