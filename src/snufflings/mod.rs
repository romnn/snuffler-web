mod amplitude_spectrum;
mod cake;
mod minmax;
mod polarization;
mod rms;
mod seisbench;
mod seismosizer;
mod spectrogram;
mod stalta;
mod widgets;

use crate::bridge::SnufflingParams;
use crate::state::ViewMode;

pub use seisbench::{Progress as SeisbenchProgress, RuntimeStatus as SeisbenchRuntimeStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    AmplitudeSpectrum,
    Spectrogram,
    MinMax,
    Stalta,
    Polarization,
    Rms,
    Cake,
    Seismosizer,
    SeisbenchPicker,
}

impl Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::AmplitudeSpectrum => "Plot Amplitude Spectrum",
            Panel::Spectrogram => "Plot Spectrogram",
            Panel::MinMax => "Minimum / Maximum",
            Panel::Stalta => "STA LTA Detector",
            Panel::Polarization => "Plot Polarization",
            Panel::Rms => "RMS",
            Panel::Cake => "Cake Phase",
            Panel::Seismosizer => "Seismosizer",
            Panel::SeisbenchPicker => "SeisBench Picker",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Spectrum,
    Spectrogram,
    Polarization,
    Run(&'static str),
    Rms,
    /// Open/refresh the Cake Phase "Plot Model" window.
    CakePlotModel,
    /// Open/refresh the Cake Phase "Plot Rays" window.
    CakePlotRays,
    /// Let the user upload a custom `.nd` earth model.
    AddCakeModel,
    /// Mount a Green's-functions store by fetching it from an HTTP URL.
    LoadGfStoreUrl,
    /// Mount a Green's-functions store directory (`NativeFS`) and add its 1D
    /// earth model to the dropdown.
    #[cfg(feature = "gf")]
    AddGfStore,
}

#[derive(Default)]
pub struct SnufflingState {
    active_panel: Option<Panel>,
    spectrum: amplitude_spectrum::Config,
    spectrogram: spectrogram::Config,
    stalta: stalta::Config,
    polarization: polarization::Config,
    rms: rms::Config,
    cake: cake::Config,
    seismosizer: seismosizer::Config,
    seisbench: seisbench::Config,
}

impl SnufflingState {
    pub fn params(&self) -> SnufflingParams {
        SnufflingParams {
            spectrum: Some(self.spectrum.params()),
            spectrogram: Some(self.spectrogram.params()),
            stalta: Some(self.stalta.params()),
            polarization: Some(self.polarization.params()),
            rms: Some(self.rms.params()),
            cake: Some(self.cake.params()),
            seismosizer: Some(self.seismosizer.params()),
            seisbench: Some(self.seisbench.params()),
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

    pub fn show_spectrogram_plot(&self) -> bool {
        self.spectrogram.show_plot()
    }

    pub fn open_spectrogram_plot(&mut self) {
        self.spectrogram.open_plot();
    }

    pub fn close_spectrogram_plot(&mut self) {
        self.spectrogram.close_plot();
    }

    pub fn show_polarization_plot(&self) -> bool {
        self.polarization.show_plot()
    }

    pub fn open_polarization_plot(&mut self) {
        self.polarization.open_plot();
    }

    pub fn close_polarization_plot(&mut self) {
        self.polarization.close_plot();
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

    /// Register a newly-mounted GF store with both panels that can use it (the
    /// Cake model dropdown and the Seismosizer store selector).
    pub fn add_gf_store(&mut self, label: String) {
        self.seismosizer.add_store(label.clone());
        self.cake.add_model(label);
    }

    /// The HTTP base URL typed into the Seismosizer's "from URL" field.
    pub fn seis_load_url(&self) -> String {
        self.seismosizer.load_url()
    }
}

pub fn menu(ui: &mut egui::Ui, state: &mut SnufflingState, view_mode: &mut ViewMode) {
    ui.menu_button("Snufflings", |ui| {
        if ui.button("Waterfall (inline spectrogram)").clicked() {
            *view_mode = ViewMode::Waterfall;
            ui.close();
        }
        if ui
            .button("Spectrogram (figure)")
            .on_hover_text(
                "Windowed STFT spectrogram of the visible channels as a \
                 matplotlib figure (pyrocko snufflings/spectrogram.py).",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::Spectrogram);
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
        if ui
            .button("Seismosizer (synthetics)")
            .on_hover_text(
                "Synthetic seismograms from a double-couple point source through a \
                 mounted Green's-functions store (pyrocko.gf).",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::Seismosizer);
            ui.close();
        }
        ui.separator();
        if ui
            .button("SeisBench picker (models)")
            .on_hover_text(
                "Deep-learning P/S phase picker: exported SeisBench ONNX models \
                 run in-browser through onnxruntime-web over the 3-component data \
                 in the visible window.",
            )
            .clicked()
        {
            state.active_panel = Some(Panel::SeisbenchPicker);
            ui.close();
        }
    });
}

pub fn show_panel(
    ctx: &egui::Context,
    state: &mut SnufflingState,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
    seisbench_runtime: &SeisbenchRuntimeStatus,
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
                Panel::Spectrogram => spectrogram::panel(ui, &mut state.spectrogram),
                Panel::MinMax => minmax::panel(ui),
                Panel::Stalta => stalta::panel(ui, &mut state.stalta, main_highpass, main_lowpass),
                Panel::Polarization => {
                    polarization::panel(ui, &mut state.polarization, main_highpass, main_lowpass)
                }
                Panel::Rms => rms::panel(ui, &mut state.rms, main_highpass, main_lowpass),
                Panel::Cake => cake::panel(ui, &mut state.cake),
                Panel::Seismosizer => seismosizer::panel(ui, &mut state.seismosizer),
                Panel::SeisbenchPicker => {
                    seisbench::panel(ui, &mut state.seisbench, seisbench_runtime)
                }
            };
        });
    if !open {
        state.active_panel = None;
    }
    action
}
