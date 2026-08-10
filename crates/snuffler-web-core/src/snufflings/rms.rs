//! Blockwise root-mean-square trace parameters.
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
