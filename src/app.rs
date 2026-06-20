use egui::{Color32, FontId};

use crate::bridge::{
    Bridge, CatalogRequest, MarkerFile, MarkerFileEntry, Nslc, SnufflingRequestParams,
    StationRequest, Status, ViewParams, WaveformDownloadRequest,
};
use crate::snufflings::Action as SnufflingAction;
use crate::state::{
    AppState, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength, WiggleTheme,
};
use crate::view::markers::{MARKER_COLORS, Marker};
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};

/// Supersampling factor for the resvg-backed PNG export: the output is this many
/// times the on-screen point size per axis, giving a crisp print/retina image.
#[cfg(feature = "svg-raster")]
const PNG_EXPORT_SCALE: f32 = 2.0;

/// The egui application: UI state plus the bridge to pyrocko (via Pyodide).
pub struct App {
    state: AppState,
    bridge: Bridge,
    /// Scene generation we last fit the view window to (resets on new data).
    last_scene_gen: u64,
    wf_cache: WaterfallCache,
    /// Params the spectrum snuffling was last requested for (refetch gate).
    spectrum_key: Option<SnufflingRequestParams>,
    /// Params the Cake "Plot Model" / "Plot Rays" windows were last fetched for.
    cake_model_key: Option<SnufflingRequestParams>,
    cake_rays_key: Option<SnufflingRequestParams>,
    /// Snuffling-result generation we last applied (markers/text once each).
    last_snuffling_gen: u64,
    /// Scene time bounds last used for auto-fit; derived traces keep the same bounds.
    last_scene_bounds: Option<(f64, f64)>,
    map: crate::view::map::MapState,
    /// Waveform central-panel rect from the current frame; the crop for PNG export.
    last_waveform_rect: egui::Rect,
    /// A "Save image (PNG)" menu click, handled after layout once the rect is known.
    screenshot_request: bool,
    /// True between requesting a screenshot and eframe delivering the frame.
    screenshot_awaiting: bool,
    /// The waveform rect captured at request time, used to crop the screenshot.
    screenshot_crop: egui::Rect,
    /// A "Save image (SVG)" menu click, handled after layout once the view is known.
    svg_request: bool,
}

fn configure_look(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
    ctx.set_theme(egui::ThemePreference::System);

    // egui's bundled proportional face is Ubuntu-Light, which renders very thin.
    // Register Ubuntu Regular as the primary proportional font (same typeface, one
    // weight up) and keep the built-in fonts as fallbacks for glyph coverage.
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "ubuntu_regular".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Ubuntu-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(Proportional)
        .or_default()
        .insert(0, "ubuntu_regular".to_owned());
    ctx.set_fonts(fonts);

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
    /// Build the app, configure egui styling, and kick off loading the example.
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
            cake_model_key: None,
            cake_rays_key: None,
            last_snuffling_gen: 0,
            last_scene_bounds: None,
            map: crate::view::map::MapState::default(),
            last_waveform_rect: egui::Rect::ZERO,
            screenshot_request: false,
            screenshot_awaiting: false,
            screenshot_crop: egui::Rect::ZERO,
            svg_request: false,
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
            track_offset: s.track_offset,
            track_count: s.ntracks_shown,
        }
    }

    fn current_snuffling_params(&self, npx: u32) -> SnufflingRequestParams {
        SnufflingRequestParams {
            view: self.current_params(npx),
            snufflings: self.state.snufflings.params(),
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
                    .save_markers(ctx, markers_to_file(&self.state.markers));
                ui.close();
            }
            if ui.button("Load markers…").clicked() {
                self.bridge.load_markers(ctx);
                ui.close();
            }
            ui.separator();
            if ui.button("Save image (PNG)…").clicked() {
                self.screenshot_request = true;
                ui.close();
            }
            if ui.button("Save image (SVG)…").clicked() {
                self.svg_request = true;
                ui.close();
            }
            ui.separator();
            ui.label("Drag & drop SAC / MiniSEED files to load");
        });
    }

    fn data_menu(&mut self, ui: &mut egui::Ui) {
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
                .button("Cake Phase (theoretical arrivals)…")
                .on_hover_text(
                    "Open the Cake Phase snuffling: theoretical phase arrivals, a \
                     model-velocity plot and ray paths (pyrocko.cake).",
                )
                .clicked()
            {
                self.state.snufflings.open_cake_panel();
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

            ui.label("Wiggle theme");
            for theme in WiggleTheme::ALL {
                ui.selectable_value(&mut self.state.wiggle_theme, theme, theme.label());
            }
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
        clippy::cast_precision_loss,
        reason = "pixel sizes are small non-negative values; truncation/precision loss is intended"
    )]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Fit the view window whenever a genuinely new scene is loaded.
        let scene_gen = self.bridge.scene_gen();
        if scene_gen != self.last_scene_gen
            && let Some(scene) = self.bridge.scene()
            && !scene.tracks.is_empty()
        {
            let bounds = (scene.tmin, scene.tmax);
            let range_changed = self.last_scene_bounds.is_none_or(|old| {
                (old.0 - bounds.0).abs() > 1e-6 || (old.1 - bounds.1).abs() > 1e-6
            });
            if range_changed {
                self.state.tmin = scene.tmin;
                self.state.tmax = scene.tmax;
                self.state.ntracks_shown = scene
                    .tracks
                    .len()
                    .clamp(1, crate::state::DEFAULT_NTRACKS_SHOWN);
                self.state.track_offset = 0;
            } else {
                self.state.track_offset = self
                    .state
                    .track_offset
                    .min(scene.tracks.len().saturating_sub(1));
            }
            self.last_scene_bounds = Some(bounds);
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

        let scene = self.bridge.scene();
        let total_tracks = scene.as_ref().map_or(0, |s| s.tracks.len());
        let scene_range = scene.as_ref().map(|s| (s.tmin, s.tmax));

        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                self.file_menu(ui, &ctx);
                self.data_menu(ui);
                self.view_menu(ui);
                self.options_menu(ui);
                self.scaling_menu(ui);
                self.sorting_menu(ui);
                crate::snufflings::menu(ui, &mut self.state.snufflings, &mut self.state.view_mode);
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

        let central = egui::CentralPanel::default()
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
                    scene_range,
                );
            });
        self.last_waveform_rect = central.response.rect;

        // SVG export: re-render the current view through the shared DrawTarget to a
        // vector file at the on-screen size (no framebuffer round-trip needed).
        if std::mem::take(&mut self.svg_request) {
            let dark = ctx.global_style().visuals.dark_mode;
            let size = self.last_waveform_rect.size();
            let svg = self
                .bridge
                .view()
                .map(|view| waveform::render_svg(&ctx, &self.state, &view, size, dark));
            if let Some(svg) = svg {
                self.bridge.save_image_svg(svg, "snuffler.svg".to_owned());
            }
        }

        // PNG export. With `svg-raster` (default) we rasterize the same vector SVG
        // at high DPI via resvg. Otherwise we capture the egui framebuffer at screen
        // resolution (delivered a couple frames later as Event::Screenshot).
        if std::mem::take(&mut self.screenshot_request) {
            #[cfg(feature = "svg-raster")]
            {
                let dark = ctx.global_style().visuals.dark_mode;
                let size = self.last_waveform_rect.size();
                let png = self.bridge.view().and_then(|view| {
                    let svg = waveform::render_svg(&ctx, &self.state, &view, size, dark);
                    crate::view::raster::rasterize_svg(&svg, size.x, size.y, PNG_EXPORT_SCALE)
                });
                if let Some(png) = png {
                    self.bridge.save_image_png(png, "snuffler.png".to_owned());
                }
            }
            #[cfg(not(feature = "svg-raster"))]
            {
                self.screenshot_crop = self.last_waveform_rect;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_awaiting = true;
            }
        }
        if self.screenshot_awaiting {
            let shot = ctx.input(|i| {
                i.events.iter().rev().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = shot {
                self.screenshot_awaiting = false;
                let ppp = ctx.pixels_per_point();
                let bounds = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(image.size[0] as f32 / ppp, image.size[1] as f32 / ppp),
                );
                let crop = self.screenshot_crop.intersect(bounds);
                if crop.is_positive()
                    && let Some(png) =
                        crate::view::draw::encode_png(&image.region(&crop, Some(ppp)))
                {
                    self.bridge.save_image_png(png, "snuffler.png".to_owned());
                }
            } else {
                // Keep frames coming until the screenshot reply lands.
                ctx.request_repaint();
            }
        }

        // A custom earth model just picked via Cake Phase "Add Model".
        if let Some(name) = self.bridge.take_pending_cake_model() {
            self.state.snufflings.add_cake_model(name);
        }

        if let Some(action) = crate::snufflings::show_panel(
            &ctx,
            &mut self.state.snufflings,
            self.state.highpass,
            self.state.lowpass,
        ) {
            let mut params = self.current_snuffling_params(800);
            match action {
                SnufflingAction::Spectrum => {
                    self.state.snufflings.open_spectrum_plot();
                    self.spectrum_key = None;
                }
                SnufflingAction::Run(name) => {
                    if name == "cake" {
                        "run".clone_into(&mut params.snufflings.cake.cake_mode);
                    }
                    self.bridge.run_snuffling(&ctx, name.into(), params);
                }
                SnufflingAction::Rms => self.bridge.rms_traces(&ctx, params),
                SnufflingAction::CakePlotModel => {
                    self.state.snufflings.open_cake_model_plot();
                    self.cake_model_key = None;
                }
                SnufflingAction::CakePlotRays => {
                    self.state.snufflings.open_cake_rays_plot();
                    self.cake_rays_key = None;
                }
                SnufflingAction::AddCakeModel => self.bridge.add_cake_model(&ctx),
                #[cfg(feature = "gf")]
                SnufflingAction::AddGfStore => self.bridge.add_gf_store(&ctx),
            }
        }

        // Amplitude-spectrum snuffling window.
        if self.state.snufflings.show_spectrum_plot() {
            let params = self.current_snuffling_params(800);
            if self.spectrum_key.as_ref() != Some(&params) {
                self.bridge.fetch_spectrum(&ctx, params.clone());
                self.spectrum_key = Some(params);
            }
            let spectrum = self.bridge.spectrum();
            let mut open = true;
            egui::Window::new("Amplitude spectrum")
                .open(&mut open)
                .default_size([520.0, 280.0])
                .show(&ctx, |ui| {
                    crate::snufflings::draw_spectrum_plot(ui, spectrum.as_ref());
                });
            if !open {
                self.state.snufflings.close_spectrum_plot();
                self.spectrum_key = None;
            }
        }

        // Cake Phase "Plot Model" window (P/S velocity vs depth).
        if self.state.snufflings.show_cake_model_plot() {
            let mut params = self.current_snuffling_params(800);
            "plot_model".clone_into(&mut params.snufflings.cake.cake_mode);
            if self.cake_model_key.as_ref() != Some(&params) {
                self.bridge.cake_plot(&ctx, params.clone());
                self.cake_model_key = Some(params);
            }
            let plot = self.bridge.cake_model_plot();
            let mut open = true;
            egui::Window::new("Cake model")
                .open(&mut open)
                .default_size([560.0, 460.0])
                .show(&ctx, |ui| {
                    crate::view::cakeplot::draw_model_plot(ui, plot.as_ref());
                });
            if !open {
                self.state.snufflings.close_cake_model_plot();
                self.cake_model_key = None;
            }
        }

        // Cake Phase "Plot Rays" window (ray paths through the model).
        if self.state.snufflings.show_cake_rays_plot() {
            let mut params = self.current_snuffling_params(800);
            "plot_rays".clone_into(&mut params.snufflings.cake.cake_mode);
            if self.cake_rays_key.as_ref() != Some(&params) {
                self.bridge.cake_plot(&ctx, params.clone());
                self.cake_rays_key = Some(params);
            }
            let plot = self.bridge.cake_rays_plot();
            let mut open = true;
            egui::Window::new("Cake ray paths")
                .open(&mut open)
                .default_size([620.0, 460.0])
                .show(&ctx, |ui| {
                    crate::view::cakeplot::draw_rays_plot(ui, plot.as_ref());
                });
            if !open {
                self.state.snufflings.close_cake_rays_plot();
                self.cake_rays_key = None;
            }
        }

        if self.state.show_help {
            let mut open = true;
            egui::Window::new("Snuffler controls")
                .open(&mut open)
                .default_size([560.0, 640.0])
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
                self.bridge.download_waveforms(
                    &ctx,
                    WaveformDownloadRequest {
                        datacenter: self.state.dl_datacenter.clone(),
                        net: self.state.dl_net.clone(),
                        sta: self.state.dl_sta.clone(),
                        loc: self.state.dl_loc.clone(),
                        cha: self.state.dl_cha.clone(),
                        start: self.state.dl_start.clone(),
                        end: self.state.dl_end.clone(),
                    },
                );
            }
            if submit_st || submit_resp {
                let p = StationRequest {
                    datacenter: self.state.dl_datacenter.clone(),
                    net: self.state.dl_net.clone(),
                    sta: self.state.dl_sta.clone(),
                    cha: self.state.dl_cha.clone(),
                };
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
                self.bridge.catalog_search(
                    &ctx,
                    CatalogRequest {
                        datacenter: self.state.cat_datacenter.clone(),
                        start: self.state.cat_start.clone(),
                        end: self.state.cat_end.clone(),
                        minmag: self.state.cat_minmag,
                    },
                );
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
                self.state.show_snuffling = r.show_window && !self.state.snuffling_text.is_empty();
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

fn markers_to_file(markers: &[Marker]) -> MarkerFile {
    MarkerFile {
        markers: markers
            .iter()
            .map(|m| MarkerFileEntry {
                tmin: m.tmin,
                tmax: m.tmax,
                kind: u32::from(m.kind),
                phase: m.phase.clone(),
                nslc: m.nslc.as_ref().map(|(net, sta, loc, cha)| Nslc {
                    net: net.clone(),
                    sta: sta.clone(),
                    loc: loc.clone(),
                    cha: cha.clone(),
                }),
                polarity: m.polarity.map(i32::from),
            })
            .collect(),
    }
}

fn help_text(ui: &mut egui::Ui) {
    const KEY_ROWS: &[(&str, &str)] = &[
        ("q", "Quit / close viewer"),
        ("r", "Reload modified files (native only)"),
        ("R", "Reload snufflings (native only)"),
        ("f", "Toggle full screen mode"),
        ("m", "Toggle marker sidebar"),
        ("c", "Toggle main controls"),
        (":", "Enter command"),
        ("<space>", "Forward one page in time"),
        ("b", "Backward one page in time"),
        ("<pagedown>", "Scroll tracks one page down"),
        ("<pageup>", "Scroll tracks one page up"),
        ("+", "Show one track more"),
        ("-", "Show one track less"),
        ("=", "Show initial number of tracks"),
        ("g", "Go to selection / show all"),
        ("G", "Zoom to selection / zoom to trace visibility"),
        ("n", "Go to next marker"),
        ("p", "Go to previous marker"),
        ("N", "Go to next event marker"),
        ("P", "Go to previous event marker"),
        ("<tab>", "Go to next marker of active event"),
        ("<shift> + <tab>", "Go to previous marker of active event"),
        ("a", "Select all markers currently visible"),
        ("A", "Select all markers"),
        ("d", "Deselect all markers"),
        ("0 ... 7", "Change color of marker"),
        ("<f1> ... <f9>", "Convert to phase marker"),
        ("e", "Convert to event marker / set active event"),
        ("E", "Deactivate event"),
        ("<f10>", "Convert phase marker to normal marker"),
        ("<backspace>", "Delete marker"),
        (
            "<up>, <down>",
            "Set first motion polarity on selected marker",
        ),
        (
            "<shift> + <up>, <shift> + <down>",
            "Unset first motion polarity on selected marker",
        ),
        (
            "<escape>",
            "Enter batch-picking / abort picking (native only)",
        ),
        ("?", "Help"),
    ];
    const MOUSE_ROWS: &[(&str, &str)] = &[
        ("Click and drag", "Zoom and pan"),
        ("Click and drag on time axis", "Pan only"),
        ("Click on marker", "Select marker"),
        ("<shift> + click on marker", "Select additional marker"),
        ("Wheel", "Scroll tracks vertically"),
        ("<ctrl> + wheel", "Change number of tracks shown"),
        ("Right-click", "Menu (native only)"),
        ("Double-click", "Enter picking mode / place pick"),
        (
            "Shift and left-click",
            "Uncertainty selection for current phase marker (native only)",
        ),
    ];
    const COMMAND_ROWS: &[(&str, &str)] = &[
        (
            "n|s|l|c [ <pattern> ]",
            "Reduce traces to those matching a given pattern.",
        ),
        (
            "hide <pattern>\nhide n|s|l|c <pattern>",
            "Hide traces whose network, station, location, and channel IDs match a pattern.",
        ),
        (
            "unhide <pattern>\nunhide n|s|l|c <pattern>",
            "Unhide traces previously hidden with the hide command.",
        ),
        (
            "goto YYYY-MM[-DD[ HH[:MM[:SS[.XXX]]]]]\ngoto HH:MM[:SS[.XXX]]",
            "Jump to a given time.",
        ),
        ("markers ...", "Toggle marker visibility (native only)."),
        ("scaling ...", "Set scaling rules (native only)."),
    ];

    egui::ScrollArea::vertical().show(ui, |ui| {
        help_section(ui, "Key", KEY_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Mouse", MOUSE_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Command", COMMAND_ROWS, true);
    });
}

fn help_section(ui: &mut egui::Ui, title: &str, rows: &[(&str, &str)], monospace_key: bool) {
    ui.heading(title);
    ui.add_space(6.0);
    egui::Grid::new(("help_section", title))
        .num_columns(2)
        .spacing([18.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (key, description) in rows {
                let key_text = egui::RichText::new(*key);
                let key_text = if monospace_key {
                    key_text.monospace().strong()
                } else {
                    key_text.italics().strong()
                };
                // Right-align the shortcut within its column so it sits right
                // next to the description rather than far across a wide column.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(key_text);
                });
                ui.label(*description);
                ui.end_row();
            }
        });
    ui.add_space(4.0);
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
