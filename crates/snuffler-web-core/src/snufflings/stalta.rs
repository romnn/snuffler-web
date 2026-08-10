//! STA/LTA detector parameters.
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
