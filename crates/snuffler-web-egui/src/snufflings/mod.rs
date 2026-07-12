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

use snuffler_web_core::snufflings::SnufflingConfigs;
use snuffler_web_core::state::{AppState, ViewMode};

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

pub use snuffler_web_core::snufflings::Action;

#[derive(Default)]
pub struct SnufflingPanels {
    active_panel: Option<Panel>,
}

impl SnufflingPanels {
    /// Open the Cake Phase config panel (used by the Data-menu shortcut).
    pub fn open_cake_panel(&mut self) {
        self.active_panel = Some(Panel::Cake);
    }
}

pub fn menu(ui: &mut egui::Ui, panels: &mut SnufflingPanels, view_mode: &mut ViewMode) {
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
            panels.active_panel = Some(Panel::Spectrogram);
            ui.close();
        }
        if ui.button("Amplitude spectrum").clicked() {
            panels.active_panel = Some(Panel::AmplitudeSpectrum);
            ui.close();
        }
        ui.separator();
        if ui.button("Minimum / Maximum (peak-to-peak)").clicked() {
            panels.active_panel = Some(Panel::MinMax);
            ui.close();
        }
        if ui.button("STA/LTA detector").clicked() {
            panels.active_panel = Some(Panel::Stalta);
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
            panels.active_panel = Some(Panel::Polarization);
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
            panels.active_panel = Some(Panel::Rms);
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
            panels.active_panel = Some(Panel::Cake);
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
            panels.active_panel = Some(Panel::Seismosizer);
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
            panels.active_panel = Some(Panel::SeisbenchPicker);
            ui.close();
        }
    });
}

pub fn show_panel(
    ctx: &egui::Context,
    panels: &mut SnufflingPanels,
    configs: &mut SnufflingConfigs,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
    seisbench_runtime: &SeisbenchRuntimeStatus,
) -> Option<Action> {
    let panel = panels.active_panel?;
    let mut open = true;
    let mut action = None;
    egui::Window::new(panel.title())
        .open(&mut open)
        .default_width(450.0)
        .show(ctx, |ui| {
            action = match panel {
                Panel::AmplitudeSpectrum => amplitude_spectrum::panel(ui, &mut configs.spectrum),
                Panel::Spectrogram => spectrogram::panel(ui, &mut configs.spectrogram),
                Panel::MinMax => minmax::panel(ui),
                Panel::Stalta => {
                    stalta::panel(ui, &mut configs.stalta, main_highpass, main_lowpass)
                }
                Panel::Polarization => {
                    polarization::panel(ui, &mut configs.polarization, main_highpass, main_lowpass)
                }
                Panel::Rms => rms::panel(ui, &mut configs.rms, main_highpass, main_lowpass),
                Panel::Cake => cake::panel(ui, &mut configs.cake),
                Panel::Seismosizer => seismosizer::panel(ui, &mut configs.seismosizer),
                Panel::SeisbenchPicker => {
                    seisbench::panel(ui, &mut configs.seisbench, seisbench_runtime)
                }
            };
        });
    if !open {
        panels.active_panel = None;
    }
    action
}

pub fn show_output(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_snuffling {
        return;
    }
    let mut open = true;
    let text = state.snuffling_text.clone();
    egui::Window::new("Snuffling output")
        .open(&mut open)
        .default_size([480.0, 260.0])
        .show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.monospace(text.as_str());
            });
        });
    state.show_snuffling = open;
}
