use egui::{Align2, Color32, FontId, Sense, Shape, Stroke};

use crate::bridge::{Bridge, SpectrumData, Status, ViewParams};
use crate::state::{AppState, ViewMode};
use crate::view::markers::MARKER_COLORS;
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};

pub struct App {
    state: AppState,
    bridge: Bridge,
    /// Scene generation we last fit the view window to (resets on new data).
    last_scene_gen: u64,
    wf_cache: WaterfallCache,
    /// Params the spectrum snuffling was last requested for (refetch gate).
    spectrum_key: Option<ViewParams>,
}

fn configure_look(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
    // Follow the OS light/dark preference by default.
    ctx.set_theme(egui::ThemePreference::System);
    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(19.0, Proportional)),
        (TextStyle::Body, FontId::new(14.5, Proportional)),
        (TextStyle::Monospace, FontId::new(13.5, Monospace)),
        (TextStyle::Button, FontId::new(14.5, Proportional)),
        (TextStyle::Small, FontId::new(12.0, Proportional)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    ctx.set_global_style(style);
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_look(&cc.egui_ctx);
        let bridge = Bridge::new();
        bridge.load_example(&cc.egui_ctx);
        Self {
            state: AppState::default(),
            bridge,
            last_scene_gen: 0,
            wf_cache: WaterfallCache::default(),
            spectrum_key: None,
        }
    }

    fn current_params(&self, npx: u32) -> ViewParams {
        ViewParams {
            tmin: self.state.tmin,
            tmax: self.state.tmax,
            npx,
            highpass: self.state.highpass,
            lowpass: self.state.lowpass,
            sort: self.state.sort.as_str().to_string(),
            rotate: self.state.rotate_deg,
            remove_response: self.state.remove_response,
            mode: self.state.view_mode.as_str().to_string(),
        }
    }
}

fn guess_fmt(name: &str) -> String {
    let n = name.to_lowercase();
    if n.ends_with(".mseed") || n.ends_with(".msd") || n.ends_with(".seed") {
        "mseed".into()
    } else if n.ends_with(".sac") {
        "sac".into()
    } else {
        "detect".into()
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Fit the view window whenever a genuinely new scene is loaded.
        let gen = self.bridge.scene_gen();
        if gen != self.last_scene_gen {
            if let Some(scene) = self.bridge.scene() {
                if !scene.tracks.is_empty() {
                    self.state.tmin = scene.tmin;
                    self.state.tmax = scene.tmax;
                    self.state.ntracks_shown = scene.tracks.len().clamp(1, 12);
                    self.state.track_offset = 0;
                    self.last_scene_gen = gen;
                }
            }
        }

        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            if let Some(bytes) = &f.bytes {
                let name = if f.name.is_empty() {
                    "dropped".to_string()
                } else {
                    f.name.clone()
                };
                let fmt = guess_fmt(&name);
                self.bridge.load_file(&ctx, name, bytes.to_vec(), fmt);
            }
        }

        let total_tracks = self.bridge.scene().map(|s| s.tracks.len()).unwrap_or(0);

        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Synthetic example").clicked() {
                        self.bridge.load_example(&ctx);
                        ui.close();
                    }
                    if ui.button("Load bundled test.mseed").clicked() {
                        self.bridge
                            .load_url(&ctx, "test.mseed".into(), "mseed".into());
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Drag & drop SAC / MiniSEED files");
                });
                ui.menu_button("View", |ui| {
                    ui.label("Theme");
                    egui::global_theme_preference_buttons(ui);
                    ui.separator();
                    ui.checkbox(&mut self.state.show_controls, "Show controls");
                    ui.selectable_value(&mut self.state.view_mode, ViewMode::Wiggle, "Wiggle");
                    ui.selectable_value(
                        &mut self.state.view_mode,
                        ViewMode::Waterfall,
                        "Waterfall (spectrogram)",
                    );
                    ui.separator();
                    if ui.button("Reset view to full extent").clicked() {
                        if let Some(scene) = self.bridge.scene() {
                            self.state.tmin = scene.tmin;
                            self.state.tmax = scene.tmax;
                            self.state.gain = 1.0;
                        }
                        ui.close();
                    }
                });
                ui.menu_button("Options", |ui| {
                    ui.checkbox(&mut self.state.remove_response, "Remove response")
                        .on_hover_text(
                            "Deconvolve the instrument response. Needs response \
                             metadata (StationXML/RESP); the demo data has none, so this \
                             is currently a no-op.",
                        );
                });
                ui.menu_button("Snufflings", |ui| {
                    if ui.button("Spectrogram (waterfall)").clicked() {
                        self.state.view_mode = ViewMode::Waterfall;
                        ui.close();
                    }
                    if ui.button("Amplitude spectrum").clicked() {
                        self.state.show_spectrum = true;
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Wheel: zoom time · Shift+Wheel: scroll tracks · Ctrl+Wheel: gain");
                    ui.label("Drag: pan · Shift+Drag: span marker · Double-click: marker");
                    ui.label(
                        "0–7: marker color · F1–F10: phase · Backspace: delete · Space/B: step",
                    );
                });

                ui.separator();
                match self.bridge.status() {
                    Status::Booting => {
                        ui.spinner();
                        ui.label("Starting Python (Pyodide)…");
                    }
                    Status::Loading(msg) => {
                        ui.spinner();
                        ui.label(msg);
                    }
                    Status::Ready => {
                        ui.label(format!("Ready · {total_tracks} traces"));
                    }
                    Status::Error(e) => {
                        ui.colored_label(egui::Color32::RED, format!("Error: {e}"));
                    }
                }
            });
        });

        if self.state.show_controls {
            egui::Panel::bottom("controls").show_inside(ui, |ui| {
                controls::show(ui, &mut self.state, total_tracks.max(1));
            });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                let npx = ui.available_width().max(64.0) as u32;
                let params = self.current_params(npx);
                self.bridge.ensure_view(&ctx, params);
                let epoch = self.bridge.view_epoch();
                let view = self.bridge.view();
                waveform::show(
                    ui,
                    &mut self.state,
                    view.as_deref(),
                    &mut self.wf_cache,
                    epoch,
                );
            });

        // Amplitude-spectrum snuffling window.
        if self.state.show_spectrum {
            let params = self.current_params(800);
            if self.spectrum_key.as_ref() != Some(&params) {
                self.bridge.fetch_spectrum(&ctx, params.clone());
                self.spectrum_key = Some(params);
            }
            let spectrum = self.bridge.spectrum();
            let mut open = true;
            egui::Window::new("Amplitude spectrum")
                .open(&mut open)
                .default_size([520.0, 280.0])
                .show(&ctx, |ui| draw_spectrum(ui, &spectrum));
            if !open {
                self.state.show_spectrum = false;
                self.spectrum_key = None;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}

fn draw_spectrum(ui: &mut egui::Ui, spec: &Option<SpectrumData>) {
    let Some(spec) = spec else {
        ui.label("Computing…");
        return;
    };
    if spec.meta.tracks.is_empty() {
        ui.label("No spectrum (web only).");
        return;
    }
    let dark = ui.visuals().dark_mode;
    let (resp, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width().max(240.0), 240.0),
        Sense::hover(),
    );
    let rect = resp.rect;
    let bg = if dark {
        Color32::from_rgb(22, 24, 27)
    } else {
        Color32::from_rgb(250, 250, 250)
    };
    painter.rect_filled(rect, 0.0, bg);

    let fmin = spec.meta.fmin.max(1e-6);
    let fmax = spec.meta.fmax.max(fmin * 1.0001);
    let (lxmin, lxmax) = (fmin.log10(), fmax.log10());
    let ymin = spec
        .meta
        .tracks
        .iter()
        .map(|t| t.ymin)
        .fold(f32::INFINITY, f32::min);
    let ymax = spec
        .meta
        .tracks
        .iter()
        .map(|t| t.ymax)
        .fold(f32::NEG_INFINITY, f32::max);
    let yspan = (ymax - ymin).max(1e-6);
    let x_of = |f: f64| rect.left() + ((f.log10() - lxmin) / (lxmax - lxmin)) as f32 * rect.width();
    let y_of = |v: f32| rect.bottom() - (v - ymin) / yspan * rect.height();

    let grid = if dark {
        Color32::from_rgb(48, 51, 56)
    } else {
        Color32::from_rgb(225, 225, 225)
    };
    let mut decade = 10f64.powf(lxmin.floor());
    while decade <= fmax {
        if decade >= fmin {
            let x = x_of(decade);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, grid),
            );
        }
        decade *= 10.0;
    }

    for (ti, t) in spec.meta.tracks.iter().enumerate() {
        let col = MARKER_COLORS[ti % 8];
        let end = (t.offset + t.count).min(spec.ys.len());
        let data = &spec.ys[t.offset.min(spec.ys.len())..end];
        if data.len() < 2 {
            continue;
        }
        let n = data.len();
        let pts: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let frac = i as f64 / (n - 1) as f64;
                let f = 10f64.powf(lxmin + frac * (lxmax - lxmin));
                egui::pos2(x_of(f), y_of(v))
            })
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.0, col)));
    }
    painter.text(
        rect.left_top() + egui::vec2(5.0, 3.0),
        Align2::LEFT_TOP,
        format!("{fmin:.3} – {fmax:.0} Hz  (log freq, log amp)"),
        FontId::proportional(11.0),
        if dark {
            Color32::GRAY
        } else {
            Color32::DARK_GRAY
        },
    );
}
