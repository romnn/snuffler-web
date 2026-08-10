//! Three-component polarization parameters and plot visibility.
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
