use egui::{Color32, FontId};

use crate::bridge::{
    Bridge, CatalogRequest, FigureRequest, LiveSeedlinkSession, MarkerFile, MarkerFileEntry,
    MomentTensor, Nslc, SceneMeta, SeedlinkParams, SetActiveEventRequest, SnufflingRequestParams,
    StationRequest, Status, ViewParams, WaveformDownloadRequest,
};
use crate::snufflings::Action as SnufflingAction;
use crate::state::{
    AppState, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength, WiggleTheme,
};
use crate::view::markers::{MARKER_COLORS, Marker, MarkerColumn};
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = __snufflerFrameStats)]
    fn js_snuffler_frame_stats(frame_ms: f64);
}

const FRAME_STATS_REPORT_INTERVAL_S: f64 = 0.25;
const FRAME_STATS_EMA_ALPHA: f64 = 0.2;

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
const VIEW_TIME_OVERSCAN: f64 = 2.0;
const VIEW_TIME_PAN_BUCKET_FRACTION: f64 = 0.5;

fn fdsn_datacenter(selected: &str, custom_base_url: &str) -> String {
    let custom = custom_base_url.trim();
    if custom.is_empty() {
        selected.to_owned()
    } else {
        custom.to_owned()
    }
}

fn bucketed_time_start(origin: f64, tmin: f64, bucket: f64) -> f64 {
    if !bucket.is_finite() || bucket <= 0.0 {
        return tmin;
    }
    origin + ((tmin - origin) / bucket).floor() * bucket
}

/// The egui application: UI state plus the bridge to pyrocko (via Pyodide).
pub struct App {
    state: AppState,
    bridge: Bridge,
    /// Scene generation we last fit the view window to (resets on new data).
    last_scene_gen: u64,
    wf_cache: WaterfallCache,
    /// Decoded textures for the pyrocko/matplotlib figure windows.
    figures: crate::view::figure::FigureCache,
    /// The figure request the amplitude-spectrum window was last rendered for.
    spectrum_key: Option<FigureRequest>,
    /// The figure request the spectrogram window was last rendered for.
    spectrogram_key: Option<FigureRequest>,
    /// The figure request the polarization window was last rendered for.
    polarization_key: Option<FigureRequest>,
    /// Figure requests the Cake "Plot Model" / "Plot Rays" windows last rendered.
    cake_model_key: Option<FigureRequest>,
    cake_rays_key: Option<FigureRequest>,
    /// Focal-mechanism beachball window: the moment tensor to draw (opens the
    /// window when set), plus the request the window last rendered.
    beachball_mt: Option<[f64; 6]>,
    beachball_key: Option<FigureRequest>,
    /// Marker-table sort column + direction (snuffler's clickable headers).
    marker_sort: crate::view::markers::MarkerColumn,
    marker_sort_asc: bool,
    /// Last active-event generation applied to markers (drives Kagan-column refresh).
    last_active_event_gen: u64,
    /// The marker-table cell being edited: (column, marker vec-index, buffer).
    /// At most one cell edits at a time (snuffler's inline table editing).
    marker_edit: Option<(MarkerColumn, usize, String)>,
    /// Last time (ctx seconds) the Watch-Files poll ran.
    last_watch_poll: f64,
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
    /// In-flight off-thread PNG export: receives the encoded bytes from the rayon
    /// pool, paired with the download filename. Only the threaded build runs the
    /// export off-thread; otherwise it rasterizes synchronously (no receiver).
    #[cfg(all(feature = "svg-raster", feature = "threads"))]
    export_rx: Option<(std::sync::mpsc::Receiver<Option<Vec<u8>>>, String)>,
    frame_stats_ms: Option<f64>,
    frame_stats_last_report_s: f64,
}

#[cfg(target_arch = "wasm32")]
fn report_frame_stats(frame_ms: f64) {
    js_snuffler_frame_stats(frame_ms);
}

#[cfg(not(target_arch = "wasm32"))]
fn report_frame_stats(_frame_ms: f64) {}

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
            figures: crate::view::figure::FigureCache::default(),
            spectrum_key: None,
            spectrogram_key: None,
            polarization_key: None,
            cake_model_key: None,
            cake_rays_key: None,
            beachball_mt: None,
            beachball_key: None,
            marker_sort: crate::view::markers::MarkerColumn::Time,
            marker_sort_asc: true,
            last_active_event_gen: 0,
            marker_edit: None,
            last_watch_poll: 0.0,
            last_snuffling_gen: 0,
            last_scene_bounds: None,
            map: crate::view::map::MapState::default(),
            last_waveform_rect: egui::Rect::ZERO,
            screenshot_request: false,
            screenshot_awaiting: false,
            screenshot_crop: egui::Rect::ZERO,
            svg_request: false,
            #[cfg(all(feature = "svg-raster", feature = "threads"))]
            export_rx: None,
            frame_stats_ms: None,
            frame_stats_last_report_s: 0.0,
        }
    }

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
        if self.frame_stats_last_report_s > 0.0
            && now_s - self.frame_stats_last_report_s < FRAME_STATS_REPORT_INTERVAL_S
        {
            return;
        }
        self.frame_stats_last_report_s = now_s;
        report_frame_stats(avg_ms);
    }

    fn apply_live_scene_bounds(&mut self, scene: &SceneMeta, session: LiveSeedlinkSession) {
        let start = session.started_at;
        let window = session.follow_seconds.max(1.0);
        let latest = scene
            .tracks
            .iter()
            .map(|track| track.tmax)
            .fold(scene.tmax, f64::max);

        if latest.is_finite() && latest > start {
            self.state.tmax = latest;
            self.state.tmin = (latest - window).max(start);
        } else {
            self.state.tmin = start;
            self.state.tmax = start + window;
        }
        self.state.track_scroll = 0.0;
        self.state.track_offset = 0;
    }

    fn clear_annotations_for_new_source(&mut self) {
        self.state.markers.clear();
        self.state.snuffling_text.clear();
        self.state.show_snuffling = false;
        self.state.set_active_event_request = None;
        self.state.pending_span = None;
        self.state.picking = None;
        self.state.uncertainty_drag_last_y = None;
        self.marker_edit = None;
        self.beachball_mt = None;
        self.beachball_key = None;
    }

    fn live_seedlink_params(&self) -> SeedlinkParams {
        SeedlinkParams {
            host: self.state.live_seedlink_host.clone(),
            port: self.state.live_seedlink_port.clone(),
            net: self.state.live_seedlink_net.clone(),
            sta: self.state.live_seedlink_sta.clone(),
            selectors: self.state.live_seedlink_selectors.clone(),
        }
    }

    fn start_live_seedlink(&mut self, ctx: &egui::Context) {
        self.clear_annotations_for_new_source();
        self.bridge.start_seedlink(ctx, self.live_seedlink_params());
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
            gain: f64::from(s.gain),
            fix_scale_ranges: s.fix_scale_ranges,
            show_scale_ranges: s.show_scale_ranges,
            show_scale_axes: s.show_scale_axes,
            visible_tmin: Some(s.tmin),
            visible_tmax: Some(s.tmax),
            scale_tmin: Some(s.tmin),
            scale_tmax: Some(s.tmax),
            wf_abs: s.wf_abs,
            wf_integrate: s.wf_integrate,
            wf_color_scale: s.wf_color_scale.as_str().to_string(),
            wf_colormap: s.wf_cmap.label().to_string(),
            wf_clip_lo: f64::from(s.wf_clip.0),
            wf_clip_hi: f64::from(s.wf_clip.1),
            wf_symmetry: s.wf_symmetry,
            wf_reverse: s.wf_reverse,
            track_offset: s.track_offset as u64,
            track_count: s.ntracks_shown as u64,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "overscan is a small constant multiple of an already-clamped render budget"
    )]
    fn current_view_params(&self, visible_npx: u32) -> ViewParams {
        let mut params = self.current_params(visible_npx);
        if self.state.view_mode != ViewMode::Wiggle {
            return params;
        }

        // "Liberal Fetch": fetch a wider window snapped to a coarser grid so pan /
        // zoom reuses already-fetched, already-processed data (fewer refetches).
        let (overscan, bucket_frac) = if self.state.liberal_fetch {
            (3.0, 1.0)
        } else {
            (VIEW_TIME_OVERSCAN, VIEW_TIME_PAN_BUCKET_FRACTION)
        };
        let visible_tmin = self.state.tmin;
        let visible_tmax = self.state.tmax;
        let visible_window = (visible_tmax - visible_tmin).max(1e-9);
        let data_window = visible_window * overscan;
        let origin = self.last_scene_bounds.map_or(0.0, |(tmin, _)| tmin);
        let bucket = visible_window * bucket_frac;
        let bucket_tmin = bucketed_time_start(origin, visible_tmin, bucket);

        params.tmin = bucket_tmin - 0.5 * visible_window;
        params.tmax = params.tmin + data_window;
        params.npx = (f64::from(visible_npx) * overscan).ceil() as u32;
        if self.state.view_drag.is_some() {
            // During pointer drags, keep the bridge key stable inside the
            // overscanned bucket; autoscale still uses the exact visible window.
            params.visible_tmin = Some(params.tmin);
            params.visible_tmax = Some(params.tmax);
        }
        params
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "display dimensions and track counts are small positive UI values; the \
                  rounded integer is a rendering budget, not domain data"
    )]
    fn view_sample_bins(&self, ui: &egui::Ui) -> u32 {
        let ppp = ui.ctx().pixels_per_point();
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        let visible_tracks = self.state.ntracks_shown.max(1) as f32;
        let physical_width = ui.available_width().max(MIN_VIEW_SAMPLE_BINS) * ppp;
        let (bins_per_pixel, emitted_per_bin, visible_cap) = match self.state.view_mode {
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

    fn current_snuffling_params(&self, npx: u32) -> SnufflingRequestParams {
        SnufflingRequestParams {
            view: Some(self.current_params(npx)),
            snufflings: Some(self.state.snufflings.params()),
        }
    }

    /// Build a request to render a pyrocko/matplotlib figure for `kind`: the
    /// current snuffling parameters, a fixed high-resolution canvas (scaled to
    /// fit the window on display), and the active theme so the figure matches the
    /// app. Used as the refetch key — a fresh render fires only when it changes.
    fn figure_request(&self, kind: &str, ctx: &egui::Context) -> FigureRequest {
        FigureRequest {
            kind: kind.to_owned(),
            params: Some(self.current_snuffling_params(800)),
            width_px: 1100,
            height_px: 760,
            dpi: 110.0,
            dark: ctx.theme() == egui::Theme::Dark,
            mt: None,
        }
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
        let Some(view) = self.bridge.view() else {
            return;
        };
        let svg = waveform::render_svg(ctx, &self.state, &view, size, theme);
        drop(view);

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
            self.bridge.save_image_png(png, "snuffler.png".to_owned());
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
                    self.bridge.save_image_png(png, filename);
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
    fn main_menus(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.file_menu(ui, ctx);
        self.data_menu(ui);
        self.view_menu(ui);
        self.options_menu(ui);
        self.scaling_menu(ui);
        self.sorting_menu(ui);
        crate::snufflings::menu(ui, &mut self.state.snufflings, &mut self.state.view_mode);
        self.help_menu(ui);
    }

    /// A permutation of marker indices sorted by the current marker-table column
    /// (numeric, like Qt's sort role; a time tiebreak keeps it deterministic). The
    /// underlying `Vec` order is never reordered so per-row actions map correctly.
    fn marker_row_order(&self) -> Vec<usize> {
        let ms = &self.state.markers;
        let mut order: Vec<usize> = (0..ms.len()).collect();
        let (col, asc) = (self.marker_sort, self.marker_sort_asc);
        let dist_of = |m: &Marker| -> f64 {
            m.nslc
                .as_ref()
                .and_then(|(n, s, _, _)| self.bridge.station_distance(n, s))
                .unwrap_or(f64::INFINITY)
        };
        order.sort_by(|&a, &b| {
            let (Some(ma), Some(mb)) = (ms.get(a), ms.get(b)) else {
                return std::cmp::Ordering::Equal;
            };
            let ord = match col {
                MarkerColumn::Type => ma.is_event.cmp(&mb.is_event),
                MarkerColumn::Time => ma.tmin.total_cmp(&mb.tmin),
                MarkerColumn::Span => (ma.tmax - ma.tmin).total_cmp(&(mb.tmax - mb.tmin)),
                MarkerColumn::Phase => ma
                    .phase
                    .as_deref()
                    .unwrap_or("")
                    .cmp(mb.phase.as_deref().unwrap_or("")),
                MarkerColumn::Pol => ma
                    .polarity
                    .unwrap_or(i8::MIN)
                    .cmp(&mb.polarity.unwrap_or(i8::MIN)),
                MarkerColumn::Kind => (ma.kind & 7).cmp(&(mb.kind & 7)),
                MarkerColumn::Channel => nslc_key(ma).cmp(&nslc_key(mb)),
                MarkerColumn::Dist => dist_of(ma).total_cmp(&dist_of(mb)),
                MarkerColumn::Kagan => ma
                    .kagan_deg
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.kagan_deg.unwrap_or(f64::INFINITY)),
                MarkerColumn::Mag => ma
                    .magnitude
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.magnitude.unwrap_or(f64::INFINITY)),
                MarkerColumn::Depth => ma
                    .depth_km
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.depth_km.unwrap_or(f64::INFINITY)),
                MarkerColumn::Lat => ma
                    .lat
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.lat.unwrap_or(f64::INFINITY)),
                MarkerColumn::Lon => ma
                    .lon
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.lon.unwrap_or(f64::INFINITY)),
            };
            let ord = ord.then_with(|| ma.tmin.total_cmp(&mb.tmin));
            if asc { ord } else { ord.reverse() }
        });
        order
    }

    #[allow(
        clippy::too_many_lines,
        reason = "cohesive File-menu builder; one button per action reads better inline"
    )]
    fn file_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("File", |ui| {
            if ui.button("Open files…").clicked() {
                self.bridge.open_files(ctx);
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
                self.bridge.open_directory(ctx);
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
                self.bridge.reload_files(ctx);
                ui.close();
            }
            if ui
                .button("Print…")
                .on_hover_text("Open the browser print dialog for the current view (also → PDF).")
                .clicked()
            {
                if let Some(w) = web_sys::window() {
                    let _ = w.print();
                }
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
            if ui
                .button("Open StationXML…")
                .on_hover_text(
                    "Load a local StationXML file: station coordinates (distance/azimuth \
                     sorting) + instrument responses (Options ▸ Deconvolution).",
                )
                .clicked()
            {
                self.bridge.load_station_xml(ctx);
                ui.close();
            }
            if ui
                .button("Open stations…")
                .on_hover_text(
                    "Load a local pyrocko station file (coordinates for sorting/cake/map).",
                )
                .clicked()
            {
                self.bridge.load_stations(ctx);
                ui.close();
            }
            if ui
                .button("Open events…")
                .on_hover_text("Load a local pyrocko event file (event markers + active event).")
                .clicked()
            {
                self.bridge.load_events(ctx);
                ui.close();
            }
            ui.separator();
            if ui.button("Save markers…").clicked() {
                self.bridge
                    .save_markers(ctx, markers_to_file(&self.state.markers));
                ui.close();
            }
            if ui
                .add_enabled(
                    self.state.markers.iter().any(|m| m.selected),
                    egui::Button::new("Save selected markers…"),
                )
                .clicked()
            {
                let selected: Vec<Marker> = self
                    .state
                    .markers
                    .iter()
                    .filter(|m| m.selected)
                    .cloned()
                    .collect();
                self.bridge.save_markers(ctx, markers_to_file(&selected));
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
            if ui.button("Live SeedLink…").clicked() {
                self.state.show_live_seedlink = true;
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
            ui.label("Real data via browser networking.");
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
                        let c = f64::midpoint(self.state.tmin, self.state.tmax);
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
            ui.separator();
            ui.checkbox(&mut self.state.watch_files, "Watch Files")
                .on_hover_text(
                    "Automatically reload opened files as they change on disk \
                     (polls every 2 s; File System Access, Chromium).",
                );
            ui.checkbox(&mut self.state.liberal_fetch, "Liberal Fetch Optimization")
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.update_frame_stats(ui, frame);
        let ctx = ui.ctx().clone();

        // Fit the view window whenever a genuinely new scene is loaded.
        let scene_gen = self.bridge.scene_gen();
        if scene_gen != self.last_scene_gen
            && let Some(scene) = self.bridge.scene()
        {
            let bounds = (scene.tmin, scene.tmax);
            if let Some(session) = self.bridge.live_seedlink_session() {
                self.apply_live_scene_bounds(&scene, session);
                self.last_scene_bounds = Some(bounds);
                self.last_scene_gen = scene_gen;
            } else if !scene.tracks.is_empty() {
                let range_changed = self.last_scene_bounds.is_none_or(|old| {
                    (old.0 - bounds.0).abs() > 1e-6 || (old.1 - bounds.1).abs() > 1e-6
                });
                if range_changed {
                    self.state.tmin = scene.tmin;
                    self.state.tmax = scene.tmax;
                    // New time range: scroll back to the top. The track height (and the
                    // derived `track_offset`/`ntracks_shown` caches) are owned by the
                    // waveform layout, so leave them for it to refit.
                    self.state.track_scroll = 0.0;
                    self.state.track_offset = 0;
                }
                self.last_scene_bounds = Some(bounds);
                self.last_scene_gen = scene_gen;
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

        let scene = self.bridge.scene();
        let total_tracks = scene.as_ref().map_or(0, |s| s.tracks.len());
        let scene_range = scene.as_ref().map(|s| (s.tmin, s.tmax));
        self.bridge.refresh_seisbench_progress(&ctx);

        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                self.main_menus(ui, &ctx);

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
                if self.state.picking.is_some() {
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
                // Continuous UTC time under the cursor (like snuffler's status
                // readout). Uses the previous frame's waveform rect — a harmless
                // one-frame lag for a hover readout.
                if let Some(pos) = ctx.pointer_hover_pos() {
                    let rect = self.last_waveform_rect;
                    if rect.is_positive() && pos.x >= rect.left() && pos.x <= rect.right() {
                        let frac =
                            f64::from(((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
                        let t = self.state.tmin + frac * (self.state.tmax - self.state.tmin);
                        ui.separator();
                        ui.monospace(crate::view::timeaxis::format_full(t));
                    }
                }
            });
        });

        // Apply outcomes of any ':' command that resolved since last frame.
        if let Some((t, tlen)) = self.bridge.take_goto() {
            // A window length (from the goto date's specificity) zooms to it;
            // otherwise keep the current window centred on the target.
            let w = tlen.unwrap_or_else(|| (self.state.tmax - self.state.tmin).max(1.0));
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
                                "goto <time> · hide [n|s|l|c] <pat> · unhide · n|s|l|c <pat> · \
                                 markers [all|<digits>] · scaling [n.s.l.c] vmin vmax",
                            ),
                    );
                    resp.request_focus();
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let line = std::mem::take(&mut self.state.command_text);
                        if !line.trim().is_empty() {
                            self.state.command_feedback.clear();
                            // `markers` is native-only (markers live in Rust state,
                            // not Python) — intercept it before the Python round-trip.
                            match crate::view::markers::parse_markers_command(&line) {
                                Some(crate::view::markers::MarkersCommand::Set(kinds)) => {
                                    self.state.set_visible_marker_kinds(kinds);
                                }
                                Some(crate::view::markers::MarkersCommand::NoChange) => {}
                                None => self.bridge.run_command(&ctx, line),
                            }
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
                let npx = self.view_sample_bins(ui);
                let epoch = self.bridge.view_epoch();
                let wave_resp = {
                    let view = self.bridge.view();
                    waveform::show(
                        ui,
                        &mut self.state,
                        view.as_deref(),
                        &mut self.wf_cache,
                        epoch,
                        scene_range,
                    )
                };
                let params = self.current_view_params(npx);
                self.bridge.ensure_view(&ctx, params);
                wave_resp
            });
        self.last_waveform_rect = central.response.rect;
        // Right-click anywhere on the trace view opens the full app menu at the
        // pointer, reusing the exact menubar builders (Qt pops the same menu).
        central.inner.context_menu(|ui| {
            self.main_menus(ui, &ctx);
        });

        // The 'r' key sets a flag (keyboard handlers can't touch the bridge); run
        // the reload here where the bridge is reachable.
        if std::mem::take(&mut self.state.reload_requested) {
            self.bridge.reload_files(&ctx);
        }
        if std::mem::take(&mut self.state.reload_snufflings_requested) {
            self.bridge.reload_snufflings(&ctx);
        }
        // "Watch Files": poll opened handles for on-disk changes every ~2 s.
        if self.state.watch_files {
            let now = ctx.input(|i| i.time);
            if now - self.last_watch_poll > 2.0 {
                self.last_watch_poll = now;
                self.bridge.watch_poll(&ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }

        // 'e'/'E' queued an active-event change; send it to the bridge here.
        if let Some(req) = self.state.set_active_event_request.take() {
            self.bridge.set_active_event(
                &ctx,
                SetActiveEventRequest {
                    deactivate: req.deactivate,
                    event_hash: req.event_hash,
                    t: req.t,
                    nslc: req
                        .nslc
                        .map(|(net, sta, loc, cha)| Nslc { net, sta, loc, cha }),
                },
            );
        }
        // When the active event changed, refresh each event marker's Kagan angle
        // from the recomputed values (distances refetch via active_event_gen).
        let aeg = self.bridge.active_event_gen();
        if aeg != self.last_active_event_gen {
            self.last_active_event_gen = aeg;
            for m in &mut self.state.markers {
                if let Some(hash) = m.event_hash.clone() {
                    m.kagan_deg = self.bridge.kagan_for(&hash);
                }
            }
        }

        // SVG export: re-render the current view through the shared DrawTarget to a
        // vector file at the on-screen size (no framebuffer round-trip needed).
        if std::mem::take(&mut self.svg_request) {
            let theme = ctx.theme();
            let size = self.last_waveform_rect.size();
            let svg = self
                .bridge
                .view()
                .map(|view| waveform::render_svg(&ctx, &self.state, &view, size, theme));
            if let Some(svg) = svg {
                self.bridge.save_image_svg(svg, "snuffler.svg".to_owned());
            }
        }

        // PNG export. With `svg-raster` (default) we rasterize the same vector SVG
        // at high DPI via resvg. Otherwise we capture the egui framebuffer at screen
        // resolution (delivered a couple frames later as Event::Screenshot).
        if std::mem::take(&mut self.screenshot_request) {
            #[cfg(feature = "svg-raster")]
            self.start_png_export(&ctx);
            #[cfg(not(feature = "svg-raster"))]
            {
                self.screenshot_crop = self.last_waveform_rect;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_awaiting = true;
            }
        }
        #[cfg(all(feature = "svg-raster", feature = "threads"))]
        self.poll_png_export(&ctx);
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

        // A custom earth model just picked ("Add Model") or a GF store just mounted
        // ("Add GF Store…"). A mounted store goes to every panel that can use it.
        if let Some(name) = self.bridge.take_pending_cake_model() {
            if name.starts_with("GF Store: ") {
                self.state.snufflings.add_gf_store(name);
            } else {
                self.state.snufflings.add_cake_model(name);
            }
        }

        let (seisbench_device, seisbench_progress) = self.bridge.seisbench_runtime();
        if let Some(action) = crate::snufflings::show_panel(
            &ctx,
            &mut self.state.snufflings,
            self.state.highpass,
            self.state.lowpass,
            &crate::snufflings::SeisbenchRuntimeStatus {
                device: seisbench_device,
                progress: seisbench_progress.map(|progress| crate::snufflings::SeisbenchProgress {
                    fraction: progress.fraction,
                    label: progress.label,
                }),
            },
        ) {
            let params = self.current_snuffling_params(800);
            match action {
                SnufflingAction::Spectrum => {
                    self.state.snufflings.open_spectrum_plot();
                    self.spectrum_key = None;
                }
                SnufflingAction::Spectrogram => {
                    self.state.snufflings.open_spectrogram_plot();
                    self.spectrogram_key = None;
                }
                SnufflingAction::Polarization => {
                    self.state.snufflings.open_polarization_plot();
                    self.polarization_key = None;
                }
                SnufflingAction::Run(name) => {
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
                SnufflingAction::LoadGfStoreUrl => {
                    let url = self.state.snufflings.seis_load_url();
                    if !url.trim().is_empty() {
                        self.bridge.load_gf_store_url(&ctx, url);
                    }
                }
                #[cfg(feature = "gf")]
                SnufflingAction::AddGfStore => self.bridge.add_gf_store(&ctx),
            }
        }

        // Amplitude-spectrum snuffling window: the real pyrocko/matplotlib figure
        // (ampspec.py), rendered in Pyodide and shown as an image — not a Rust
        // reimplementation.
        if self.state.snufflings.show_spectrum_plot() {
            let req = self.figure_request("ampspec", &ctx);
            if self.spectrum_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.spectrum_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Amplitude spectrum")
                .open(&mut open)
                .default_size([620.0, 460.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "ampspec");
                });
            if !open {
                self.state.snufflings.close_spectrum_plot();
                self.spectrum_key = None;
            }
        }

        // Spectrogram snuffling window: pyrocko's real STFT matplotlib figure
        // (snufflings/spectrogram.py), rendered in Pyodide and shown as an image.
        if self.state.snufflings.show_spectrogram_plot() {
            let req = self.figure_request("spectrogram", &ctx);
            if self.spectrogram_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.spectrogram_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Spectrogram")
                .open(&mut open)
                .default_size([680.0, 480.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "spectrogram");
                });
            if !open {
                self.state.snufflings.close_spectrogram_plot();
                self.spectrogram_key = None;
            }
        }

        // Polarization snuffling window: pyrocko's real per-station polarization
        // figure (snufflings/polarization.py) — hodograms + eigensystem.
        if self.state.snufflings.show_polarization_plot() {
            let req = self.figure_request("polarization", &ctx);
            if self.polarization_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.polarization_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Polarization")
                .open(&mut open)
                .default_size([760.0, 420.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "polarization");
                });
            if !open {
                self.state.snufflings.close_polarization_plot();
                self.polarization_key = None;
            }
        }

        // Cake Phase "Plot Model" window: pyrocko's real matplotlib model figure
        // (P/S velocity vs depth), rendered in Pyodide via cake_plot.my_model_plot.
        if self.state.snufflings.show_cake_model_plot() {
            let req = self.figure_request("cake_model", &ctx);
            if self.cake_model_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.cake_model_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Cake model")
                .open(&mut open)
                .default_size([620.0, 480.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "cake_model");
                });
            if !open {
                self.state.snufflings.close_cake_model_plot();
                self.cake_model_key = None;
            }
        }

        // Cake Phase "Plot Rays" window: pyrocko's real matplotlib ray-path figure
        // (cake_plot.my_rays_plot) over the shaded model.
        if self.state.snufflings.show_cake_rays_plot() {
            let req = self.figure_request("cake_rays", &ctx);
            if self.cake_rays_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.cake_rays_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Cake ray paths")
                .open(&mut open)
                .default_size([680.0, 480.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "cake_rays");
                });
            if !open {
                self.state.snufflings.close_cake_rays_plot();
                self.cake_rays_key = None;
            }
        }

        // Focal-mechanism beachball window: pyrocko's real matplotlib beachball
        // (plot_beachball_mpl) for an event marker's moment tensor — opened from
        // the marker table's ◍ button; never re-drawn in Rust.
        if let Some(mt) = self.beachball_mt {
            let req = FigureRequest {
                kind: "beachball".to_owned(),
                params: None,
                width_px: 520,
                height_px: 520,
                dpi: 120.0,
                dark: ctx.theme() == egui::Theme::Dark,
                mt: Some(MomentTensor {
                    mnn: mt[0],
                    mee: mt[1],
                    mdd: mt[2],
                    mne: mt[3],
                    mnd: mt[4],
                    med: mt[5],
                }),
            };
            if self.beachball_key.as_ref() != Some(&req) {
                self.bridge.render_figure(&ctx, req.clone());
                self.beachball_key = Some(req);
            }
            let mut open = true;
            egui::Window::new("Focal mechanism")
                .open(&mut open)
                .default_size([440.0, 480.0])
                .show(&ctx, |ui| {
                    self.figures.show(ui, &self.bridge, "beachball");
                });
            if !open {
                self.beachball_mt = None;
                self.beachball_key = None;
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
                            for dc in [
                                "geofon",
                                "earthscope",
                                "iris",
                                "orfeus",
                                "ingv",
                                "resif",
                                "bgr",
                            ] {
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
                            ui.label("Custom base URL");
                            ui.text_edit_singleline(&mut self.state.dl_base_url);
                            ui.end_row();
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
                        datacenter: fdsn_datacenter(
                            &self.state.dl_datacenter,
                            &self.state.dl_base_url,
                        ),
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
                    datacenter: fdsn_datacenter(&self.state.dl_datacenter, &self.state.dl_base_url),
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

        if self.state.show_live_seedlink {
            let mut open = true;
            let mut start = false;
            let mut stop = false;
            egui::Window::new("Live SeedLink")
                .open(&mut open)
                .default_width(420.0)
                .show(&ctx, |ui| {
                    egui::Grid::new("seedlink_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Host");
                            ui.text_edit_singleline(&mut self.state.live_seedlink_host);
                            ui.end_row();
                            ui.label("Port");
                            ui.text_edit_singleline(&mut self.state.live_seedlink_port);
                            ui.end_row();
                            ui.label("Network");
                            ui.text_edit_singleline(&mut self.state.live_seedlink_net);
                            ui.end_row();
                            ui.label("Station");
                            ui.text_edit_singleline(&mut self.state.live_seedlink_sta);
                            ui.end_row();
                            ui.label("Selectors");
                            ui.text_edit_singleline(&mut self.state.live_seedlink_selectors);
                            ui.end_row();
                        });
                    let active = self.bridge.seedlink_active();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!active, egui::Button::new("Start"))
                            .clicked()
                        {
                            start = true;
                        }
                        if ui.add_enabled(active, egui::Button::new("Stop")).clicked() {
                            stop = true;
                        }
                    });
                });
            if start {
                self.start_live_seedlink(&ctx);
            }
            if stop {
                self.bridge.stop_seedlink();
            }
            self.state.show_live_seedlink = open;
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
                            for dc in ["usgs", "geofon", "earthscope", "iris", "ingv"] {
                                ui.selectable_value(
                                    &mut self.state.cat_datacenter,
                                    dc.to_string(),
                                    dc,
                                );
                            }
                        });
                    egui::Grid::new("cat_grid").num_columns(2).show(ui, |ui| {
                        ui.label("Custom base URL");
                        ui.text_edit_singleline(&mut self.state.cat_base_url);
                        ui.end_row();
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
                    ui.checkbox(
                        &mut self.state.cat_with_mt,
                        "Fetch moment tensors (beachball / Kagan)",
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
                        datacenter: fdsn_datacenter(
                            &self.state.cat_datacenter,
                            &self.state.cat_base_url,
                        ),
                        start: self.state.cat_start.clone(),
                        end: self.state.cat_end.clone(),
                        minmag: self.state.cat_minmag,
                        with_mt: self.state.cat_with_mt,
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
            self.bridge
                .ensure_station_distances(&ctx, self.bridge.snuffling_gen());
            let mut open = true;
            let mut center_t: Option<f64> = None;
            let mut open_beachball: Option<[f64; 6]> = None;
            egui::Window::new("Markers")
                .open(&mut open)
                .default_size([560.0, 320.0])
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        let nsel = self.state.markers.iter().filter(|m| m.selected).count();
                        ui.label(format!(
                            "{} markers · {nsel} selected",
                            self.state.markers.len()
                        ));
                        if ui.button("Select all").clicked() {
                            for m in &mut self.state.markers {
                                m.selected = true;
                            }
                        }
                        if ui.button("Deselect all").clicked() {
                            for m in &mut self.state.markers {
                                m.selected = false;
                            }
                        }
                        if ui.button("Delete selected").clicked() {
                            self.state.markers.retain(|m| !m.selected);
                        }
                        if ui.button("Clear all").clicked() {
                            self.state.markers.clear();
                        }
                    });
                    ui.separator();
                    // Marker rows are shown via a sort permutation so the underlying
                    // Vec order stays as-inserted (content-identified); clicking a
                    // header re-sorts, like snuffler's clickable marker table.
                    let order = self.marker_row_order();
                    let (mut sort_col, mut sort_asc) = (self.marker_sort, self.marker_sort_asc);
                    let mut edit = self.marker_edit.take();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        egui::Grid::new("marker_table")
                            .striped(true)
                            .num_columns(14)
                            .spacing([10.0, 3.0])
                            .show(ui, |ui| {
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Type,
                                    "",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Time,
                                    "time (UTC)",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Span,
                                    "span",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Phase,
                                    "phase",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Pol,
                                    "pol",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Kind,
                                    "kind",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Channel,
                                    "channel",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Dist,
                                    "dist",
                                )
                                .on_hover_text("great-circle distance from the active event [°]");
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Kagan,
                                    "Kagan [°]",
                                )
                                .on_hover_text(
                                    "Kagan angle between this event's focal mechanism and the \
                                     active event's (0° = identical, 120° = maximally different)",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Mag,
                                    "mag",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Depth,
                                    "depth [km]",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Lat,
                                    "lat",
                                );
                                sort_header(
                                    ui,
                                    &mut sort_col,
                                    &mut sort_asc,
                                    MarkerColumn::Lon,
                                    "lon",
                                );
                                ui.strong("");
                                ui.end_row();
                                for &row in &order {
                                    let Some(m) = self.state.markers.get_mut(row) else {
                                        continue;
                                    };
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
                                    // Phase pickers can rename their phase inline;
                                    // an event's `phase` holds its label (read-only).
                                    edit_text_cell(
                                        ui,
                                        &mut edit,
                                        row,
                                        MarkerColumn::Phase,
                                        &mut m.phase,
                                        !m.is_event,
                                    );
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
                                    if ui
                                        .small_button(format!("{}", m.kind & 7))
                                        .on_hover_text("cycle marker kind / colour (0–7)")
                                        .clicked()
                                    {
                                        m.kind = (m.kind + 1) & 7;
                                    }
                                    ui.label(
                                        m.nslc
                                            .as_ref()
                                            .map(|(n, s, l, c)| format!("{n}.{s}.{l}.{c}"))
                                            .unwrap_or_default(),
                                    );
                                    let dist = m.nslc.as_ref().and_then(|(n, s, _, _)| {
                                        self.bridge.station_distance(n, s)
                                    });
                                    ui.label(
                                        dist.map_or_else(
                                            || "—".to_string(),
                                            |d| format!("{d:.2}°"),
                                        ),
                                    );
                                    ui.label(
                                        m.kagan_deg
                                            .map_or_else(|| "—".to_string(), |k| format!("{k:.1}")),
                                    );
                                    let ev = m.is_event;
                                    edit_num_cell(
                                        ui,
                                        &mut edit,
                                        row,
                                        MarkerColumn::Mag,
                                        &mut m.magnitude,
                                        1,
                                        ev,
                                    );
                                    edit_num_cell(
                                        ui,
                                        &mut edit,
                                        row,
                                        MarkerColumn::Depth,
                                        &mut m.depth_km,
                                        1,
                                        ev,
                                    );
                                    edit_num_cell(
                                        ui,
                                        &mut edit,
                                        row,
                                        MarkerColumn::Lat,
                                        &mut m.lat,
                                        2,
                                        ev,
                                    );
                                    edit_num_cell(
                                        ui,
                                        &mut edit,
                                        row,
                                        MarkerColumn::Lon,
                                        &mut m.lon,
                                        2,
                                        ev,
                                    );
                                    ui.horizontal(|ui| {
                                        if ui
                                            .small_button("⌖")
                                            .on_hover_text("center view")
                                            .clicked()
                                        {
                                            center_t = Some(f64::midpoint(m.tmin, m.tmax));
                                        }
                                        if let Some(mt) = m.moment_tensor
                                            && ui
                                                .small_button("◍")
                                                .on_hover_text("show focal-mechanism beachball")
                                                .clicked()
                                        {
                                            open_beachball = Some(mt);
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                    self.marker_sort = sort_col;
                    self.marker_sort_asc = sort_asc;
                    self.marker_edit = edit;
                });
            if let Some(t) = center_t {
                let w = (self.state.tmax - self.state.tmin).max(1.0);
                self.state.tmin = t - w / 2.0;
                self.state.tmax = t + w / 2.0;
            }
            if let Some(mt) = open_beachball {
                self.beachball_mt = Some(mt);
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
                    let mut mk = Marker::new(
                        m.t,
                        m.tmax.unwrap_or(m.t),
                        u8::try_from(m.kind).unwrap_or(0),
                    );
                    mk.phase.clone_from(&m.phase);
                    mk.is_event = m.is_event;
                    mk.nslc = m
                        .nslc
                        .as_ref()
                        .map(|n| (n.net.clone(), n.sta.clone(), n.loc.clone(), n.cha.clone()));
                    mk.polarity = m.polarity.and_then(|p| i8::try_from(p).ok());
                    mk.moment_tensor = m
                        .moment_tensor
                        .as_ref()
                        .map(|t| [t.mnn, t.mee, t.mdd, t.mne, t.mnd, t.med]);
                    mk.kagan_deg = m.kagan_deg;
                    mk.uncertainty = m.uncertainty;
                    mk.event_hash.clone_from(&m.event_hash);
                    mk.magnitude = m.magnitude;
                    mk.depth_km = m.depth.map(|d| d / 1000.0);
                    mk.lat = m.lat;
                    mk.lon = m.lon;
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
                uncertainty: m.uncertainty,
                is_event: m.is_event,
                magnitude: m.magnitude,
                depth: m.depth_km.map(|d| d * 1000.0),
                lat: m.lat,
                lon: m.lon,
                event_hash: m.event_hash.clone(),
                moment_tensor: m.moment_tensor.map(|t| MomentTensor {
                    mnn: t[0],
                    mee: t[1],
                    mdd: t[2],
                    mne: t[3],
                    mnd: t[4],
                    med: t[5],
                }),
            })
            .collect(),
    }
}

/// The `n.s.l.c` string a marker row displays in the channel column (empty when
/// the marker applies to all tracks), used as its sort key.
fn nslc_key(m: &Marker) -> String {
    m.nslc
        .as_ref()
        .map(|(n, s, l, c)| format!("{n}.{s}.{l}.{c}"))
        .unwrap_or_default()
}

/// A clickable marker-table column header: shows a ▲/▼ arrow on the active sort
/// column, toggles direction on re-click, and starts ascending on a new column.
fn sort_header(
    ui: &mut egui::Ui,
    cur: &mut MarkerColumn,
    asc: &mut bool,
    col: MarkerColumn,
    label: &str,
) -> egui::Response {
    // egui's bundled font lacks the ▲/▼ triangles (they render as tofu), so use
    // ASCII carets for the sort-direction indicator.
    let active = *cur == col;
    let text = if active {
        format!("{label} {}", if *asc { '^' } else { 'v' })
    } else {
        label.to_string()
    };
    let resp = ui.add(egui::Button::new(egui::RichText::new(text).strong()).frame(false));
    if resp.clicked() {
        if active {
            *asc = !*asc;
        } else {
            *cur = col;
            *asc = true;
        }
    }
    resp
}

/// A marker-table numeric cell that becomes an inline text editor on click and
/// commits a valid parse on Enter/blur; an invalid parse is discarded (Qt's
/// toFloat validity check). Non-editable cells render as a plain label.
fn edit_num_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    row: usize,
    col: MarkerColumn,
    value: &mut Option<f64>,
    prec: usize,
    editable: bool,
) {
    let shown = value.map_or_else(|| "—".to_string(), |v| format!("{v:.prec$}"));
    if !editable {
        ui.label(shown);
        return;
    }
    if matches!(edit, Some((c, r, _)) if *c == col && *r == row) {
        if let Some((_, _, buf)) = edit {
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(56.0));
            if resp.lost_focus() {
                if let Ok(v) = buf.trim().parse::<f64>() {
                    *value = Some(v);
                }
                *edit = None;
            } else {
                resp.request_focus();
            }
        }
    } else if ui
        .add(egui::Label::new(shown).sense(egui::Sense::click()))
        .on_hover_text("click to edit")
        .clicked()
    {
        *edit = Some((
            col,
            row,
            value.map_or(String::new(), |v| format!("{v:.prec$}")),
        ));
    }
}

/// A marker-table string cell (the phase name) with the same click-to-edit,
/// commit-on-blur behaviour; empty commits clear the value.
fn edit_text_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    row: usize,
    col: MarkerColumn,
    value: &mut Option<String>,
    editable: bool,
) {
    let shown = value.clone().unwrap_or_default();
    if !editable {
        ui.label(shown);
        return;
    }
    if matches!(edit, Some((c, r, _)) if *c == col && *r == row) {
        if let Some((_, _, buf)) = edit {
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(110.0));
            if resp.lost_focus() {
                let trimmed = buf.trim();
                *value = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                };
                *edit = None;
            } else {
                resp.request_focus();
            }
        }
    } else if ui
        .add(egui::Label::new(shown).sense(egui::Sense::click()))
        .on_hover_text("click to edit")
        .clicked()
    {
        *edit = Some((col, row, value.clone().unwrap_or_default()));
    }
}

const HELP_KEY_ROWS: &[(&str, &str)] = &[
    ("q", "Quit / close viewer"),
    (
        "r",
        "Reload opened files changed on disk (Chromium file access)",
    ),
    (
        "R",
        "Reload the Python snuffling code (bridge.py), keeping the loaded pile",
    ),
    ("f", "Toggle full screen mode"),
    ("m", "Toggle marker sidebar"),
    ("c", "Toggle main controls"),
    (":", "Enter command"),
    ("<space>", "Forward one page in time"),
    ("b", "Backward one page in time"),
    ("<pagedown>", "Scroll tracks one page down"),
    ("<pageup>", "Scroll tracks one page up"),
    ("+", "Shorter tracks (show one more)"),
    ("-", "Taller tracks (show one less)"),
    ("=", "Reset track height (auto-fit)"),
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
        "Enter batch-picking mode / abort the current pick",
    ),
    ("?", "Help"),
];

const HELP_MOUSE_ROWS: &[(&str, &str)] = &[
    ("Click and drag", "Zoom and pan"),
    ("Click and drag on time axis", "Pan only"),
    ("Click on marker", "Select marker"),
    ("<shift> + click on marker", "Select additional marker"),
    ("Wheel", "Scroll tracks vertically"),
    ("<ctrl> + wheel", "Zoom track height (more / fewer tracks)"),
    (
        "Right-click",
        "Context menu (File / Data / View / Options / Scaling / Sorting / Snufflings / Help)",
    ),
    ("Double-click", "Enter picking mode / place pick"),
    (
        "Shift and left-click",
        "Uncertainty selection for current phase marker (native only)",
    ),
];

const HELP_COMMAND_ROWS: &[(&str, &str)] = &[
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
    (
        "markers [ all | <digits> ]",
        "Show only markers of the given colour kinds (0..7); bare 'markers' hides all.",
    ),
    (
        "scaling [ n.s.l.c ] vmin vmax",
        "Pin amplitude scale range(s); 'nan' leaves a bound auto; 'scaling' alone clears.",
    ),
];

fn help_text(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        help_section(ui, "Key", HELP_KEY_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Mouse", HELP_MOUSE_ROWS, false);
        ui.add_space(14.0);
        help_section(ui, "Command", HELP_COMMAND_ROWS, true);
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
