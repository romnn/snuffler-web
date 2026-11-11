//! STA/LTA detector parameters.
use crate::bridge::StaltaSnufflingParams;

/// Configuration for pyrocko's STA/LTA detector.
pub struct Config {
    /// Optional high-pass cutoff in hertz.
    pub highpass: Option<f64>,
    /// Optional low-pass cutoff in hertz.
    pub lowpass: Option<f64>,
    /// Short-term averaging window in seconds.
    pub short_window_s: f64,
    /// Long-to-short window ratio.
    pub long_to_short_window_ratio: f64,
    /// Trigger threshold in scaled detector units.
    pub trigger_level: f64,
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
            short_window_s: 30.0,
            long_to_short_window_ratio: 3.0,
            trigger_level: 0.5,
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
            highpass: self.highpass,
            lowpass: self.lowpass,
            short_window_s: self.short_window_s,
            long_to_short_window_ratio: self.long_to_short_window_ratio,
            trigger_level: self.trigger_level,
            show_level_traces: self.show_level_traces,
            variant: if self.centered_variant {
                "centered"
            } else {
                "right"
            }
            .to_owned(),
            scaling_method: scaling_label(self.scaling_method).to_owned(),
            apply_to_sum: self.apply_to_sum,
        }
    }
}

fn scaling_label(index: usize) -> &'static str {
    match index {
        1 => "[-1/ratio,1]",
        2 => "[-1/ratio,1] clipped to [0,1]",
        _ => "[0-1]",
    }
}
