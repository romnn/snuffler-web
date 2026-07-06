use crate::bridge::SeismosizerSnufflingParams;

use super::Action;
use super::widgets::{param_row, run_button};

const WAVEFORM_TYPES: [&str; 6] = [
    "Displacement [m]",
    "Displacement [nm]",
    "Velocity [m/s]",
    "Velocity [nm/s]",
    "Acceleration [m/s^2]",
    "Acceleration [nm/s^2]",
];

pub struct Config {
    /// Selected mounted GF-store label ("GF Store: <id>"), and the available set.
    store: String,
    stores: Vec<String>,
    /// HTTP base URL to mount a GF store from (an alternative to the directory
    /// picker; the base must serve a `.manifest` of its files).
    load_url: String,
    waveform_type: String,
    time: f64,
    north_km: f64,
    east_km: f64,
    depth_km: f64,
    magnitude: f64,
    strike: f64,
    dip: f64,
    rake: f64,
    stf_duration: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            store: String::new(),
            stores: Vec::new(),
            load_url: String::new(),
            waveform_type: "Displacement [m]".to_owned(),
            time: 0.0,
            north_km: 0.0,
            east_km: 0.0,
            depth_km: 10.0,
            magnitude: 6.0,
            strike: 0.0,
            dip: 90.0,
            rake: 0.0,
            stf_duration: 0.0,
        }
    }
}

impl Config {
    pub fn params(&self) -> SeismosizerSnufflingParams {
        SeismosizerSnufflingParams {
            seis_store: self.store.clone(),
            seis_waveform_type: self.waveform_type.clone(),
            seis_time: self.time,
            seis_north_km: self.north_km,
            seis_east_km: self.east_km,
            seis_depth_km: self.depth_km,
            seis_magnitude: self.magnitude,
            seis_strike: self.strike,
            seis_dip: self.dip,
            seis_rake: self.rake,
            seis_stf_duration: self.stf_duration,
        }
    }

    /// Register a mounted GF store (from "Add GF Store…") and select it.
    pub fn add_store(&mut self, label: String) {
        if !self.stores.contains(&label) {
            self.stores.push(label.clone());
        }
        self.store = label;
    }

    pub fn load_url(&self) -> String {
        self.load_url.clone()
    }
}

fn choice_row(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
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
    ui.label("Synthetic seismograms from a double-couple point source (pyrocko.gf).");
    ui.add_space(6.0);
    let mut action = None;

    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
            egui::Label::new("GF Store").halign(egui::Align::Max),
        );
        egui::ComboBox::from_id_salt("seis_store")
            .selected_text(if config.store.is_empty() {
                "<none>"
            } else {
                config.store.as_str()
            })
            .show_ui(ui, |ui| {
                let stores = config.stores.clone();
                for s in &stores {
                    ui.selectable_value(&mut config.store, s.clone(), s);
                }
            });
        #[cfg(feature = "gf")]
        if ui
            .button("Add GF Store…")
            .on_hover_text("Mount a Green's-functions store directory (pyrocko.gf).")
            .clicked()
        {
            action = Some(Action::AddGfStore);
        }
    });

    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 0.0],
            egui::Label::new("… or from URL").halign(egui::Align::Max),
        );
        ui.add(
            egui::TextEdit::singleline(&mut config.load_url)
                .desired_width(220.0)
                .hint_text("https://host/path/to/store"),
        );
        if ui
            .add_enabled(
                !config.load_url.trim().is_empty(),
                egui::Button::new("Load"),
            )
            .on_hover_text("Fetch a GF store's files from an HTTP base URL (needs a .manifest).")
            .clicked()
        {
            action = Some(Action::LoadGfStoreUrl);
        }
    });

    choice_row(
        ui,
        "Waveform type",
        &mut config.waveform_type,
        &WAVEFORM_TYPES,
    );
    param_row(ui, "Time [s]", &mut config.time, -50.0, 50.0);
    param_row(ui, "North shift [km]", &mut config.north_km, -50.0, 50.0);
    param_row(ui, "East shift [km]", &mut config.east_km, -50.0, 50.0);
    param_row(ui, "Depth [km]", &mut config.depth_km, 0.0, 600.0);
    param_row(ui, "Magnitude", &mut config.magnitude, 0.0, 10.0);
    param_row(ui, "Strike", &mut config.strike, -180.0, 180.0);
    param_row(ui, "Dip", &mut config.dip, 0.0, 90.0);
    param_row(ui, "Rake", &mut config.rake, -180.0, 180.0);
    param_row(ui, "STF duration [s]", &mut config.stf_duration, 0.0, 50.0);

    if run_button(ui) {
        action = Some(Action::Run("seismosizer"));
    }
    action
}
