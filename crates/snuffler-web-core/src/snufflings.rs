//! Toolkit-neutral snuffling configuration and actions.
use crate::bridge::SnufflingParams;

/// Amplitude-spectrum parameters and plot visibility.
pub mod amplitude_spectrum {
    use crate::bridge::SpectrumSnufflingParams;

    /// Configuration for pyrocko's amplitude-spectrum snuffling.
    pub struct Config {
        /// Whether the rendered spectrum window is open.
        show_plot: bool,
        /// Whether pyrocko smooths the spectrum.
        pub smoothing: bool,
        /// Smoothing bandwidth in spectral bins.
        pub smoothing_bandwidth: f64,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                show_plot: false,
                smoothing: false,
                smoothing_bandwidth: 40.0,
            }
        }
    }

    impl Config {
        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> SpectrumSnufflingParams {
            SpectrumSnufflingParams {
                spec_smoothing: self.smoothing,
                spec_smoothing_bandwidth: self.smoothing_bandwidth,
            }
        }

        /// Reports whether the spectrum window is open.
        #[must_use]
        pub fn show_plot(&self) -> bool {
            self.show_plot
        }

        /// Opens the spectrum window.
        pub fn open_plot(&mut self) {
            self.show_plot = true;
        }

        /// Closes the spectrum window.
        pub fn close_plot(&mut self) {
            self.show_plot = false;
        }
    }
}

/// Theoretical Cake phase parameters and figure visibility.
pub mod cake {
    use crate::bridge::CakeSnufflingParams;

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
        /// `(phase name, enabled)` — includes the builtin phases plus any added via
        /// the "Add phase" field. The leading `~` is kept; Python interprets it.
        pub phases: Vec<(String, bool)>,
        /// Available built-in, uploaded, and Green's-function earth models.
        pub models: Vec<String>,
        /// Model selected for the next Cake request.
        pub chosen_model: String,
        /// Global arrival-time shift in seconds.
        pub tshift: f64,
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
        /// Serializes parameters for a bridge request.
        #[must_use]
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
            if !self.models.contains(&name) {
                self.models.push(name.clone());
            }
            self.chosen_model = name;
        }
    }
}

/// Three-component polarization parameters and plot visibility.
pub mod polarization {
    use crate::bridge::PolarizationSnufflingParams;

    /// Configuration for pyrocko's polarization analysis.
    pub struct Config {
        /// Whether the rendered polarization window is open.
        show_plot: bool,
        /// Analysis-window length in seconds.
        pub length: f64,
        /// Window offset as a fraction of its length.
        pub offset: f64,
        /// Horizontal rotation azimuth in degrees.
        pub azimuth: f64,
        /// Optional high-pass cutoff in hertz.
        pub highpass: Option<f64>,
        /// Optional low-pass cutoff in hertz.
        pub lowpass: Option<f64>,
        /// Fractional position of the animated time marker.
        pub dot_position: f64,
        /// Whether horizontal components rotate into radial/transverse coordinates.
        pub rotate_to_rt: bool,
        /// Whether the two-dimensional eigensystem is drawn.
        pub show_eigensystem_2d: bool,
        /// Whether the three-dimensional eigensystem is drawn.
        pub show_eigensystem_3d: bool,
        /// Whether all panels share a fixed scale.
        pub fix_scale: bool,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                show_plot: false,
                length: 1.0,
                offset: 0.0,
                azimuth: 0.0,
                highpass: None,
                lowpass: None,
                dot_position: 0.0,
                rotate_to_rt: false,
                show_eigensystem_2d: false,
                show_eigensystem_3d: false,
                fix_scale: false,
            }
        }
    }

    impl Config {
        /// Reports whether the polarization window is open.
        #[must_use]
        pub fn show_plot(&self) -> bool {
            self.show_plot
        }

        /// Opens the polarization window.
        pub fn open_plot(&mut self) {
            self.show_plot = true;
        }

        /// Closes the polarization window.
        pub fn close_plot(&mut self) {
            self.show_plot = false;
        }

        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> PolarizationSnufflingParams {
            PolarizationSnufflingParams {
                polarization_t_length: self.length,
                polarization_ft_offset: self.offset,
                polarization_azimuth: self.azimuth,
                polarization_highpass: self.highpass,
                polarization_lowpass: self.lowpass,
                polarization_dot_position: self.dot_position,
                polarization_rotate_to_rt: self.rotate_to_rt,
                polarization_show_eigensystem_2d: self.show_eigensystem_2d,
                polarization_show_eigensystem_3d: self.show_eigensystem_3d,
                polarization_fix_scale: self.fix_scale,
            }
        }
    }
}

/// Blockwise root-mean-square trace parameters.
pub mod rms {
    use crate::bridge::RmsSnufflingParams;

    /// Configuration for derived RMS traces.
    pub struct Config {
        /// Optional high-pass cutoff in hertz.
        pub highpass: Option<f64>,
        /// Optional low-pass cutoff in hertz.
        pub lowpass: Option<f64>,
        /// RMS block length in seconds.
        pub block: f64,
        /// Whether channels are grouped before RMS calculation.
        pub group: bool,
        /// Whether RMS output is logarithmic.
        pub log: bool,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                highpass: None,
                lowpass: None,
                block: 100.0,
                group: true,
                log: false,
            }
        }
    }

    impl Config {
        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> RmsSnufflingParams {
            RmsSnufflingParams {
                rms_highpass: self.highpass,
                rms_lowpass: self.lowpass,
                rms_block: self.block,
                rms_group: self.group,
                rms_log: self.log,
            }
        }
    }
}

/// `SeisBench` model and phase-threshold parameters.
pub mod seisbench {
    use crate::bridge::SeisbenchSnufflingParams;

    #[derive(Clone, Copy)]
    struct ModelChoice {
        model: &'static str,
        weights: &'static str,
        p_threshold: f64,
        s_threshold: f64,
    }

    const DEFAULT_MODEL: ModelChoice = ModelChoice {
        model: "PhaseNet",
        weights: "instance",
        p_threshold: 0.086_398_242_819_485_67,
        s_threshold: 0.119_433_303_464_473_5,
    };

    const MODELS: &[ModelChoice] = &[
        DEFAULT_MODEL,
        ModelChoice {
            model: "EQTransformer",
            weights: "original",
            p_threshold: 0.1,
            s_threshold: 0.1,
        },
        ModelChoice {
            model: "GPD",
            weights: "stead",
            p_threshold: 0.740_418_855_563_376_4,
            s_threshold: 0.753_522_613_817_665_6,
        },
    ];

    /// Configuration for the in-browser `SeisBench` picker.
    pub struct Config {
        /// Index into the supported model list.
        pub selected: usize,
        /// Whether user-entered thresholds replace model defaults.
        pub override_thresholds: bool,
        /// P-phase probability threshold.
        pub p_threshold: f64,
        /// S-phase probability threshold.
        pub s_threshold: f64,
    }

    impl Default for Config {
        fn default() -> Self {
            let choice = DEFAULT_MODEL;
            Self {
                selected: 0,
                override_thresholds: false,
                p_threshold: choice.p_threshold,
                s_threshold: choice.s_threshold,
            }
        }
    }

    impl Config {
        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> SeisbenchSnufflingParams {
            let choice = selected_model(self.selected);
            SeisbenchSnufflingParams {
                model: choice.model.to_owned(),
                weights: choice.weights.to_owned(),
                p_threshold: self.override_thresholds.then_some(self.p_threshold),
                s_threshold: self.override_thresholds.then_some(self.s_threshold),
                model_base_url: model_base_url().to_owned(),
            }
        }
    }

    fn selected_model(selected: usize) -> ModelChoice {
        MODELS.get(selected).copied().unwrap_or(DEFAULT_MODEL)
    }

    fn model_base_url() -> &'static str {
        if cfg!(feature = "embed-data") {
            "assets/models/"
        } else {
            option_env!("SNUFFLER_ONNX_MODEL_BASE_URL").unwrap_or("")
        }
    }
}

/// Green's-function synthetic waveform parameters.
pub mod seismosizer {
    use crate::bridge::SeismosizerSnufflingParams;

    /// Configuration for pyrocko's Seismosizer snuffling.
    pub struct Config {
        /// Selected mounted GF-store label ("GF Store: <id>"), and the available set.
        pub store: String,
        /// Mounted Green's-function stores available to the selector.
        pub stores: Vec<String>,
        /// HTTP base URL to mount a GF store from (an alternative to the directory
        /// picker; the base must serve a `.manifest` of its files).
        pub load_url: String,
        /// Requested displacement, velocity, or acceleration unit.
        pub waveform_type: String,
        /// Source-time offset in seconds.
        pub time: f64,
        /// Source north offset in kilometres.
        pub north_km: f64,
        /// Source east offset in kilometres.
        pub east_km: f64,
        /// Source depth in kilometres.
        pub depth_km: f64,
        /// Moment magnitude of the double-couple source.
        pub magnitude: f64,
        /// Source strike in degrees.
        pub strike: f64,
        /// Source dip in degrees.
        pub dip: f64,
        /// Source rake in degrees.
        pub rake: f64,
        /// Source-time-function duration in seconds.
        pub stf_duration: f64,
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
        /// Serializes parameters for a bridge request.
        #[must_use]
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

        /// Returns the editable remote store base URL.
        #[must_use]
        pub fn load_url(&self) -> String {
            self.load_url.clone()
        }
    }
}

/// Spectrogram parameters and plot visibility.
pub mod spectrogram {
    use crate::bridge::SpectrogramSnufflingParams;

    /// Configuration for pyrocko's spectrogram figure.
    pub struct Config {
        /// Whether the rendered spectrogram window is open.
        show_plot: bool,
        /// Short-time Fourier transform window length in seconds.
        pub twin: f64,
        /// Overlap between adjacent windows in percent.
        pub overlap: f64,
        /// Window taper name understood by pyrocko.
        pub taper: String,
        /// Optional lower frequency bound in hertz.
        pub fmin: Option<f64>,
        /// Optional upper frequency bound in hertz.
        pub fmax: Option<f64>,
        /// Power-to-color transfer function.
        pub color_scale: String,
        /// Matplotlib color-table name.
        pub color_table: String,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                show_plot: false,
                twin: 100.0,
                overlap: 75.0,
                taper: "Hanning".to_owned(),
                fmin: None,
                fmax: None,
                color_scale: "log".to_owned(),
                color_table: "spectro".to_owned(),
            }
        }
    }

    impl Config {
        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> SpectrogramSnufflingParams {
            SpectrogramSnufflingParams {
                spectrogram_twin: self.twin,
                spectrogram_overlap: self.overlap,
                spectrogram_taper: self.taper.clone(),
                spectrogram_fmin: self.fmin,
                spectrogram_fmax: self.fmax,
                spectrogram_color_scale: self.color_scale.clone(),
                spectrogram_color_table: self.color_table.clone(),
            }
        }

        /// Reports whether the spectrogram window is open.
        #[must_use]
        pub fn show_plot(&self) -> bool {
            self.show_plot
        }

        /// Opens the spectrogram window.
        pub fn open_plot(&mut self) {
            self.show_plot = true;
        }

        /// Closes the spectrogram window.
        pub fn close_plot(&mut self) {
            self.show_plot = false;
        }
    }
}

/// STA/LTA detector parameters.
pub mod stalta {
    use crate::bridge::StaltaSnufflingParams;

    /// Configuration for pyrocko's STA/LTA detector.
    pub struct Config {
        /// Optional high-pass cutoff in hertz.
        pub highpass: Option<f64>,
        /// Optional low-pass cutoff in hertz.
        pub lowpass: Option<f64>,
        /// Short-term averaging window in seconds.
        pub short_window: f64,
        /// Long-to-short window ratio.
        pub ratio: f64,
        /// Trigger threshold in scaled detector units.
        pub level: f64,
        /// Whether detector-level traces are added to the pile.
        pub show_level_traces: bool,
        /// Whether the short window is centered instead of right-aligned.
        pub centered_variant: bool,
        /// Index of pyrocko's detector scaling method.
        pub scaling_method: usize,
        /// Whether channels are summed before detection.
        pub apply_to_sum: bool,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                highpass: None,
                lowpass: None,
                short_window: 30.0,
                ratio: 3.0,
                level: 0.5,
                show_level_traces: false,
                centered_variant: true,
                scaling_method: 0,
                apply_to_sum: false,
            }
        }
    }

    impl Config {
        /// Serializes parameters for a bridge request.
        #[must_use]
        pub fn params(&self) -> StaltaSnufflingParams {
            StaltaSnufflingParams {
                stalta_highpass: self.highpass,
                stalta_lowpass: self.lowpass,
                stalta_swin: self.short_window,
                stalta_ratio: self.ratio,
                stalta_level: self.level,
                stalta_show_level_traces: self.show_level_traces,
                stalta_variant: if self.centered_variant {
                    "centered"
                } else {
                    "right"
                }
                .to_owned(),
                stalta_scalingmethod: scaling_label(self.scaling_method).to_owned(),
                stalta_apply_to_sum: self.apply_to_sum,
            }
        }
    }

    fn scaling_label(idx: usize) -> &'static str {
        match idx {
            1 => "[-1/ratio,1]",
            2 => "[-1/ratio,1] clipped to [0,1]",
            _ => "[0-1]",
        }
    }
}

/// Cross-boundary intent emitted by a snuffling configuration panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Renders an amplitude-spectrum figure.
    Spectrum,
    /// Renders a spectrogram figure.
    Spectrogram,
    /// Renders a polarization figure.
    Polarization,
    /// Runs a named text/marker-producing snuffling.
    Run(&'static str),
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
