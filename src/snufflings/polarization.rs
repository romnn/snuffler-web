use egui::Align;

use crate::bridge::PolarizationSnufflingParams;

use super::Action;
use super::widgets::{optional_freq_row, param_row};

pub struct Config {
    show_plot: bool,
    length: f64,
    offset: f64,
    azimuth: f64,
    highpass: Option<f64>,
    lowpass: Option<f64>,
    dot_position: f64,
    rotate_to_rt: bool,
    show_eigensystem_2d: bool,
    show_eigensystem_3d: bool,
    fix_scale: bool,
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
    pub fn show_plot(&self) -> bool {
        self.show_plot
    }

    pub fn open_plot(&mut self) {
        self.show_plot = true;
    }

    pub fn close_plot(&mut self) {
        self.show_plot = false;
    }

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

pub fn panel(
    ui: &mut egui::Ui,
    config: &mut Config,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
) -> Option<Action> {
    ui.label("Analyze 3-component polarization around selected marker anchors.");
    ui.add_space(6.0);
    param_row(ui, "Length [s]", &mut config.length, 0.001, 1000.0);
    param_row(ui, "Offset (relative)", &mut config.offset, -5.0, 5.0);
    param_row(ui, "Delta Azimuth", &mut config.azimuth, -180.0, 180.0);
    optional_freq_row(
        ui,
        "Highpass [Hz]",
        &mut config.highpass,
        0.001,
        1000.0,
        true,
    );
    optional_freq_row(
        ui,
        "Lowpass [Hz]",
        &mut config.lowpass,
        0.001,
        1000.0,
        false,
    );
    param_row(ui, "Dot Position", &mut config.dot_position, 0.0, 1.0);
    checkbox_row(ui, &mut config.rotate_to_rt, "Rotate to RT");
    checkbox_row(ui, &mut config.show_eigensystem_2d, "Show 2D Eigensystems");
    checkbox_row(ui, &mut config.show_eigensystem_3d, "Show 3D Eigensystem");
    checkbox_row(ui, &mut config.fix_scale, "Fix Scale");
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        if ui.button("Copy passband from Main").clicked() {
            config.highpass = main_highpass;
            config.lowpass = main_lowpass;
        }
    });
    ui.add_space(8.0);
    let mut action = None;
    ui.horizontal(|ui| {
        ui.add_space(160.0);
        if ui
            .button("Plot")
            .on_hover_text("Per-station polarization figure: hodograms + eigensystem (pyrocko).")
            .clicked()
        {
            action = Some(Action::Polarization);
        }
        if ui
            .button("Run (text)")
            .on_hover_text("Numeric azimuth / incidence / rectilinearity / planarity table.")
            .clicked()
        {
            action = Some(Action::Run("polarization"));
        }
    });
    action
}

fn checkbox_row(ui: &mut egui::Ui, value: &mut bool, label: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([160.0, 0.0], egui::Label::new("").halign(Align::Max));
        ui.checkbox(value, label);
    });
}
