mod amplitude_spectrum;
mod minmax;
mod polarization;
mod rms;
mod stalta;
mod widgets;

use crate::bridge::SnufflingParams;
use crate::state::ViewMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    AmplitudeSpectrum,
    MinMax,
    Stalta,
    Polarization,
    Rms,
}

impl Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::AmplitudeSpectrum => "Plot Amplitude Spectrum",
            Panel::MinMax => "Minimum / Maximum",
            Panel::Stalta => "STA LTA Detector",
            Panel::Polarization => "Polarization",
            Panel::Rms => "RMS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Spectrum,
    Run(&'static str),
    Rms,
}

#[derive(Default)]
pub struct SnufflingState {
    active_panel: Option<Panel>,
    spectrum: amplitude_spectrum::Config,
    stalta: stalta::Config,
    polarization: polarization::Config,
    rms: rms::Config,
}

impl SnufflingState {
    pub fn params(&self) -> SnufflingParams {
        SnufflingParams {
            spectrum: self.spectrum.params(),
            stalta: self.stalta.params(),
            polarization: self.polarization.params(),
            rms: self.rms.params(),
        }
    }

    pub fn show_spectrum_plot(&self) -> bool {
        self.spectrum.show_plot()
    }

    pub fn open_spectrum_plot(&mut self) {
        self.spectrum.open_plot();
    }

    pub fn close_spectrum_plot(&mut self) {
        self.spectrum.close_plot();
    }
}

pub fn menu(ui: &mut egui::Ui, state: &mut SnufflingState, view_mode: &mut ViewMode) {
    ui.menu_button("Snufflings", |ui| {
        if ui.button("Spectrogram (waterfall)").clicked() {
            *view_mode = ViewMode::Waterfall;
            ui.close();
        }
        if ui.button("Amplitude spectrum").clicked() {
            state.active_panel = Some(Panel::AmplitudeSpectrum);
            ui.close();
        }
        ui.separator();
        if ui.button("Minimum / Maximum (peak-to-peak)").clicked() {
            state.active_panel = Some(Panel::MinMax);
            ui.close();
        }
        if ui.button("STA/LTA detector").clicked() {
            state.active_panel = Some(Panel::Stalta);
            ui.close();
        }
        if ui
            .button("Polarization (3-component)")
            .on_hover_text(
                "Covariance-eigen polarization (azimuth, incidence, \
                 rectilinearity, planarity) per Z/N/E station in the window.",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::Polarization);
            ui.close();
        }
        if ui
            .button("RMS (blockwise)")
            .on_hover_text(
                "Add derived traces of blockwise root-mean-square amplitude \
                 over the visible window (channel suffix -RMS).",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::Rms);
            ui.close();
        }
    });
}

pub fn show_panel(
    ctx: &egui::Context,
    state: &mut SnufflingState,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
) -> Option<Action> {
    let panel = state.active_panel?;
    let mut open = true;
    let mut action = None;
    egui::Window::new(panel.title())
        .open(&mut open)
        .default_width(450.0)
        .show(ctx, |ui| {
            action = match panel {
                Panel::AmplitudeSpectrum => amplitude_spectrum::panel(ui, &mut state.spectrum),
                Panel::MinMax => minmax::panel(ui),
                Panel::Stalta => stalta::panel(ui, &mut state.stalta, main_highpass, main_lowpass),
                Panel::Polarization => {
                    polarization::panel(ui, &mut state.polarization, main_highpass, main_lowpass)
                }
                Panel::Rms => rms::panel(ui, &mut state.rms, main_highpass, main_lowpass),
            };
        });
    if !open {
        state.active_panel = None;
    }
    action
}

pub fn draw_spectrum_plot(ui: &mut egui::Ui, spec: Option<&crate::bridge::SpectrumData>) {
    amplitude_spectrum::draw_plot(ui, spec);
}
