//! Amplitude-spectrum parameters and plot visibility.
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
            smoothing: self.smoothing,
            smoothing_bandwidth: self.smoothing_bandwidth,
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
