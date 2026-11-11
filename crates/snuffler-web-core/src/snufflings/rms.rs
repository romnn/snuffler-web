//! Blockwise root-mean-square trace parameters.
use crate::bridge::RmsSnufflingParams;

/// Configuration for derived RMS traces.
pub struct Config {
    /// Optional high-pass cutoff in hertz.
    pub highpass: Option<f64>,
    /// Optional low-pass cutoff in hertz.
    pub lowpass: Option<f64>,
    /// RMS block length in seconds.
    pub block_length_s: f64,
    /// Whether channels are grouped before RMS calculation.
    pub group_components: bool,
    /// Whether RMS output is logarithmic.
    pub logarithmic: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            highpass: None,
            lowpass: None,
            block_length_s: 100.0,
            group_components: true,
            logarithmic: false,
        }
    }
}

impl Config {
    /// Serializes parameters for a bridge request.
    #[must_use]
    pub fn params(&self) -> RmsSnufflingParams {
        RmsSnufflingParams {
            highpass: self.highpass,
            lowpass: self.lowpass,
            block_length_s: self.block_length_s,
            group_components: self.group_components,
            logarithmic: self.logarithmic,
        }
    }
}
