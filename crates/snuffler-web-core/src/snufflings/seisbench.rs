//! `SeisBench` model and phase-threshold parameters.
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
