use egui::Align;

use crate::bridge::CakeSnufflingParams;

use super::Action;
use super::widgets::param_row;

/// Theoretical phase names offered as switches, matching the Qt Cake Phase
/// snuffling (`gui/snuffler/snufflings/cake_phase.py`). A leading `~` selects a
/// raw cake phase definition; the rest use the classic-phase expansion. Only the
/// first (`~P`) is enabled by default, as in Qt.
const PHASE_NAMES: &[&str] = &[
    "~P", "Pg", "Sg", "pP", "p", "P", "Pdiff", "PKP", "PcP", "PcS", "PKIKP", "pPKIKP", "SSP",
    "PPS", "SPP", "PSP", "SP", "PS", "~PS", "~SP", "Pn", "s", "S", "Sn", "PP", "PPP", "ScS",
    "Sdiff", "SS", "SSS", "SKS", "SKIKS",
];

/// Builtin cake earth models shipped as build assets (`ensureEarthModels` in
/// web/bridge.js stages the matching `.nd` files into the Pyodide FS).
const MODELS: &[&str] = &[
    "ak135-f-average-no-ocean.f",
    "ak135-f-average-no-ocean.l",
    "ak135-f-average-no-ocean.m",
    "ak135-f-average.f",
    "ak135-f-average.l",
    "ak135-f-average.m",
    "ak135-f-average.vf",
    "ak135-f-continental.f",
    "ak135-f-continental.l",
    "ak135-f-continental.m",
    "prem-no-ocean.f",
    "prem-no-ocean.l",
    "prem-no-ocean.m",
    "prem.f",
    "prem.l",
    "prem.m",
];

const LABEL_W: f32 = 160.0;

pub struct Config {
    /// `(phase name, enabled)` — includes the builtin phases plus any added via
    /// the "Add phase" field. The leading `~` is kept; Python interprets it.
    phases: Vec<(String, bool)>,
    models: Vec<String>,
    chosen_model: String,
    tshift: f64,
    use_station_depth: bool,
    new_phase: String,
    show_model_plot: bool,
    show_rays_plot: bool,
}

impl Default for Config {
    fn default() -> Self {
        let phases = PHASE_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| ((*name).to_owned(), i == 0))
            .collect();
        Self {
            phases,
            models: MODELS.iter().map(|m| (*m).to_owned()).collect(),
            chosen_model: "ak135-f-continental.m".to_owned(),
            tshift: 0.0,
            use_station_depth: false,
            new_phase: String::new(),
            show_model_plot: false,
            show_rays_plot: false,
        }
    }
}

impl Config {
    pub fn params(&self) -> CakeSnufflingParams {
        CakeSnufflingParams {
            cake_model: self.chosen_model.clone(),
            cake_phases: self
                .phases
                .iter()
                .filter(|(_, on)| *on)
                .map(|(name, _)| name.clone())
                .collect(),
            cake_tshift: self.tshift,
            cake_use_station_depth: self.use_station_depth,
            cake_mode: String::new(),
        }
    }

    pub fn show_model_plot(&self) -> bool {
        self.show_model_plot
    }

    pub fn open_model_plot(&mut self) {
        self.show_model_plot = true;
    }

    pub fn close_model_plot(&mut self) {
        self.show_model_plot = false;
    }

    pub fn show_rays_plot(&self) -> bool {
        self.show_rays_plot
    }

    pub fn open_rays_plot(&mut self) {
        self.show_rays_plot = true;
    }

    pub fn close_rays_plot(&mut self) {
        self.show_rays_plot = false;
    }

    /// Register a custom `.nd` model uploaded via "Add Model" and select it.
    pub fn add_model(&mut self, name: String) {
        if !self.models.contains(&name) {
            self.models.push(name.clone());
        }
        self.chosen_model = name;
    }
}

pub fn panel(ui: &mut egui::Ui, config: &mut Config) -> Option<Action> {
    let mut action = None;
    ui.label("Theoretical phase arrivals through a 1D earth model (pyrocko.cake).");
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.add_sized([LABEL_W, 0.0], egui::Label::new("Model").halign(Align::Max));
        egui::ComboBox::from_id_salt("cake_model")
            .selected_text(config.chosen_model.clone())
            .show_ui(ui, |ui| {
                let models = config.models.clone();
                for m in &models {
                    ui.selectable_value(&mut config.chosen_model, m.clone(), m);
                }
            });
        if ui.button("Add Model…").clicked() {
            action = Some(Action::AddCakeModel);
        }
    });

    param_row(ui, "Global shift [s]", &mut config.tshift, -20.0, 20.0);
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        ui.checkbox(&mut config.use_station_depth, "Use station depth");
    });

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
            [LABEL_W, 0.0],
            egui::Label::new("Add phase").halign(Align::Max),
        );
        let resp = ui.text_edit_singleline(&mut config.new_phase);
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || submit) && !config.new_phase.trim().is_empty() {
            config
                .phases
                .push((config.new_phase.trim().to_owned(), true));
            config.new_phase.clear();
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
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
