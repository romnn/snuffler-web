use egui::FontId;

use crate::snufflings::Action as SnufflingAction;
use crate::view::marker_table::markers_to_file;
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};
use snuffler_web_core::bridge::{ExampleDataset, Repaint, Status};
use snuffler_web_core::markers::Marker;
use snuffler_web_core::session::Session;
use snuffler_web_core::state::{
    AppState, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength, WiggleTheme,
};

/// How often the displayed frame rate is allowed to change. The measurement is
/// smoothed every frame; only the number on screen is held still, because one
/// that rewrites itself at 60 Hz is unreadable.
const FRAME_STATS_REFRESH_INTERVAL_S: f64 = 0.25;
const FRAME_STATS_EMA_ALPHA: f64 = 0.2;
/// The widest the readout can get (the rate is capped at 60), used to reserve a
/// width that does not change as the number does.
const FRAME_STATS_WIDEST: &str = "60 fps";

/// How much of the right-hand readout the menu row can afford this frame.
struct RightReadout {
    /// Width to keep clear at the right of the row; zero shows nothing at all.
    width: f32,
    /// Whether the build stamp fits beside the frame rate.
    with_stamp: bool,
}

/// Supersampling factor for the resvg-backed PNG export: the output is this many
/// times the on-screen point size per axis, giving a crisp print/retina image.
#[cfg(feature = "svg-raster")]
const PNG_EXPORT_SCALE: f32 = 2.0;

// `npx` is a per-trace bin budget for Python's min/max envelope. The display-scale
// bin budget keeps transport and renderer work bounded when zoomed far out.
const MIN_VIEW_SAMPLE_BINS: f32 = 64.0;
const MAX_VIEW_SAMPLE_BINS_PER_TRACE: f32 = 8192.0;
const WIGGLE_BINS_PER_PHYSICAL_PIXEL: f32 = 1.0;
const WATERFALL_BINS_PER_PHYSICAL_PIXEL: f32 = 1.0;
const MAX_VISIBLE_WIGGLE_POINTS: f32 = 16_000.0;
const MAX_VISIBLE_WATERFALL_CELLS: f32 = 400_000.0;
const WIGGLE_POINTS_PER_BIN: f32 = 4.0;
const WATERFALL_CELLS_PER_BIN: f32 = 2.0;

/// The egui application: UI state plus the bridge to pyrocko (via Pyodide).
pub struct App {
    session: Session,
    snuffling_panels: crate::snufflings::SnufflingPanels,
    wf_cache: WaterfallCache,
    figures: crate::view::figure::FigureWindows,
    marker_table: crate::view::marker_table::MarkerTable,
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
    /// In-flight off-thread PNG export: receives the encoded bytes from the rayon
    /// pool, paired with the download filename. Only the threaded build runs the
    /// export off-thread; otherwise it rasterizes synchronously (no receiver).
    #[cfg(all(feature = "svg-raster", feature = "threads"))]
    export_rx: Option<(std::sync::mpsc::Receiver<Option<Vec<u8>>>, String)>,
    /// Smoothed egui CPU time per frame, in milliseconds.
    frame_stats_ms: Option<f64>,
    /// The frame rate the top bar is currently showing, and when it last changed.
    frame_stats_shown_fps: Option<u32>,
    frame_stats_shown_at_s: f64,
    /// The light/dark preference to restore when leaving a theme that pins the
    /// canvas. Snuffler Qt takes the *whole* frontend light, not just the waveform:
    /// a white canvas inside dark chrome is neither theme, it just looks broken.
    theme_before_pinning: Option<egui::ThemePreference>,
}

fn configure_look(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
    // The default wiggle theme decides the boot preference: Snuffler Qt pins the whole
    // frontend light, so following the system into dark here would show white
    // waveforms in dark chrome for the first frames and then snap.
    ctx.set_theme(if AppState::default().wiggle_theme.forces_light() {
        egui::ThemePreference::Light
    } else {
        egui::ThemePreference::System
    });

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
    // egui's bundled monospace face is Hack, which carries no arrows — and the ':'
    // command line spells its history keys as ↑/↓, which would draw as tofu.
    // JetBrains Mono covers the arrows (and ± for marker polarities).
    fonts.font_data.insert(
        "jetbrains_mono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/JetBrainsMono-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(Monospace)
        .or_default()
        .insert(0, "jetbrains_mono".to_owned());
    // Also the last proportional fallback: Ubuntu has no arrows either, so this is
    // what keeps a ↑ from turning into tofu in any label that happens to use one.
    fonts
        .families
        .entry(Proportional)
        .or_default()
        .push("jetbrains_mono".to_owned());
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
        crate::a11y::install(&cc.egui_ctx);
        let repaint_now = cc.egui_ctx.clone();
        let repaint_after = cc.egui_ctx.clone();
        let session = Session::new(Repaint::new(
            move || repaint_now.request_repaint(),
            move |delay| repaint_after.request_repaint_after(delay),
        ));
        // A `?example=<slug>` link is what the app was opened *for*, so it takes the
        // place of the synthetic demo rather than replacing it a second later. Both
        // are issued before the worker has booted; the transport queues them.
        match crate::deep_link::requested_example() {
            Some(slug) => session.bridge.open_example(slug),
            None => session.bridge.load_example(),
        }
        Self {
            session,
            snuffling_panels: crate::snufflings::SnufflingPanels::default(),
            wf_cache: WaterfallCache::default(),
            figures: crate::view::figure::FigureWindows::default(),
            marker_table: crate::view::marker_table::MarkerTable::default(),
            map: crate::view::map::MapState::default(),
            last_waveform_rect: egui::Rect::ZERO,
            screenshot_request: false,
            screenshot_awaiting: false,
            screenshot_crop: egui::Rect::ZERO,
            svg_request: false,
            #[cfg(all(feature = "svg-raster", feature = "threads"))]
            export_rx: None,
            frame_stats_ms: None,
            frame_stats_shown_fps: None,
            frame_stats_shown_at_s: 0.0,
            theme_before_pinning: None,
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rounded rate is clamped to 0..=60 before the cast"
    )]
    fn update_frame_stats(&mut self, ui: &egui::Ui, frame: &eframe::Frame) {
        let Some(cpu_usage) = frame.info().cpu_usage else {
            return;
        };
        let frame_ms = f64::from(cpu_usage) * 1000.0;
        if !frame_ms.is_finite() || frame_ms < 0.0 {
            return;
        }
        let avg_ms = match self.frame_stats_ms {
            Some(old) => old * (1.0 - FRAME_STATS_EMA_ALPHA) + frame_ms * FRAME_STATS_EMA_ALPHA,
            None => frame_ms,
        };
        self.frame_stats_ms = Some(avg_ms);

        let now_s = ui.input(|i| i.time);
        if self.frame_stats_shown_fps.is_some()
            && now_s - self.frame_stats_shown_at_s < FRAME_STATS_REFRESH_INTERVAL_S
        {
            return;
        }
        self.frame_stats_shown_at_s = now_s;
        // egui repaints on demand, so elapsed wall time between frames measures
        // idleness, not capacity. What the CPU time per frame says is how many
        // frames per second this machine could sustain — capped at the 60 Hz the
        // display would grant it anyway.
        let fps = if avg_ms > 0.0 { 1000.0 / avg_ms } else { 60.0 };
        self.frame_stats_shown_fps = Some(fps.clamp(0.0, 60.0).round() as u32);
    }

    fn clear_annotation_caches(&mut self) {
        self.marker_table = crate::view::marker_table::MarkerTable::default();
        self.figures.clear_beachball();
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "display dimensions and track counts are small positive UI values; the rounded integer is a rendering budget"
    )]
    fn view_sample_bins(&self, ui: &egui::Ui) -> u32 {
        let points_per_pixel = ui.ctx().pixels_per_point();
        let points_per_pixel = if points_per_pixel.is_finite() && points_per_pixel > 0.0 {
            points_per_pixel
        } else {
            1.0
        };
        let visible_tracks = self.session.state.ntracks_shown.max(1) as f32;
        let physical_width = ui.available_width().max(MIN_VIEW_SAMPLE_BINS) * points_per_pixel;
        let (bins_per_pixel, emitted_per_bin, visible_cap) = match self.session.state.view_mode {
            ViewMode::Wiggle => (
                WIGGLE_BINS_PER_PHYSICAL_PIXEL,
                WIGGLE_POINTS_PER_BIN,
                MAX_VISIBLE_WIGGLE_POINTS,
            ),
            ViewMode::Waterfall => (
                WATERFALL_BINS_PER_PHYSICAL_PIXEL,
                WATERFALL_CELLS_PER_BIN,
                MAX_VISIBLE_WATERFALL_CELLS,
            ),
        };
        let per_trace_visible_cap = visible_cap / (visible_tracks * emitted_per_bin);
        let bins = (physical_width * bins_per_pixel)
            .min(MAX_VIEW_SAMPLE_BINS_PER_TRACE)
            .min(per_trace_visible_cap.max(MIN_VIEW_SAMPLE_BINS))
            .max(MIN_VIEW_SAMPLE_BINS);
        bins.ceil() as u32
    }

    /// Start a high-DPI PNG export of the current waveform view. The vector SVG is
    /// built here (it needs the egui `Context`), then rasterized: on the rayon
    /// worker pool when available — so a big export never freezes the UI — and
    /// otherwise synchronously on the main thread when the worker pool is not
    /// available. See `AGENTS.md`: never block the egui render loop.
    #[cfg(feature = "svg-raster")]
    fn start_png_export(&mut self, ctx: &egui::Context) {
        let theme = ctx.theme();
        let size = self.last_waveform_rect.size();
        // Scoped so the `Ref` into the bridge ends the moment the SVG exists: what
        // follows either rasterizes inline for as long as that takes, or hands the
        // work to the rayon pool and returns while it runs. Either way pyrocko
        // results keep landing and taking `borrow_mut()` on the same `RefCell`, so
        // holding a read guard across that point is a panic waiting for the timing.
        let svg = {
            let Some(view) = self.session.bridge.view() else {
                return;
            };
            waveform::render_svg(ctx, &self.session.state, &view, size, theme)
        };

        #[cfg(feature = "threads")]
        if crate::pool::ready() {
            let (tx, rx) = std::sync::mpsc::channel();
            let (w, h) = (size.x, size.y);
            rayon::spawn(move || {
                let _ = tx.send(crate::view::raster::rasterize_svg(
                    &svg,
                    w,
                    h,
                    PNG_EXPORT_SCALE,
                ));
            });
            self.export_rx = Some((rx, "snuffler.png".to_owned()));
            return;
        }

        if let Some(png) =
            crate::view::raster::rasterize_svg(&svg, size.x, size.y, PNG_EXPORT_SCALE)
        {
            self.session
                .bridge
                .save_image_png(png, "snuffler.png".to_owned());
        }
    }

    /// Deliver a finished off-thread PNG export, or keep the progress overlay up
    /// (and frames coming) while the rayon pool is still rasterizing.
    #[cfg(all(feature = "svg-raster", feature = "threads"))]
    fn poll_png_export(&mut self, ctx: &egui::Context) {
        let status = self.export_rx.as_ref().map(|(rx, _)| rx.try_recv());
        match status {
            Some(Ok(png)) => {
                if let Some((_, filename)) = self.export_rx.take()
                    && let Some(png) = png
                {
                    self.session.bridge.save_image_png(png, filename);
                }
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                crate::job::progress_overlay(ctx, "Rendering PNG…");
                ctx.request_repaint();
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.export_rx = None;
            }
            None => {}
        }
    }

    /// The full menu set, shared by the top menubar and the canvas right-click
    /// context menu so both stay in sync (Qt pops the same menu object for both).
    fn main_menus(&mut self, ui: &mut egui::Ui) {
        self.file_menu(ui);
        self.data_menu(ui);
        self.view_menu(ui);
        self.options_menu(ui);
        self.scaling_menu(ui);
        self.sorting_menu(ui);
        crate::snufflings::menu(
            ui,
            &mut self.snuffling_panels,
            &mut self.session.state.view_mode,
        );
        self.help_menu(ui);
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            self.file_open_items(ui);
            self.file_data_items(ui);
            ui.separator();
            self.file_marker_items(ui);
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
            ui.label("Drag & drop SAC / MiniSEED files, or a .zip bundle, to load");
        });
    }

    fn file_open_items(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Open files…")
            .on_hover_text(
                "Waveform files, or a .zip bundle holding a whole dataset \
                 (waveforms + StationXML + events), loaded in one go.",
            )
            .clicked()
        {
            self.session.bridge.open_files();
            ui.close();
        }
        if ui
            .button("Open waveform directory…")
            .on_hover_text(
                "Recursively load every waveform file under a folder \
                 (File System Access; Chromium).",
            )
            .clicked()
        {
            self.session.bridge.open_directory();
            ui.close();
        }
        if ui
            .button("Reload modified files")
            .on_hover_text(
                "Re-read opened files whose contents changed on disk (key 'r'). \
                 Needs the File System Access API (Chromium).",
            )
            .clicked()
        {
            self.session.bridge.reload_files();
            ui.close();
        }
        if ui
            .button("Print…")
            .on_hover_text("Open the browser print dialog for the current view (also → PDF).")
            .clicked()
        {
            if let Some(window) = web_sys::window() {
                let _ = window.print();
            }
            ui.close();
        }
    }

    /// `File ▸ Examples`: real recorded datasets, each one fetch away.
    ///
    /// Nothing here ships in the app. The index is fetched when this submenu is
    /// first opened and a dataset's bytes only when someone picks it, so a menu
    /// full of examples costs a page load nothing — which is the point, since the
    /// people who most need example data are the ones who have none of their own.
    fn file_examples_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Examples", |ui| {
            // A submenu sizes itself to its content, and these labels are a title
            // plus a download size — without a floor they wrap to one word per
            // line in a column narrow enough to be unreadable.
            ui.set_min_width(320.0);
            self.session.bridge.list_examples(false);
            match self.session.bridge.examples() {
                None => {
                    ui.label("Loading…");
                }
                Some(Err(error)) => {
                    ui.label(format!("Unavailable: {error}"));
                    if ui.button("Retry").clicked() {
                        self.session.bridge.list_examples(true);
                    }
                }
                Some(Ok(examples)) if examples.is_empty() => {
                    ui.label("No examples in this build");
                }
                Some(Ok(examples)) => {
                    for example in examples {
                        let label = format!("{} ({})", example.title, download_size(example.bytes));
                        if ui
                            .button(label)
                            .on_hover_text(example_hover(&example))
                            .clicked()
                        {
                            self.session.bridge.load_bundle(
                                example.url.clone(),
                                example.sha256.clone(),
                                example.title.clone(),
                            );
                            // Point the address bar at what is now loaded, so
                            // sharing it is copying the URL.
                            crate::deep_link::remember_example(&example.slug);
                            ui.close();
                        }
                    }
                }
            }
        });
    }

    fn file_data_items(&mut self, ui: &mut egui::Ui) {
        self.file_examples_menu(ui);
        if ui.button("Synthetic example").clicked() {
            self.session.bridge.load_example();
            ui.close();
        }
        if ui.button("Load bundled test.mseed").clicked() {
            self.session
                .bridge
                .load_url("test.mseed".into(), "mseed".into());
            ui.close();
        }
        if ui
            .button("Open StationXML files…")
            .on_hover_text(
                "Load one or more local StationXML files: station coordinates \
                 (distance/azimuth sorting) + instrument responses \
                 (Options ▸ Deconvolution).",
            )
            .clicked()
        {
            self.session.bridge.load_station_xml();
            ui.close();
        }
        if ui
            .button("Open stations…")
            .on_hover_text("Load a local pyrocko station file (coordinates for sorting/cake/map).")
            .clicked()
        {
            self.session.bridge.load_stations();
            ui.close();
        }
        if ui
            .button("Open events…")
            .on_hover_text("Load a local pyrocko event file (event markers + active event).")
            .clicked()
        {
            self.session.bridge.load_events();
            ui.close();
        }
    }

    fn file_marker_items(&mut self, ui: &mut egui::Ui) {
        if ui.button("Save markers…").clicked() {
            self.session
                .bridge
                .save_markers(markers_to_file(&self.session.state.markers));
            ui.close();
        }
        if ui
            .add_enabled(
                self.session
                    .state
                    .markers
                    .iter()
                    .any(|marker| marker.selected),
                egui::Button::new("Save selected markers…"),
            )
            .clicked()
        {
            let selected: Vec<Marker> = self
                .session
                .state
                .markers
                .iter()
                .filter(|marker| marker.selected)
                .cloned()
                .collect();
            self.session.bridge.save_markers(markers_to_file(&selected));
            ui.close();
        }
        if ui.button("Load markers…").clicked() {
            self.session.bridge.load_markers();
            ui.close();
        }
    }

    fn data_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Data", |ui| {
            if ui.button("Download waveforms (FDSN)…").clicked() {
                self.session.state.show_download = true;
                ui.close();
            }
            if ui.button("Live SeedLink…").clicked() {
                self.session.state.show_live_seedlink = true;
                ui.close();
            }
            if ui.button("Catalog search (events)…").clicked() {
                self.session.state.show_catalog = true;
                ui.close();
            }
            if ui.button("Map (stations & events)…").clicked() {
                self.session.state.show_map = true;
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
                self.snuffling_panels.open_cake_panel();
                ui.close();
            }
            ui.separator();
            ui.label("Real data via browser networking.");
        });
    }

    /// Take the whole frontend light for a canvas-pinning theme, and give the user's
    /// own preference back when they leave it.
    fn apply_theme_pinning(&mut self, ctx: &egui::Context, theme: WiggleTheme) {
        if theme.forces_light() {
            let current = ctx.options(|o| o.theme_preference);
            if current != egui::ThemePreference::Light {
                self.theme_before_pinning = Some(current);
            }
            ctx.set_theme(egui::ThemePreference::Light);
        } else if let Some(restored) = self.theme_before_pinning.take() {
            ctx.set_theme(restored);
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            ui.label("Theme");
            // Snuffler Qt owns its canvas: snuffler is white-only, so a dark variant
            // of that theme would be a different theme wearing the name. The control
            // stays visible and says why rather than vanishing under some themes.
            let pinned = self.session.state.wiggle_theme.forces_light();
            ui.add_enabled_ui(!pinned, egui::global_theme_preference_buttons)
                .response
                .on_disabled_hover_text(
                    "Only the Snuffler Web theme has light and dark. Snuffler itself \
                     draws on white, so the Snuffler Qt theme stays there.",
                );
            ui.separator();

            ui.label("Visible length");
            for vl in VisibleLength::ALL {
                if ui
                    .selectable_value(&mut self.session.state.visible_length, vl, vl.label())
                    .clicked()
                {
                    let max = vl.seconds();
                    if self.session.state.tmax - self.session.state.tmin > max {
                        let c = f64::midpoint(self.session.state.tmin, self.session.state.tmax);
                        self.session.state.tmin = c - max / 2.0;
                        self.session.state.tmax = c + max / 2.0;
                    }
                }
            }
            ui.separator();

            ui.selectable_value(
                &mut self.session.state.view_mode,
                ViewMode::Wiggle,
                "Wiggle plot",
            );
            ui.selectable_value(
                &mut self.session.state.view_mode,
                ViewMode::Waterfall,
                "Waterfall",
            );
            ui.separator();

            ui.label("Wiggle theme");
            for theme in WiggleTheme::ALL {
                let previous = self.session.state.wiggle_theme;
                if ui
                    .selectable_value(&mut self.session.state.wiggle_theme, theme, theme.label())
                    .clicked()
                    && theme != previous
                {
                    self.apply_theme_pinning(ui.ctx(), theme);
                }
            }
            ui.separator();

            ui.checkbox(&mut self.session.state.clip_traces, "Clip traces");
            ui.checkbox(&mut self.session.state.show_boxes, "Show boxes");
            ui.checkbox(&mut self.session.state.color_traces, "Color traces");
            ui.checkbox(&mut self.session.state.antialiasing, "Antialiasing");
            ui.separator();
            ui.checkbox(
                &mut self.session.state.show_scale_ranges,
                "Show scale ranges",
            );
            ui.checkbox(&mut self.session.state.show_scale_axes, "Show scale axes");
            ui.checkbox(&mut self.session.state.show_zero_lines, "Show zero lines");
            ui.checkbox(&mut self.session.state.show_time_grid, "Show time grid");
            // Backed by an Option: the checkbox shows what the theme is actually
            // doing, and clicking it records a choice that outlives a theme switch.
            let mut separators = self.session.state.track_separators();
            if ui
                .checkbox(&mut separators, "Show track separators")
                .changed()
            {
                self.session.state.show_track_separators = Some(separators);
            }
            ui.separator();
            ui.checkbox(&mut self.session.state.show_controls, "Show main controls");
            ui.checkbox(&mut self.session.state.show_marker_editor, "Marker editor");
            if ui.button("Reset view to full extent").clicked() {
                if let Some(scene) = self.session.bridge.scene() {
                    self.session.state.tmin = scene.tmin;
                    self.session.state.tmax = scene.tmax;
                    self.session.state.gain = 1.0;
                }
                ui.close();
            }
        });
    }

    fn options_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Options", |ui| {
            ui.checkbox(&mut self.session.state.demean, "Demean");
            ui.checkbox(&mut self.session.state.remove_response, "Deconvolution")
                .on_hover_text(
                    "Deconvolve the instrument response, giving the instrument's own \
                     input unit (velocity for a broadband). Fetch responses first via \
                     Data ▸ Download ▸ Fetch responses.",
                );
            ui.checkbox(&mut self.session.state.distances_3d, "3D distances")
                .on_hover_text("Use station depth in distance sorting (needs coordinates).");
            ui.separator();
            ui.checkbox(
                &mut self.session.state.allow_downsampling,
                "Allow downsampling",
            );
            ui.checkbox(&mut self.session.state.degap, "Allow degapping");
            ui.separator();
            ui.checkbox(&mut self.session.state.fft_filtering, "FFT filtering")
                .on_hover_text("Brick-wall FFT filtering instead of Butterworth.");
            ui.checkbox(
                &mut self.session.state.bandpass_lphp,
                "Bandpass is Low- + Highpass",
            );
            ui.separator();
            ui.checkbox(&mut self.session.state.watch_files, "Watch Files")
                .on_hover_text(
                    "Automatically reload opened files as they change on disk \
                     (polls every 2 s; File System Access, Chromium).",
                );
            ui.checkbox(
                &mut self.session.state.liberal_fetch,
                "Liberal Fetch Optimization",
            )
            .on_hover_text(
                "Fetch a wider, grid-snapped time window so pan/zoom reuses \
                     already-fetched data instead of re-requesting.",
            );
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
                ui.selectable_value(&mut self.session.state.scaling_group, g, g.label());
            }
            ui.separator();
            for b in [
                ScalingBase::MinMax,
                ScalingBase::Robust,
                ScalingBase::Mean2Std,
                ScalingBase::Mean4Std,
            ] {
                ui.selectable_value(&mut self.session.state.scaling_base, b, b.label());
            }
            ui.separator();
            ui.checkbox(&mut self.session.state.fix_scale_ranges, "Fix scale ranges");
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
                let r = ui.selectable_value(&mut self.session.state.sort, m, m.label());
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
                ui.selectable_value(&mut self.session.state.subsort, ss, ss.label());
            }
        });
    }

    fn help_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Help", |ui| {
            if ui.button("Snuffler controls").clicked() {
                self.session.state.show_help = true;
                ui.close();
            }
            if ui.button("About").clicked() {
                self.session.state.show_about = true;
                ui.close();
            }
        });
    }

    /// Open a just-loaded bundle in the view it asked for.
    ///
    /// A dataset shipped as an example exists to show something, and the setting
    /// that shows it is part of the example — co-located sensors say nothing until
    /// the subsort groups them onto one row. Unknown values are ignored, so a
    /// bundle built against a later version cannot wedge this one.
    fn apply_pending_view(&mut self) {
        let Some(view) = self.session.bridge.take_pending_view() else {
            return;
        };
        if let Some(sort) = SortMode::from_wire(&view.sort) {
            self.session.state.sort = sort;
        }
        if let Some(subsort) = Subsort::from_wire(&view.subsort) {
            self.session.state.subsort = subsort;
        }
    }

    fn load_dropped_files(&mut self, ctx: &egui::Context) {
        for file in ctx.input(|input| input.raw.dropped_files.clone()) {
            let Some(bytes) = &file.bytes else {
                continue;
            };
            let name = if file.name.is_empty() {
                "dropped".to_owned()
            } else {
                file.name
            };
            let format = guess_fmt(&name);
            self.session.bridge.load_file(name, bytes.to_vec(), format);
        }
    }

    fn show_status_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, total_tracks: usize) {
        egui::Panel::top("menu").show_inside(ui, |ui| {
            // This row has to stay exactly one line tall whatever it ends up
            // holding: a wrapped label would make the panel taller and push every
            // panel below it down, and since the row grows and shrinks on its own
            // (the cursor clock is hover-only, the PICK badge comes and goes, the
            // status text is a message of any length), a height that followed its
            // contents would move the traces out from under the pointer that just
            // changed them. Truncating pins the height for any font, any message,
            // and any window width, and degrades to an ellipsis on a narrow screen
            // instead of spilling widgets past the panel edge.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.horizontal(|ui| {
                self.main_menus(ui);
                // snuffler's menu-bar ':' button, which toggles the command line.
                if ui
                    .button(egui::RichText::new(":").monospace().strong())
                    .on_hover_text("Show command line.")
                    .clicked()
                {
                    self.session.state.command.toggle();
                }
                ui.separator();
                // Hand the right edge to the build stamp and frame rate before the
                // status message is laid out: a message of any length then truncates
                // within what is left, instead of pushing the readout off the row.
                let readout = Self::right_readout(ui);
                let left = (ui.available_width() - readout.width).max(0.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(left, ui.available_height()),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| self.show_activity(ui, ctx, total_tracks),
                );
                if readout.width > 0.0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.show_frame_stats(ui);
                        if readout.with_stamp {
                            ui.separator();
                            self.show_build_stamp(ui);
                        }
                    });
                }
            });
        });
    }

    /// What the app is doing right now: the status line, the picking badge, and
    /// the time under the pointer.
    fn show_activity(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, total_tracks: usize) {
        // The status line is the one place the app tells you that something
        // finished, failed, or is still running, and none of it is tied to a
        // widget you would be focused on — so it is announced as it changes.
        let (status, urgency) = match self.session.bridge.status() {
            Status::Booting => {
                ui.spinner();
                (
                    ui.label("Starting Python (Pyodide)…"),
                    crate::a11y::Urgency::Polite,
                )
            }
            Status::Loading(message) => {
                ui.spinner();
                (ui.label(message), crate::a11y::Urgency::Polite)
            }
            Status::Ready => (
                ui.label(format!("Ready · {total_tracks} traces")),
                crate::a11y::Urgency::Polite,
            ),
            Status::Error(error) => (
                ui.colored_label(egui::Color32::RED, format!("Error: {error}")),
                crate::a11y::Urgency::Urgent,
            ),
        };
        crate::a11y::announce_changes(ctx, &status, urgency);
        if self.session.state.picking.is_some() {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(0xE0, 0x50, 0x50),
                "◉ PICK (Esc to exit)",
            )
            .on_hover_text(
                "Batch picking: click/drag on a trace to place a pick; number/F keys \
                 set kind/phase and carry to the next pick.",
            );
        }
        self.show_cursor_time(ui, ctx);
    }

    /// How much of the build stamp and frame rate the right of the menu row can
    /// take without crowding out what is to their left.
    ///
    /// Measured rather than assumed. Everything here moves — the menus are as wide
    /// as the font makes them, the stamp is a timestamp with a zone or just
    /// `dev build`, the window is any size — so a fixed threshold (what the HTML
    /// badge this replaces used, as a hardcoded `max-width` media query) is wrong
    /// for some combination of them.
    fn right_readout(ui: &egui::Ui) -> RightReadout {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let text_width = |text: String| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        };
        let gap = ui.spacing().item_spacing.x;
        // Reserved for the widest rate rather than the current one, so the status
        // beside it does not resize when the first measurement lands or the number
        // gains a digit. `6.0` is the width egui gives a vertical separator.
        let rate = text_width(FRAME_STATS_WIDEST.to_owned()) + gap * 2.0;
        let stamp = text_width(crate::view::about::build_stamp()) + 6.0 + gap * 2.0;
        // What the menus and the status keep no matter what: below it the readout
        // is the part worth dropping, and dropping it is what keeps it from
        // painting over them.
        let status_min = ui.text_style_height(&egui::TextStyle::Body) * 8.0;
        let spare = ui.available_width() - status_min;
        if spare >= rate + stamp {
            RightReadout {
                width: rate + stamp,
                with_stamp: true,
            }
        } else if spare >= rate {
            // The stamp is by far the longer of the two and never changes within a
            // build; the rate is short and is the one worth watching.
            RightReadout {
                width: rate,
                with_stamp: false,
            }
        } else {
            RightReadout {
                width: 0.0,
                with_stamp: false,
            }
        }
    }

    /// When the published build was built, and the way into the About window it
    /// is a one-line summary of.
    fn show_build_stamp(&mut self, ui: &mut egui::Ui) {
        let stamp = egui::RichText::new(crate::view::about::build_stamp())
            .monospace()
            .weak();
        if ui
            .add(egui::Label::new(stamp).sense(egui::Sense::click()))
            .on_hover_text(crate::view::about::build_tooltip())
            .clicked()
        {
            self.session.state.show_about = true;
        }
    }

    fn show_frame_stats(&self, ui: &mut egui::Ui) {
        let Some(fps) = self.frame_stats_shown_fps else {
            return;
        };
        ui.label(egui::RichText::new(format!("{fps} fps")).monospace().weak())
            .on_hover_text(
                "Frames per second this machine could sustain, from egui's CPU time \
                 per frame. Capped at 60; the view only redraws when something changes.",
            );
    }

    fn show_cursor_time(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(position) = ctx.pointer_hover_pos() else {
            return;
        };
        // The whole rect, not just its horizontal span: the controls sit directly
        // below the traces and share their x range, so testing x alone made the
        // clock blink in and out while the pointer was on a slider down there.
        let rect = self.last_waveform_rect;
        if !rect.is_positive() || !rect.contains(position) {
            return;
        }
        let fraction = f64::from(((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        let time = self.session.state.tmin
            + fraction * (self.session.state.tmax - self.session.state.tmin);
        ui.separator();
        ui.monospace(snuffler_web_core::timeaxis::format_full(time));
    }

    fn show_command_panel(&mut self, ui: &mut egui::Ui) {
        if crate::view::command::show(ui, &mut self.session.state) {
            self.session.submit_command();
        }
    }

    fn show_waveform_panel(
        &mut self,
        ui: &mut egui::Ui,
        total_tracks: usize,
        scene_range: Option<(f64, f64)>,
    ) {
        if self.session.state.show_controls {
            egui::Panel::bottom("controls").show_inside(ui, |ui| {
                controls::show(ui, &mut self.session.state, total_tracks.max(1));
            });
        }
        let central = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                let epoch = self.session.bridge.view_epoch();
                let view = self.session.bridge.view();
                waveform::show(
                    ui,
                    &mut self.session.state,
                    view.as_deref(),
                    &mut self.wf_cache,
                    epoch,
                    scene_range,
                )
            });
        self.last_waveform_rect = central.response.rect;
        central.inner.context_menu(|ui| self.main_menus(ui));
    }

    #[cfg_attr(
        not(feature = "svg-raster"),
        allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "the framebuffer fallback converts bounded browser pixel sizes only when SVG rasterization is disabled"
        )
    )]
    fn handle_export_requests(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.svg_request) {
            let theme = ctx.theme();
            let size = self.last_waveform_rect.size();
            let svg = self
                .session
                .bridge
                .view()
                .map(|view| waveform::render_svg(ctx, &self.session.state, &view, size, theme));
            if let Some(svg) = svg {
                self.session
                    .bridge
                    .save_image_svg(svg, "snuffler.svg".to_owned());
            }
        }

        if std::mem::take(&mut self.screenshot_request) {
            #[cfg(feature = "svg-raster")]
            self.start_png_export(ctx);
            #[cfg(not(feature = "svg-raster"))]
            {
                self.screenshot_crop = self.last_waveform_rect;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_awaiting = true;
            }
        }
        #[cfg(all(feature = "svg-raster", feature = "threads"))]
        self.poll_png_export(ctx);
        self.deliver_screenshot(ctx);
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "screenshot dimensions are bounded browser pixel sizes represented as points"
    )]
    fn deliver_screenshot(&mut self, ctx: &egui::Context) {
        if !self.screenshot_awaiting {
            return;
        }
        let screenshot = ctx.input(|input| {
            input.events.iter().rev().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = screenshot else {
            ctx.request_repaint();
            return;
        };

        self.screenshot_awaiting = false;
        let points_per_pixel = ctx.pixels_per_point();
        let bounds = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(
                image.size[0] as f32 / points_per_pixel,
                image.size[1] as f32 / points_per_pixel,
            ),
        );
        let crop = self.screenshot_crop.intersect(bounds);
        if crop.is_positive()
            && let Some(png) =
                crate::view::draw::encode_png(&image.region(&crop, Some(points_per_pixel)))
        {
            self.session
                .bridge
                .save_image_png(png, "snuffler.png".to_owned());
        }
    }

    fn show_snuffling_windows(&mut self, ctx: &egui::Context) {
        if let Some(name) = self.session.bridge.take_pending_cake_model() {
            if name.starts_with("GF Store: ") {
                self.session.snufflings.add_gf_store(name);
            } else {
                self.session.snufflings.add_cake_model(name);
            }
        }

        let (device, progress) = self.session.bridge.seisbench_runtime();
        let runtime = crate::snufflings::SeisbenchRuntimeStatus {
            device,
            progress: progress.map(|progress| crate::snufflings::SeisbenchProgress {
                fraction: progress.fraction,
                label: progress.label,
            }),
        };
        if let Some(action) = crate::snufflings::show_panel(
            ctx,
            &mut self.snuffling_panels,
            &mut self.session.snufflings,
            self.session.state.highpass,
            self.session.state.lowpass,
            &runtime,
        ) {
            self.apply_snuffling_action(action);
        }
        self.figures.show(ctx, &mut self.session);
    }

    fn apply_snuffling_action(&mut self, action: SnufflingAction) {
        let params = self.session.snuffling_params(800);
        match action {
            SnufflingAction::Spectrum => {
                self.session.snufflings.spectrum.open_plot();
                self.figures.invalidate("ampspec");
            }
            SnufflingAction::Spectrogram => {
                self.session.snufflings.spectrogram.open_plot();
                self.figures.invalidate("spectrogram");
            }
            SnufflingAction::Polarization => {
                self.session.snufflings.polarization.open_plot();
                self.figures.invalidate("polarization");
            }
            SnufflingAction::Run(name) => self.session.bridge.run_snuffling(name.into(), params),
            SnufflingAction::Rms => self.session.bridge.rms_traces(params),
            SnufflingAction::CakePlotModel => {
                self.session.snufflings.cake.open_model_plot();
                self.figures.invalidate("cake_model");
            }
            SnufflingAction::CakePlotRays => {
                self.session.snufflings.cake.open_rays_plot();
                self.figures.invalidate("cake_rays");
            }
            SnufflingAction::AddCakeModel => self.session.bridge.add_cake_model(),
            SnufflingAction::LoadGfStoreUrl => {
                let url = self.session.snufflings.seismosizer.load_url();
                if !url.trim().is_empty() {
                    self.session.bridge.load_gf_store_url(url);
                }
            }
            #[cfg(feature = "gf")]
            SnufflingAction::AddGfStore => self.session.bridge.add_gf_store(),
        }
    }

    fn show_auxiliary_windows(&mut self, ctx: &egui::Context) {
        crate::view::help::show(ctx, &mut self.session.state);
        // Only crosses the JS seam while the window is actually open.
        let runtime = self
            .session
            .state
            .show_about
            .then(|| self.session.bridge.runtime_info())
            .flatten();
        crate::view::about::show(ctx, &mut self.session.state, runtime.as_ref());
        let downloads = crate::view::forms::download(ctx, &mut self.session.state);
        if let Some(request) = downloads.waveforms {
            self.session.bridge.download_waveforms(request);
        }
        if let Some(request) = downloads.stations {
            self.session.bridge.fetch_stations(request);
        }
        if let Some(request) = downloads.responses {
            self.session.bridge.fetch_responses(request);
        }

        let seedlink = crate::view::forms::seedlink(
            ctx,
            &mut self.session.state,
            self.session.bridge.seedlink_active(),
        );
        match seedlink {
            Some(crate::view::forms::SeedlinkIntent::Start(params)) => {
                self.clear_annotation_caches();
                self.session.start_seedlink(params);
            }
            Some(crate::view::forms::SeedlinkIntent::Stop) => {
                self.session.bridge.stop_seedlink();
            }
            None => {}
        }
        if let Some(request) = crate::view::forms::catalog(ctx, &mut self.session.state) {
            self.session.bridge.catalog_search(request);
        }
        crate::view::map::window(
            ctx,
            &mut self.map,
            &mut self.session.state,
            &self.session.bridge,
        );
        if let Some(moment_tensor) =
            self.marker_table
                .show(ctx, &mut self.session.state, &self.session.bridge)
        {
            self.figures.open_beachball(moment_tensor);
        }
        crate::snufflings::show_output(ctx, &mut self.session.state);
    }
}

/// What a click will cost, to one decimal, in integer arithmetic — printing one
/// digit is no reason to route a byte count through `f64`.
fn download_size(bytes: u64) -> String {
    if bytes < 1_000_000 {
        return format!("{} kB", bytes.div_ceil(1000));
    }
    let tenths = (bytes + 50_000) / 100_000; // rounded, not truncated
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

/// Tooltip for one example: what it is, then what it is worth opening for.
///
/// Only the blurb's opening paragraph — enough to choose by. The whole thing is
/// shown after the load, in a window that has room for it.
fn example_hover(example: &ExampleDataset) -> String {
    let mut text = example
        .blurb
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    for item in &example.teaches {
        text.push_str("\n• ");
        text.push_str(item);
    }
    text
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
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::a11y::drain(raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.update_frame_stats(ui, frame);
        let ctx = ui.ctx().clone();
        let visible_bins = self.view_sample_bins(ui);
        self.session.sync(ctx.input(|input| input.time));
        self.load_dropped_files(&ctx);
        self.apply_pending_view();

        let scene = self.session.bridge.scene();
        let total_tracks = scene.as_ref().map_or(0, |scene| scene.tracks.len());
        let scene_range = scene.as_ref().map(|scene| (scene.tmin, scene.tmax));
        self.show_status_panel(ui, &ctx, total_tracks);
        self.show_command_panel(ui);
        self.show_waveform_panel(ui, total_tracks, scene_range);
        self.handle_export_requests(&ctx);
        self.show_snuffling_windows(&ctx);
        self.show_auxiliary_windows(&ctx);
        // After layout + input: the waveform panel just updated the track window
        // and pan/zoom state this request must reflect (see Session::request_view).
        self.session.request_view(visible_bins);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}
