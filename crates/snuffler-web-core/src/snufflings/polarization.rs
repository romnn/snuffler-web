//! Three-component polarization parameters and plot visibility.
use crate::bridge::PolarizationSnufflingParams;

/// Configuration for pyrocko's polarization analysis.
pub struct Config {
    /// Whether the rendered polarization window is open.
    show_plot: bool,
    /// Analysis-window length in seconds.
    pub window_length_s: f64,
    /// Window offset as a fraction of its length.
    pub window_offset_fraction: f64,
    /// Horizontal rotation azimuth in degrees.
    pub azimuth_deg: f64,
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
            window_length_s: 1.0,
            window_offset_fraction: 0.0,
            azimuth_deg: 0.0,
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
            window_length_s: self.window_length_s,
            window_offset_fraction: self.window_offset_fraction,
            azimuth_deg: self.azimuth_deg,
            highpass: self.highpass,
            lowpass: self.lowpass,
            dot_position: self.dot_position,
            rotate_to_rt: self.rotate_to_rt,
            show_eigensystem_2d: self.show_eigensystem_2d,
            show_eigensystem_3d: self.show_eigensystem_3d,
            fix_scale: self.fix_scale,
        }
    }
}
