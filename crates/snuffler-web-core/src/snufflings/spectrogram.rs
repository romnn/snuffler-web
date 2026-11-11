//! Spectrogram parameters and plot visibility.
use crate::bridge::SpectrogramSnufflingParams;

/// Configuration for pyrocko's spectrogram figure.
pub struct Config {
    /// Whether the rendered spectrogram window is open.
    show_plot: bool,
    /// Short-time Fourier transform window length in seconds.
    pub window_length_s: f64,
    /// Overlap between adjacent windows in percent.
    pub overlap_percent: f64,
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
            window_length_s: 100.0,
            overlap_percent: 75.0,
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
            window_length_s: self.window_length_s,
            overlap_percent: self.overlap_percent,
            taper: self.taper.clone(),
            fmin: self.fmin,
            fmax: self.fmax,
            color_scale: self.color_scale.clone(),
            color_table: self.color_table.clone(),
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
