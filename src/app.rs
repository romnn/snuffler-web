use egui::{Align2, Color32, FontId, Sense, Shape, Stroke};

use crate::bridge::{Bridge, SpectrumData, Status, ViewParams};
use crate::state::{
    AppState, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength,
};
use crate::view::markers::{MARKER_COLORS, Marker};
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
    /// Snuffling-result generation we last applied (markers/text once each).
    last_snuffling_gen: u64,
    map: crate::view::map::MapState,
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
    #[must_use]
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
            last_snuffling_gen: 0,
            map: crate::view::map::MapState::default(),
        }
    }

    fn current_params(&self, npx: u32) -> ViewParams {
        let s = &self.state;
        ViewParams {
            tmin: s.tmin,
            tmax: s.tmax,
            npx,
            highpass: s.highpass,
            lowpass: s.lowpass,
            sort: s.sort.as_str().to_string(),
            subsort: s.subsort.as_str().to_string(),
            rotate: s.rotate_deg,
            remove_response: s.remove_response,
            mode: s.view_mode.as_str().to_string(),
            demean: s.demean,
            degap: s.degap,
            allow_downsampling: s.allow_downsampling,
            fft_filtering: s.fft_filtering,
            bandpass_lphp: s.bandpass_lphp,
            distances_3d: s.distances_3d,
            scaling_base: s.scaling_base.as_str().to_string(),
            scaling_group: s.scaling_group.as_str().to_string(),
            wf_abs: s.wf_abs,
            wf_integrate: s.wf_integrate,
            wf_color_scale: s.wf_color_scale.as_str().to_string(),
        }
    }

    fn file_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("File", |ui| {
            if ui.button("Open files…").clicked() {
                self.bridge.open_files(ctx);
                ui.close();
            }
            if ui.button("Synthetic example").clicked() {
                self.bridge.load_example(ctx);
                ui.close();
            }
            if ui.button("Load bundled test.mseed").clicked() {
                self.bridge
                    .load_url(ctx, "test.mseed".into(), "mseed".into());
                ui.close();
            }
            ui.separator();
            if ui.button("Save markers…").clicked() {
                self.bridge
                    .save_markers(ctx, markers_to_json(&self.state.markers));
                ui.close();
            }
            if ui.button("Load markers…").clicked() {
                self.bridge.load_markers(ctx);
                ui.close();
            }
            ui.separator();
            ui.label("Drag & drop SAC / MiniSEED files to load");
        });
    }

    fn data_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("Data", |ui| {
            if ui.button("Download waveforms (FDSN)…").clicked() {
                self.state.show_download = true;
                ui.close();
            }
            if ui.button("Catalog search (events)…").clicked() {
                self.state.show_catalog = true;
                ui.close();
            }
            if ui.button("Map (stations & events)…").clicked() {
                self.state.show_map = true;
                ui.close();
            }
            ui.separator();
            if ui
                .button("Add theoretical arrivals (cake)")
                .on_hover_text(
                    "Compute P/S travel times (ak135) for the active catalog event \
                     at the loaded stations and add them as phase markers.",
                )
                .clicked()
            {
                self.bridge.cake_arrivals(ctx);
                ui.close();
            }
            ui.separator();
            ui.label("Real data via the browser's fetch() (CORS-enabled FDSN).");
        });
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            ui.label("Theme");
            egui::global_theme_preference_buttons(ui);
            ui.separator();

            ui.label("Visible length");
            for vl in VisibleLength::ALL {
                if ui
                    .selectable_value(&mut self.state.visible_length, vl, vl.label())
                    .clicked()
                {
                    let max = vl.seconds();
                    if self.state.tmax - self.state.tmin > max {
                        let c = 0.5 * (self.state.tmin + self.state.tmax);
                        self.state.tmin = c - max / 2.0;
                        self.state.tmax = c + max / 2.0;
                    }
                }
            }
            ui.separator();

            ui.selectable_value(&mut self.state.view_mode, ViewMode::Wiggle, "Wiggle plot");
            ui.selectable_value(&mut self.state.view_mode, ViewMode::Waterfall, "Waterfall");
            ui.separator();

            ui.checkbox(&mut self.state.clip_traces, "Clip traces");
            ui.checkbox(&mut self.state.show_boxes, "Show boxes");
            ui.checkbox(&mut self.state.color_traces, "Color traces");
            ui.checkbox(&mut self.state.antialiasing, "Antialiasing");
            ui.separator();
            ui.checkbox(&mut self.state.show_scale_ranges, "Show scale ranges");
            ui.checkbox(&mut self.state.show_scale_axes, "Show scale axes");
            ui.checkbox(&mut self.state.show_zero_lines, "Show zero lines");
            ui.separator();
            ui.checkbox(&mut self.state.show_controls, "Show main controls");
            ui.checkbox(&mut self.state.show_marker_editor, "Marker editor");
            if ui.button("Reset view to full extent").clicked() {
                if let Some(scene) = self.bridge.scene() {
                    self.state.tmin = scene.tmin;
                    self.state.tmax = scene.tmax;
                    self.state.gain = 1.0;
                }
                ui.close();
            }
        });
    }

    fn options_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Options", |ui| {
            ui.checkbox(&mut self.state.demean, "Demean");
            ui.checkbox(&mut self.state.remove_response, "Deconvolution")
                .on_hover_text(
                    "Deconvolve the instrument response to displacement. Fetch \
                     responses first via Data ▸ Download ▸ Fetch responses.",
                );
            ui.checkbox(&mut self.state.distances_3d, "3D distances")
                .on_hover_text("Use station depth in distance sorting (needs coordinates).");
            ui.separator();
            ui.checkbox(&mut self.state.allow_downsampling, "Allow downsampling");
            ui.checkbox(&mut self.state.degap, "Allow degapping");
            ui.separator();
            ui.checkbox(&mut self.state.fft_filtering, "FFT filtering")
                .on_hover_text("Brick-wall FFT filtering instead of Butterworth.");
            ui.checkbox(&mut self.state.bandpass_lphp, "Bandpass is Low- + Highpass");
        });
    }

    fn scaling_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Scaling", |ui| {
            for g in [
                ScalingGroup::Individual,
                ScalingGroup::Common,
                ScalingGroup::PerStation,
                ScalingGroup::PerStationLocation,
                ScalingGroup::PerComponent,
            ] {
                ui.selectable_value(&mut self.state.scaling_group, g, g.label());
            }
            ui.separator();
            for b in [
                ScalingBase::MinMax,
                ScalingBase::Robust,
                ScalingBase::Mean2Std,
                ScalingBase::Mean4Std,
            ] {
                ui.selectable_value(&mut self.state.scaling_base, b, b.label());
            }
            ui.separator();
            ui.checkbox(&mut self.state.fix_scale_ranges, "Fix scale ranges");
        });
    }

    fn sorting_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Sorting", |ui| {
            for m in [
                SortMode::Name,
                SortMode::Distance,
                SortMode::Azimuth,
                SortMode::AzimuthBlocks,
                SortMode::Backazimuth,
            ] {
                let r = ui.selectable_value(&mut self.state.sort, m, m.label());
                if m.needs_coords() {
                    r.on_hover_text(
                        "Needs station coordinates + an active event; \
                         falls back to name order without them.",
                    );
                }
            }
            ui.separator();
            ui.label("Subsort / grouping");
            for ss in [
                Subsort::Nslc,
                Subsort::Nscl,
                Subsort::Sncl,
                Subsort::Lnsc,
                Subsort::Cnsl,
                Subsort::NscGroupL,
                Subsort::SncGroupL,
            ] {
                ui.selectable_value(&mut self.state.subsort, ss, ss.label());
            }
        });
    }

    fn snufflings_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("Snufflings", |ui| {
            if ui.button("Spectrogram (waterfall)").clicked() {
                self.state.view_mode = ViewMode::Waterfall;
                ui.close();
            }
            if ui.button("Amplitude spectrum").clicked() {
                self.state.show_spectrum = true;
                ui.close();
            }
            ui.separator();
            if ui.button("Minimum / Maximum (peak-to-peak)").clicked() {
                let p = self.current_params(800);
                self.bridge.run_snuffling(ctx, "minmax".into(), p);
                ui.close();
            }
            if ui.button("STA/LTA detector").clicked() {
                let p = self.current_params(800);
                self.bridge.run_snuffling(ctx, "stalta".into(), p);
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
                let p = self.current_params(800);
                self.bridge.run_snuffling(ctx, "polarization".into(), p);
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
                let p = self.current_params(800);
                self.bridge.rms_traces(ctx, p);
                ui.close();
            }
        });
    }

    fn help_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Help", |ui| {
            if ui.button("Snuffler controls").clicked() {
                self.state.show_help = true;
                ui.close();
            }
            if ui.button("About").clicked() {
                self.state.show_about = true;
                ui.close();
            }
        });
    }
}

fn guess_fmt(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.eq_ignore_ascii_case("mseed")
        || ext.eq_ignore_ascii_case("msd")
        || ext.eq_ignore_ascii_case("seed")
    {
        "mseed".into()
    } else if ext.eq_ignore_ascii_case("sac") {
        "sac".into()
    } else {
        "detect".into()
    }
}

impl eframe::App for App {
    #[allow(
        clippy::too_many_lines,
        reason = "cohesive draw/update routine; splitting would obscure the control flow"
    )]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pixel width is a small non-negative value clamped to >= 64.0; truncation to u32 is intended"
    )]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Fit the view window whenever a genuinely new scene is loaded.
        let scene_gen = self.bridge.scene_gen();
        if scene_gen != self.last_scene_gen
            && let Some(scene) = self.bridge.scene()
            && !scene.tracks.is_empty()
        {
            self.state.tmin = scene.tmin;
            self.state.tmax = scene.tmax;
            self.state.ntracks_shown = scene
                .tracks
                .len()
                .clamp(1, crate::state::DEFAULT_NTRACKS_SHOWN);
            self.state.track_offset = 0;
            self.last_scene_gen = scene_gen;
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

        let total_tracks = self.bridge.scene().map_or(0, |s| s.tracks.len());

        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                self.file_menu(ui, &ctx);
                self.data_menu(ui, &ctx);
                self.view_menu(ui);
                self.options_menu(ui);
                self.scaling_menu(ui);
                self.sorting_menu(ui);
                self.snufflings_menu(ui, &ctx);
                self.help_menu(ui);

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

        // Apply outcomes of any ':' command that resolved since last frame.
        if let Some(t) = self.bridge.take_goto() {
            let w = (self.state.tmax - self.state.tmin).max(1.0);
            self.state.tmin = t - w * 0.5;
            self.state.tmax = t + w * 0.5;
        }
        if let Some(msg) = self.bridge.take_command_msg() {
            self.state.command_feedback = msg;
        }

        if self.state.show_command {
            egui::Panel::bottom("cmdline").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(":");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.state.command_text)
                            .desired_width(f32::INFINITY)
                            .hint_text(
                                "goto <time> · hide [n|s|l|c] <pat> · unhide · n|s|l|c <pat>",
                            ),
                    );
                    resp.request_focus();
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let line = std::mem::take(&mut self.state.command_text);
                        if !line.trim().is_empty() {
                            self.state.command_feedback.clear();
                            self.bridge.run_command(&ctx, line);
                        }
                        self.state.show_command = false;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.state.command_text.clear();
                        self.state.show_command = false;
                    }
                });
                if !self.state.command_feedback.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xC0, 0x60, 0x40),
                        &self.state.command_feedback,
                    );
                }
            });
        }

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
                .show(&ctx, |ui| draw_spectrum(ui, spectrum.as_ref()));
            if !open {
                self.state.show_spectrum = false;
                self.spectrum_key = None;
            }
        }

        if self.state.show_help {
            let mut open = true;
            egui::Window::new("Snuffler controls")
                .open(&mut open)
                .default_size([460.0, 380.0])
                .show(&ctx, help_text);
            self.state.show_help = open;
        }
        if self.state.show_about {
            let mut open = true;
            egui::Window::new("About")
                .open(&mut open)
                .show(&ctx, about_text);
            self.state.show_about = open;
        }

        if self.state.show_download {
            let mut open = true;
            let mut submit = false;
            let mut submit_st = false;
            let mut submit_resp = false;
            egui::Window::new("Download waveforms (FDSN)")
                .open(&mut open)
                .default_width(420.0)
                .show(&ctx, |ui| {
                    egui::ComboBox::from_label("Data center")
                        .selected_text(self.state.dl_datacenter.clone())
                        .show_ui(ui, |ui| {
                            for dc in ["geofon", "earthscope", "orfeus"] {
                                ui.selectable_value(
                                    &mut self.state.dl_datacenter,
                                    dc.to_string(),
                                    dc,
                                );
                            }
                        });
                    egui::Grid::new("dl_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            for (lbl, val) in [
                                ("Network", &mut self.state.dl_net),
                                ("Station", &mut self.state.dl_sta),
                                ("Location", &mut self.state.dl_loc),
                                ("Channel", &mut self.state.dl_cha),
                                ("Start (UTC)", &mut self.state.dl_start),
                                ("End (UTC)", &mut self.state.dl_end),
                            ] {
                                ui.label(lbl);
                                ui.text_edit_singleline(val);
                                ui.end_row();
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Fetch waveforms").clicked() {
                            submit = true;
                        }
                        if ui.button("Fetch station coords").clicked() {
                            submit_st = true;
                        }
                        if ui.button("Fetch responses").clicked() {
                            submit_resp = true;
                        }
                    });
                    ui.small(
                        "Wildcards * and ? allowed (e.g. channel BH? = BHZ/BHN/BHE). \
                         Responses enable Options ▸ Deconvolution.",
                    );
                });
            if submit {
                let p = serde_json::json!({
                    "datacenter": self.state.dl_datacenter, "net": self.state.dl_net,
                    "sta": self.state.dl_sta, "loc": self.state.dl_loc, "cha": self.state.dl_cha,
                    "start": self.state.dl_start, "end": self.state.dl_end,
                })
                .to_string();
                self.bridge.download_waveforms(&ctx, p);
            }
            if submit_st || submit_resp {
                let p = serde_json::json!({
                    "datacenter": self.state.dl_datacenter, "net": self.state.dl_net,
                    "sta": self.state.dl_sta, "cha": self.state.dl_cha,
                })
                .to_string();
                if submit_st {
                    self.bridge.fetch_stations(&ctx, p.clone());
                }
                if submit_resp {
                    self.bridge.fetch_responses(&ctx, p);
                }
            }
            self.state.show_download = open;
        }

        if self.state.show_catalog {
            let mut open = true;
            let mut submit = false;
            egui::Window::new("Catalog search (events)")
                .open(&mut open)
                .default_width(380.0)
                .show(&ctx, |ui| {
                    egui::ComboBox::from_label("Catalog")
                        .selected_text(self.state.cat_datacenter.clone())
                        .show_ui(ui, |ui| {
                            for dc in ["usgs", "geofon"] {
                                ui.selectable_value(
                                    &mut self.state.cat_datacenter,
                                    dc.to_string(),
                                    dc,
                                );
                            }
                        });
                    egui::Grid::new("cat_grid").num_columns(2).show(ui, |ui| {
                        ui.label("Start (UTC)");
                        ui.text_edit_singleline(&mut self.state.cat_start);
                        ui.end_row();
                        ui.label("End (UTC)");
                        ui.text_edit_singleline(&mut self.state.cat_end);
                        ui.end_row();
                    });
                    ui.add(
                        egui::Slider::new(&mut self.state.cat_minmag, 0.0..=9.0)
                            .text("Min magnitude"),
                    );
                    if ui.button("Search").clicked() {
                        submit = true;
                    }
                    ui.small("Events become markers on the time axis at their origin times.");
                });
            if submit {
                let p = serde_json::json!({
                    "datacenter": self.state.cat_datacenter, "minmag": self.state.cat_minmag,
                    "start": self.state.cat_start, "end": self.state.cat_end,
                })
                .to_string();
                self.bridge.catalog_search(&ctx, p);
            }
            self.state.show_catalog = open;
        }

        if self.state.show_map {
            let mut open = true;
            egui::Window::new("Map — stations & events")
                .open(&mut open)
                .default_size([640.0, 480.0])
                .show(&ctx, |ui| {
                    crate::view::map::show(ui, &mut self.map, &self.bridge, &ctx);
                });
            self.state.show_map = open;
        }

        if self.state.show_marker_editor {
            let mut open = true;
            let mut center_t: Option<f64> = None;
            egui::Window::new("Markers")
                .open(&mut open)
                .default_size([560.0, 320.0])
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} markers", self.state.markers.len()));
                        if ui.button("Delete selected").clicked() {
                            self.state.markers.retain(|m| !m.selected);
                        }
                        if ui.button("Clear all").clicked() {
                            self.state.markers.clear();
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        egui::Grid::new("marker_table")
                            .striped(true)
                            .num_columns(7)
                            .spacing([10.0, 3.0])
                            .show(ui, |ui| {
                                ui.strong("");
                                ui.strong("time (UTC)");
                                ui.strong("span");
                                ui.strong("phase");
                                ui.strong("pol");
                                ui.strong("channel");
                                ui.strong("");
                                ui.end_row();
                                for m in &mut self.state.markers {
                                    let col = MARKER_COLORS
                                        .get((m.kind & 7) as usize)
                                        .copied()
                                        .unwrap_or(Color32::from_rgb(204, 0, 0));
                                    let dot =
                                        egui::RichText::new(if m.is_event { "▲" } else { "●" })
                                            .color(col);
                                    if ui.selectable_label(m.selected, dot).clicked() {
                                        m.selected = !m.selected;
                                    }
                                    if ui
                                        .selectable_label(
                                            m.selected,
                                            crate::view::timeaxis::format_full(m.tmin),
                                        )
                                        .clicked()
                                    {
                                        m.selected = !m.selected;
                                    }
                                    let span = m.tmax - m.tmin;
                                    ui.label(if span > 0.0 {
                                        format!("{span:.3} s")
                                    } else {
                                        "—".to_string()
                                    });
                                    ui.label(m.phase.clone().unwrap_or_default());
                                    let pol = match m.polarity {
                                        Some(1) => "+",
                                        Some(-1) => "-",
                                        Some(0) => "0",
                                        _ => "—",
                                    };
                                    if ui
                                        .small_button(pol)
                                        .on_hover_text("cycle polarity (+up / -down / 0 / none)")
                                        .clicked()
                                    {
                                        m.polarity = match m.polarity {
                                            None => Some(1),
                                            Some(1) => Some(-1),
                                            Some(-1) => Some(0),
                                            _ => None,
                                        };
                                    }
                                    ui.label(
                                        m.nslc
                                            .as_ref()
                                            .map(|(n, s, l, c)| format!("{n}.{s}.{l}.{c}"))
                                            .unwrap_or_default(),
                                    );
                                    if ui.small_button("⌖").on_hover_text("center view").clicked()
                                    {
                                        center_t = Some(0.5 * (m.tmin + m.tmax));
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                });
            if let Some(t) = center_t {
                let w = (self.state.tmax - self.state.tmin).max(1.0);
                self.state.tmin = t - w / 2.0;
                self.state.tmax = t + w / 2.0;
            }
            self.state.show_marker_editor = open;
        }

        // Apply a snuffling result (markers + text) exactly once per run.
        let sg = self.bridge.snuffling_gen();
        if sg != self.last_snuffling_gen {
            self.last_snuffling_gen = sg;
            if let Some(r) = self.bridge.snuffling() {
                let (mut ev_min, mut ev_max, mut any_ev) =
                    (f64::INFINITY, f64::NEG_INFINITY, false);
                for m in &r.markers {
                    let mut mk = Marker::new(m.t, m.tmax.unwrap_or(m.t), m.kind);
                    mk.phase.clone_from(&m.phase);
                    mk.is_event = m.is_event;
                    mk.nslc.clone_from(&m.nslc);
                    mk.polarity = m.polarity;
                    if m.is_event {
                        any_ev = true;
                        ev_min = ev_min.min(m.t);
                        ev_max = ev_max.max(m.t);
                    }
                    self.state.markers.push(mk);
                }
                self.state.snuffling_text = r.text;
                self.state.show_snuffling = true;
                // Catalog events are at absolute times; if they fell outside the
                // current window, jump the view so the markers are visible.
                if any_ev
                    && ev_max >= ev_min
                    && (ev_max < self.state.tmin || ev_min > self.state.tmax)
                {
                    let pad = ((ev_max - ev_min) * 0.1).max(120.0);
                    self.state.tmin = ev_min - pad;
                    self.state.tmax = ev_max + pad;
                }
            }
        }
        if self.state.show_snuffling {
            let mut open = true;
            let text = self.state.snuffling_text.clone();
            egui::Window::new("Snuffling output")
                .open(&mut open)
                .default_size([480.0, 260.0])
                .show(&ctx, |ui| {
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.monospace(text.as_str());
                    });
                });
            self.state.show_snuffling = open;
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}

fn markers_to_json(markers: &[Marker]) -> String {
    let arr: Vec<_> = markers
        .iter()
        .map(|m| {
            serde_json::json!({
                "tmin": m.tmin, "tmax": m.tmax, "kind": m.kind,
                "phase": m.phase, "nslc": m.nslc, "polarity": m.polarity,
            })
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

fn help_text(ui: &mut egui::Ui) {
    egui::Grid::new("help_grid")
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for (k, v) in [
                ("Wheel", "zoom the time axis"),
                ("Shift + Wheel", "scroll through tracks"),
                ("Ctrl + Wheel", "adjust gain"),
                ("Drag", "pan the time axis"),
                ("Shift + Drag", "place a span marker"),
                ("Double-click", "place a point marker"),
                ("0 – 7", "set marker colour"),
                ("F1 – F10", "assign a phase name"),
                ("Up / Down", "phase polarity up / down (Shift = undecided)"),
                ("Left / Right", "nudge selected marker (Shift = ×10)"),
                ("Backspace", "delete selected marker"),
                ("Space / B", "scroll time forward / back"),
                ("n / p", "select next / previous marker"),
                ("N / P", "select next / previous event"),
                ("Tab / Shift+Tab", "cycle marker selection"),
                ("a / A", "select markers in view / all"),
                ("d / Esc", "deselect all"),
                ("g / G", "go to selection / zoom tight"),
                ("m / c", "toggle marker editor / controls"),
            ] {
                ui.strong(k);
                ui.label(v);
                ui.end_row();
            }
        });
}

fn about_text(ui: &mut egui::Ui) {
    ui.heading("snuffler-web");
    ui.label(
        "pyrocko's snuffler seismic waveform inspector, running in the browser.\n\n\
         This egui frontend drives real pyrocko + numpy + scipy compiled to \
         WebAssembly via Pyodide — client-side, no server.",
    );
    ui.add_space(6.0);
    ui.hyperlink("https://pyrocko.org");
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "screen-space plotting math: f64 frequencies map to f32 pixel coordinates and sample indices to f64 fractions; precision is ample for on-screen rendering"
)]
fn draw_spectrum(ui: &mut egui::Ui, spec: Option<&SpectrumData>) {
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
        let col = MARKER_COLORS
            .get(ti % 8)
            .copied()
            .unwrap_or(Color32::from_rgb(204, 0, 0));
        let end = (t.offset + t.count).min(spec.ys.len());
        let Some(data) = spec.ys.get(t.offset.min(spec.ys.len())..end) else {
            continue;
        };
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
