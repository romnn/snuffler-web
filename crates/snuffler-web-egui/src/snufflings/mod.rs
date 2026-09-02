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

/// The set of open snuffling panels. Qt gives every snuffling its own dock
/// widget alongside the others (snuffling.py:226), so panels coexist here too:
/// opening one never closes another.
#[derive(Default)]
pub struct SnufflingPanels {
    open_panels: Vec<Panel>,
}

impl SnufflingPanels {
    fn open(&mut self, panel: Panel) {
        if !self.open_panels.contains(&panel) {
            self.open_panels.push(panel);
        }
    }

    /// Open the Cake Phase config panel (used by the Data-menu shortcut).
    pub fn open_cake_panel(&mut self) {
        self.open(Panel::Cake);
    }
}

/// The Snufflings menu opens configuration panels; work starts only from the
/// panel's explicit Run or Plot control.
#[expect(
    clippy::too_many_lines,
    reason = "one menu, one entry per snuffling; splitting it hides the order"
)]
pub fn menu(
    ui: &mut egui::Ui,
    panels: &mut SnufflingPanels,
    state: &mut AppState,
) -> Option<Action> {
    let mut action = None;
    ui.menu_button("Snufflings", |ui| {
        if ui.button("Waterfall (inline spectrogram)").clicked() {
            state.view_mode = ViewMode::Waterfall;
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
            panels.open(Panel::Spectrogram);
            action = Some(Action::Spectrogram);
            ui.close();
        }
        if ui.button("Amplitude spectrum").clicked() {
            panels.open(Panel::AmplitudeSpectrum);
            action = Some(Action::Spectrum);
            ui.close();
        }
        ui.separator();
        if ui.button("Minimum / Maximum (peak-to-peak)").clicked() {
            panels.open(Panel::MinMax);
            ui.close();
        }
        if ui.button("STA/LTA detector").clicked() {
            panels.open(Panel::Stalta);
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
            panels.open(Panel::Polarization);
            action = Some(Action::Polarization);
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
            panels.open(Panel::Rms);
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
            panels.open(Panel::Cake);
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
            panels.open(Panel::Seismosizer);
            ui.close();
        }
        ui.separator();
        // Qt's `Reload Snufflings` menu item. Its Qt semantic — re-scanning the
        // user's `snuffling_paths` by mtime — has nothing to reload here (a
        // browser page carries no user snuffling directory), so the entry
        // reloads the Python side the way Shift+R does.
        if ui
            .button("Reload Snufflings")
            .on_hover_text("Reload the Python snuffling code (same as Shift+R).")
            .clicked()
        {
            action = Some(Action::ReloadSnufflings);
            ui.close();
        }
        // Qt's four one-click GEOFON event queries (`Get GEOFON Events`),
        // served over FDSN instead of the eqinfo scrape a browser cannot reach.
        ui.menu_button("Get GEOFON Events", |ui| {
            for (label, minmag) in [
                ("All events", 0.0),
                ("Events, M ≥ 6", 6.0),
                ("Events, M ≥ 7", 7.0),
                ("Events, M ≥ 8", 8.0),
            ] {
                if ui.button(label).clicked() {
                    action = Some(Action::GeofonPreset(minmag));
                    ui.close();
                }
            }
        });
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
            panels.open(Panel::SeisbenchPicker);
            ui.close();
        }
        ui.separator();
        // Qt files its panel togglers — Markers, Main Controls, every
        // snuffling dock — under this same menu (`toggle_panel_menu`,
        // pile_viewer.py:860 with snuffler_app.py:667): the two built-in
        // panels are `add_panel` peers of the snuffling docks. Checkboxes
        // rather than Qt's show-only actions: `m` and `c` still toggle.
        ui.checkbox(&mut state.show_controls, "Main Controls");
        ui.checkbox(&mut state.show_marker_editor, "Markers");
    });
    action
}

pub fn show_panel(
    ctx: &egui::Context,
    panels: &mut SnufflingPanels,
    configs: &mut SnufflingConfigs,
    main_highpass: Option<f64>,
    main_lowpass: Option<f64>,
    nyquist: Option<f64>,
    seisbench_runtime: &SeisbenchRuntimeStatus,
) -> Option<Action> {
    // Every open panel draws, its own window each (Qt's tabified docks). One
    // frame carries at most one click, so the first action is the only one.
    let mut action = None;
    panels.open_panels.retain(|&panel| {
        let mut open = true;
        egui::Window::new(panel.title())
            .open(&mut open)
            .default_width(450.0)
            .show(ctx, |ui| {
                let panel_action = match panel {
                    Panel::AmplitudeSpectrum => {
                        amplitude_spectrum::panel(ui, &mut configs.spectrum)
                    }
                    Panel::Spectrogram => spectrogram::panel(ui, &mut configs.spectrogram, nyquist),
                    Panel::MinMax => minmax::panel(ui),
                    Panel::Stalta => stalta::panel(
                        ui,
                        &mut configs.stalta,
                        main_highpass,
                        main_lowpass,
                        nyquist,
                    ),
                    Panel::Polarization => polarization::panel(
                        ui,
                        &mut configs.polarization,
                        main_highpass,
                        main_lowpass,
                        nyquist,
                    ),
                    Panel::Rms => {
                        rms::panel(ui, &mut configs.rms, main_highpass, main_lowpass, nyquist)
                    }
                    Panel::Cake => cake::panel(ui, &mut configs.cake),
                    Panel::Seismosizer => seismosizer::panel(ui, &mut configs.seismosizer),
                    Panel::SeisbenchPicker => {
                        seisbench::panel(ui, &mut configs.seisbench, seisbench_runtime)
                    }
                };
                if action.is_none() {
                    action = panel_action;
                }
            });
        open
    });
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
