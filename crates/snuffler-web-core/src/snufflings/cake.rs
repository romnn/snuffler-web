//! Theoretical Cake phase parameters and figure visibility.
use crate::bridge::CakeSnufflingParams;

/// The phases the Cake panel lists before the user adds any.
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

/// Configuration for theoretical phase arrivals and Cake figures.
pub struct Config {
    /// `(phase name, enabled)` — includes the builtin phases plus any added via the "Add
    /// phase" field.
    /// The leading `~` is kept; Python interprets it.
    pub phases: Vec<(String, bool)>,
    /// Available built-in, uploaded, and Green's-function earth models.
    pub available_models: Vec<String>,
    /// Model selected for the next Cake request.
    pub model: String,
    /// Global arrival-time shift in seconds.
    pub time_shift_s: f64,
    /// Whether station depths participate in travel-time calculations.
    pub use_station_depth: bool,
    /// Editable custom phase expression awaiting insertion.
    pub new_phase: String,
    /// Whether the velocity-model figure is open.
    show_model_plot: bool,
    /// Whether the ray-path figure is open.
    show_rays_plot: bool,
}

impl Default for Config {
    fn default() -> Self {
        let phases = PHASE_NAMES
            .iter()
            .enumerate()
            // Only the first phase starts enabled.
            .map(|(index, name)| ((*name).to_owned(), index == 0))
            .collect();
        Self {
            phases,
            available_models: MODELS.iter().map(|model| (*model).to_owned()).collect(),
            model: "ak135-f-continental.m".to_owned(),
            time_shift_s: 0.0,
            use_station_depth: false,
            new_phase: String::new(),
            show_model_plot: false,
            show_rays_plot: false,
        }
    }
}

impl Config {
    /// Serializes parameters for a bridge request.
    #[must_use]
    pub fn params(&self) -> CakeSnufflingParams {
        CakeSnufflingParams {
            model: self.model.clone(),
            phases: self
                .phases
                .iter()
                .filter(|(_, enabled)| *enabled)
                .map(|(name, _)| name.clone())
                .collect(),
            time_shift_s: self.time_shift_s,
            use_station_depth: self.use_station_depth,
            mode: String::new(),
        }
    }

    /// Reports whether the velocity-model figure is open.
    #[must_use]
    pub fn show_model_plot(&self) -> bool {
        self.show_model_plot
    }

    /// Opens the velocity-model figure.
    pub fn open_model_plot(&mut self) {
        self.show_model_plot = true;
    }

    /// Closes the velocity-model figure.
    pub fn close_model_plot(&mut self) {
        self.show_model_plot = false;
    }

    /// Reports whether the ray-path figure is open.
    #[must_use]
    pub fn show_rays_plot(&self) -> bool {
        self.show_rays_plot
    }

    /// Opens the ray-path figure.
    pub fn open_rays_plot(&mut self) {
        self.show_rays_plot = true;
    }

    /// Closes the ray-path figure.
    pub fn close_rays_plot(&mut self) {
        self.show_rays_plot = false;
    }

    /// Register a custom `.nd` model uploaded via "Add Model" and select it.
    pub fn add_model(&mut self, name: String) {
        if !self.available_models.contains(&name) {
            self.available_models.push(name.clone());
        }
        self.model = name;
    }
}
