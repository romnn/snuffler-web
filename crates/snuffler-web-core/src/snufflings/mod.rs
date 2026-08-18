//! Toolkit-neutral snuffling configuration and actions.
pub mod amplitude_spectrum;
pub mod cake;
pub mod polarization;
pub mod rms;
pub mod seisbench;
pub mod seismosizer;
pub mod spectrogram;
pub mod stalta;

use crate::bridge::SnufflingParams;

/// Cross-boundary intent emitted by a snuffling configuration panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Renders an amplitude-spectrum figure.
    Spectrum,
    /// Renders a spectrogram figure.
    Spectrogram,
    /// Renders a polarization figure.
    Polarization,
    /// Runs a named text/marker-producing snuffling.
    Run(&'static str),
    /// One of Qt's four one-click GEOFON event queries over the visible time
    /// range, with this minimum magnitude (0 = all events).
    GeofonPreset(f64),
    /// Qt's `Reload Snufflings` menu item, mapped to the Shift+R reload.
    ReloadSnufflings,
    /// Adds derived RMS traces.
    Rms,
    /// Renders the selected Cake velocity model.
    CakePlotModel,
    /// Renders rays through the selected Cake model.
    CakePlotRays,
    /// Opens the frontend file picker for a custom Cake model.
    AddCakeModel,
    /// Mounts a remote Green's-function store from the configured URL.
    LoadGfStoreUrl,
    /// Opens the frontend directory picker for a Green's-function store.
    #[cfg(feature = "gf")]
    AddGfStore,
}

/// Owns every snuffling's user-editable parameters and persistent plot state.
#[derive(Default)]
pub struct SnufflingConfigs {
    /// Amplitude-spectrum configuration.
    pub spectrum: amplitude_spectrum::Config,
    /// Spectrogram configuration.
    pub spectrogram: spectrogram::Config,
    /// STA/LTA detector configuration.
    pub stalta: stalta::Config,
    /// Polarization configuration.
    pub polarization: polarization::Config,
    /// RMS configuration.
    pub rms: rms::Config,
    /// Cake configuration.
    pub cake: cake::Config,
    /// Seismosizer configuration.
    pub seismosizer: seismosizer::Config,
    /// `SeisBench` picker configuration.
    pub seisbench: seisbench::Config,
}

impl SnufflingConfigs {
    /// Serializes the complete configuration snapshot for a bridge request.
    #[must_use]
    pub fn params(&self) -> SnufflingParams {
        SnufflingParams {
            spectrum: Some(self.spectrum.params()),
            spectrogram: Some(self.spectrogram.params()),
            stalta: Some(self.stalta.params()),
            polarization: Some(self.polarization.params()),
            rms: Some(self.rms.params()),
            cake: Some(self.cake.params()),
            seismosizer: Some(self.seismosizer.params()),
            seisbench: Some(self.seisbench.params()),
        }
    }

    /// Registers a custom earth model and selects it for Cake requests.
    pub fn add_cake_model(&mut self, name: String) {
        self.cake.add_model(name);
    }

    /// Registers a mounted Green's-function store with every compatible snuffling.
    pub fn add_gf_store(&mut self, label: String) {
        self.seismosizer.add_store(label.clone());
        self.cake.add_model(label);
    }
}
