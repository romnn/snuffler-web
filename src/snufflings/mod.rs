mod amplitude_spectrum;
mod cake;
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
    Cake,
}

impl Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::AmplitudeSpectrum => "Plot Amplitude Spectrum",
            Panel::MinMax => "Minimum / Maximum",
            Panel::Stalta => "STA LTA Detector",
            Panel::Polarization => "Polarization",
            Panel::Rms => "RMS",
            Panel::Cake => "Cake Phase",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Spectrum,
    Run(&'static str),
    Rms,
    /// Open/refresh the Cake Phase "Plot Model" window.
    CakePlotModel,
    /// Open/refresh the Cake Phase "Plot Rays" window.
    CakePlotRays,
    /// Let the user upload a custom `.nd` earth model.
    AddCakeModel,
    /// Mount a Green's-functions store directory (`NativeFS`) and add its 1D
    /// earth model to the dropdown.
    #[cfg(feature = "gf")]
    AddGfStore,
}

#[derive(Default)]
pub struct SnufflingState {
    active_panel: Option<Panel>,
    spectrum: amplitude_spectrum::Config,
    stalta: stalta::Config,
    polarization: polarization::Config,
    rms: rms::Config,
    cake: cake::Config,
}

impl SnufflingState {
    pub fn params(&self) -> SnufflingParams {
        SnufflingParams {
            spectrum: Some(self.spectrum.params()),
            stalta: Some(self.stalta.params()),
            polarization: Some(self.polarization.params()),
            rms: Some(self.rms.params()),
            cake: Some(self.cake.params()),
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

    pub fn show_cake_model_plot(&self) -> bool {
        self.cake.show_model_plot()
    }

    pub fn open_cake_model_plot(&mut self) {
        self.cake.open_model_plot();
    }

    pub fn close_cake_model_plot(&mut self) {
        self.cake.close_model_plot();
    }

    pub fn show_cake_rays_plot(&self) -> bool {
        self.cake.show_rays_plot()
    }

    pub fn open_cake_rays_plot(&mut self) {
        self.cake.open_rays_plot();
    }

    pub fn close_cake_rays_plot(&mut self) {
        self.cake.close_rays_plot();
    }

    /// Open the Cake Phase config panel (used by the Data-menu shortcut).
    pub fn open_cake_panel(&mut self) {
        self.active_panel = Some(Panel::Cake);
    }

    /// Register a custom earth model uploaded via "Add Model" and select it.
    pub fn add_cake_model(&mut self, name: String) {
        self.cake.add_model(name);
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
        ui.separator();
        if ui
            .button("Cake Phase (theoretical arrivals)")
            .on_hover_text(
                "Theoretical phase arrivals through a 1D earth model \
                 (pyrocko.cake): markers, model-velocity plot and ray paths.",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::Cake);
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
                Panel::Cake => cake::panel(ui, &mut state.cake),
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
