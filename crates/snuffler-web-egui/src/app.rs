use std::cell::Cell;
use std::fmt::Write as _;

use egui::FontId;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::snufflings::Action as SnufflingAction;
use crate::view::marker_table::markers_to_file;
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};
use snuffler_web_core::activity::{
    CancellationCapability, JobError, JobHandle, JobRecord, JobState, Operation, Outcome, Progress,
    RuntimeState, Submission,
};
use snuffler_web_core::bridge::BridgeError;
use snuffler_web_core::bridge::{ExampleDataset, Repaint};
use snuffler_web_core::markers::Marker;
use snuffler_web_core::session::Session;
use snuffler_web_core::state::{
    ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength,
};
use snuffler_web_core::theme::WiggleTheme;

/// The dataset the app opens on when the URL asks for nothing in particular.
const BOOT_EXAMPLE: &str = "sumatra-2004-12-26-m9.1-surface-waves";

/// How often the displayed frame rate is allowed to change. The measurement is
/// smoothed every frame; only the number on screen is held still, because one
/// that rewrites itself at 60 Hz is unreadable.
const FRAME_STATS_REFRESH_INTERVAL_S: f64 = 0.25;
const FRAME_STATS_EMA_ALPHA: f64 = 0.2;

#[cfg(all(feature = "svg-raster", feature = "threads"))]
struct PngExport {
    receiver: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
    filename: String,
    job: JobHandle,
}

#[cfg(feature = "threads")]
struct MarkerExport {
    receiver: std::sync::mpsc::Receiver<snuffler_web_core::bridge::MarkerFile>,
    job: JobHandle,
}

#[derive(Clone, Copy)]
enum MarkerExportScope {
    All,
    Selected,
}

/// The widest the readout can get (the rate is capped at 60), used to reserve a
/// width that does not change as the number does.
const FRAME_STATS_WIDEST: &str = "60 fps";

thread_local! {
    static EXPORTED_FRAME_MS: Cell<f64> = const { Cell::new(0.0) };
}

/// Returns the current frame-cost estimate for browser performance tests.
#[wasm_bindgen]
#[must_use]
pub fn snuffler_frame_stats() -> String {
    EXPORTED_FRAME_MS.with(|frame_ms| {
        let frame_ms = frame_ms.get();
        let fps = if frame_ms > 0.0 {
            (1000.0 / frame_ms).clamp(0.0, 60.0)
        } else {
            0.0
        };
        format!("{{\"emaMs\":{frame_ms:.3},\"fps\":{fps:.3}}}")
    })
}

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

/// The egui font family the waveform canvas draws every label in — bold, as
/// snuffler's canvas is. Registered in [`configure_look`].
pub const CANVAS_FAMILY: &str = "canvas";

// `npx` is a per-trace bin budget for Python's min/max envelope. The display-scale
// bin budget keeps transport and renderer work bounded when zoomed far out.
//
// One bin per physical pixel is what makes the envelope invisible: the two
// extremes of a bin land in the pixel column they came from, so an Adaptive
// window draws the same trace a fixed visible length draws from raw samples. The
// total-points cap is a backstop for a very tall stack, not the usual governor —
// set below what a screenful of rows at full width costs, it would coarsen every
// trace to a fraction of the canvas's resolution and the traces would visibly
// blockify.
const MIN_VIEW_SAMPLE_BINS: f32 = 64.0;
const MAX_VIEW_SAMPLE_BINS_PER_TRACE: f32 = 8192.0;
const WIGGLE_BINS_PER_PHYSICAL_PIXEL: f32 = 1.0;
const WATERFALL_BINS_PER_PHYSICAL_PIXEL: f32 = 1.0;
const MAX_VISIBLE_WIGGLE_POINTS: f32 = 160_000.0;
const MAX_VISIBLE_WATERFALL_CELLS: f32 = 400_000.0;
/// The envelope emits a bin's minimum and its maximum, in the order they occur.
const WIGGLE_POINTS_PER_BIN: f32 = 2.0;
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
    svg_prepare_job: Option<JobHandle>,
    marker_prepare: Option<(JobHandle, MarkerExportScope)>,
    #[cfg(feature = "threads")]
    marker_export_rx: Option<MarkerExport>,
    /// In-flight off-thread PNG export: receives the encoded bytes from the rayon
    /// pool, paired with the download filename. Only the threaded build runs the
    /// export off-thread; otherwise it rasterizes synchronously (no receiver).
    #[cfg(all(feature = "svg-raster", feature = "threads"))]
    export_rx: Option<PngExport>,
    #[cfg(feature = "svg-raster")]
    png_prepare_job: Option<JobHandle>,
    /// Smoothed egui CPU time per frame, in milliseconds.
    frame_stats_ms: Option<f64>,
    /// The frame rate the top bar is currently showing, and when it last changed.
    frame_stats_shown_fps: Option<u32>,
    frame_stats_shown_at_s: f64,
    /// The light/dark preference to restore when leaving a theme that pins the
    /// canvas. Snuffler Qt takes the *whole* frontend light, not just the waveform:
    /// a white canvas inside dark chrome is neither theme, it just looks broken.
    theme_before_pinning: Option<egui::ThemePreference>,
    /// The persisted-settings snapshot last written to `localStorage`, so a
    /// frame that changed none of them writes nothing.
    settings_saved: Option<crate::persist::Snapshot>,
    /// Last readiness value published on the canvas, so unchanged frames do no DOM work.
    published_ready: Option<bool>,
}

/// Apply a wiggle theme's claim on the frontend's light/dark preference, and
/// return the user's own preference to hold on to for as long as it is overruled.
///
/// A single-canvas theme takes the *whole* frontend with it, not just the
/// waveform: a white canvas inside dark chrome is neither theme, it just looks
/// broken.
/// The choice it overrules is not discarded — it comes back the moment a theme with
/// both canvases is selected, and it is what gets persisted meanwhile.
fn pin_theme(
    ctx: &egui::Context,
    theme: WiggleTheme,
    chosen: egui::ThemePreference,
) -> Option<egui::ThemePreference> {
    if let Some(polarity) = theme.pinned_polarity() {
        let pinned = match polarity {
            snuffler_web_core::theme::Theme::Light => egui::ThemePreference::Light,
            snuffler_web_core::theme::Theme::Dark => egui::ThemePreference::Dark,
        };
        ctx.set_theme(pinned);
        (chosen != pinned).then_some(chosen)
    } else {
        ctx.set_theme(chosen);
        None
    }
}

fn configure_look(ctx: &egui::Context) {
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle;
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
    // Everything snuffler paints onto its canvas is bold: `TimeAx.drawit` sets it on
    // the axis (pile_viewer.py:543), and the paint pass sets a bold font before it
    // draws anything else (pile_viewer.py:3203, applied at :3229), which is what
    // makes its track labels and tick numbers read at a glance. The canvas asks for
    // this family by name; the chrome around it keeps the regular one.
    fonts.font_data.insert(
        "ubuntu_bold".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Ubuntu-Bold.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name(CANVAS_FAMILY.into()),
        vec![
            "ubuntu_bold".to_owned(),
            // Fallbacks for what Ubuntu Bold has no glyph for — the arrows and the
            // per-track ± the regular stack already covers.
            "jetbrains_mono".to_owned(),
            "Ubuntu-Light".to_owned(),
        ],
    );
    ctx.set_fonts(fonts);
    crate::chrome::install(ctx);

    // Both styles, not the active one: egui keeps a `Style` per theme, and
    // `set_global_style` writes only whichever is current — so the sizes below
    // vanished the moment the boot theme was applied, and would have vanished again
    // on any light/dark switch. `all_styles_mut` is what makes them the app's.
    ctx.all_styles_mut(|style| {
        // Some 18 % over egui's defaults, which are tuned for dense tool windows
        // rather than for an app somebody reads for hours while measuring a
        // seismogram against it. The canvas is *not* scaled with this: its labels are
        // held to snuffler's own metrics (`LABEL_PT`, `AXIS_PT` in the render crate)
        // and growing them here would walk that parity back.
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, Proportional)),
            (TextStyle::Body, FontId::new(17.0, Proportional)),
            (TextStyle::Monospace, FontId::new(16.0, Monospace)),
            (TextStyle::Button, FontId::new(17.0, Proportional)),
            (TextStyle::Small, FontId::new(14.5, Proportional)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}

impl App {
    /// Build the app, configure egui styling, and kick off loading the example.
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_look(&cc.egui_ctx);
        crate::a11y::install(&cc.egui_ctx);
        let repaint_now = cc.egui_ctx.clone();
        let repaint_after = cc.egui_ctx.clone();
        let mut session = Session::new(Repaint::new(
            move || repaint_now.request_repaint(),
            move |delay| repaint_after.request_repaint_after(delay),
        ));
        // The settings a reload keeps (the browser's `snuffler.pf`), applied
        // over the defaults before anything reads them.
        let restored = crate::persist::restore(&mut session.state);
        // The restored wiggle theme decides what the light/dark preference is
        // allowed to be: pinning it here, before the first frame, is what keeps a
        // white-canvas theme from flashing through dark chrome on the way in.
        let theme_before_pinning = pin_theme(
            &cc.egui_ctx,
            session.state.wiggle_theme,
            restored.theme_preference,
        );
        // What the app opens on. A `?url=` (with optional `?format=`) or a
        // `?example=<slug>` link is what it was opened *for*, so it takes that
        // place rather than replacing something a second later; `?demo=1` asks
        // for the synthetic pile the parity harness needs; and failing all
        // three, a real earthquake. Sumatra 2004 because it is the one that
        // reads as seismology at a glance — surface waves circling the planet
        // twice, visible without touching a single control. All are issued
        // before the worker has booted; the transport queues them.
        if let Some((url, format)) = crate::deep_link::requested_url() {
            let _submission = session.bridge.load_url(url, format);
        } else {
            let _submission = match crate::deep_link::requested_example() {
                Some(slug) => session.bridge.open_example(slug),
                None if crate::deep_link::wants_demo() => session.bridge.load_example(),
                None => session.bridge.open_example(BOOT_EXAMPLE.to_owned()),
            };
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
            svg_prepare_job: None,
            marker_prepare: None,
            #[cfg(feature = "threads")]
            marker_export_rx: None,
            #[cfg(all(feature = "svg-raster", feature = "threads"))]
            export_rx: None,
            #[cfg(feature = "svg-raster")]
            png_prepare_job: None,
            frame_stats_ms: None,
            frame_stats_shown_fps: None,
            frame_stats_shown_at_s: 0.0,
            theme_before_pinning,
            settings_saved: None,
            published_ready: None,
        }
    }

    fn publish_readiness(&mut self) {
        let ready = self.session.bridge.view_is_settled();
        if self.published_ready != Some(ready) {
            crate::a11y::publish_ready(ready);
            self.published_ready = Some(ready);
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
        EXPORTED_FRAME_MS.with(|frame_ms| frame_ms.set(avg_ms));

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

    #[cfg(feature = "svg-raster")]
    fn complete_png_rasterization(
        &mut self,
        job: JobHandle,
        png: Option<Vec<u8>>,
        filename: String,
    ) {
        self.session.bridge.finish_local(
            job,
            png.as_ref().map_or_else(
                || {
                    Err(BridgeError::browser(
                        "rendering a PNG",
                        "rasterization failed",
                    ))
                },
                |_| Ok(()),
            ),
        );
        if let Some(png) = png {
            let _submission = self.session.bridge.save_image_png(png, filename);
        }
    }

    /// Starts a high-DPI PNG export of the current waveform view.
    ///
    /// SVG generation stays on the main wasm thread because a cloned egui context
    /// shares locks with the live renderer.
    /// Rasterization receives only the owned SVG and runs on rayon when available.
    #[cfg(feature = "svg-raster")]
    fn start_png_export(&mut self, ctx: &egui::Context, job: JobHandle) {
        let theme = ctx.theme();
        let size = self.last_waveform_rect.size();
        let svg = {
            let Some(view) = self.session.bridge.view() else {
                self.session.bridge.finish_local(
                    job,
                    Err(BridgeError::browser(
                        "rendering a PNG",
                        "no waveform view is available",
                    )),
                );
                return;
            };
            waveform::render_svg(ctx, &self.session.state, &view, size, theme, 0.0)
        };

        #[cfg(feature = "threads")]
        if crate::pool::ready() {
            let (tx, rx) = std::sync::mpsc::channel();
            let (w, h) = (size.x, size.y);
            #[expect(
                clippy::disallowed_methods,
                reason = "the ExportPng ledger job accepted above keeps this rasterization visible"
            )]
            rayon::spawn(move || {
                let png = crate::view::raster::rasterize_svg(&svg, w, h, PNG_EXPORT_SCALE);
                crate::pool::record_worker_png_export();
                let _ = tx.send(png);
            });
            if let Some(displaced) = self.export_rx.take() {
                // Unreachable behind the one-export-at-a-time request guard,
                // but a displaced receiver must not leak its ledger job as
                // permanently running.
                self.session.bridge.finish_local(
                    displaced.job,
                    Err(BridgeError::browser(
                        "exporting PNG",
                        "superseded by a newer export",
                    )),
                );
            }
            self.export_rx = Some(PngExport {
                receiver: rx,
                filename: export_filename("png"),
                job,
            });
            return;
        }

        let png = crate::view::raster::rasterize_svg(&svg, size.x, size.y, PNG_EXPORT_SCALE);
        self.complete_png_rasterization(job, png, export_filename("png"));
    }

    /// Delivers a finished off-thread PNG export while the navbar activity keeps
    /// frames coming and communicates that rasterization is still running.
    #[cfg(all(feature = "svg-raster", feature = "threads"))]
    fn poll_png_export(&mut self, ctx: &egui::Context) {
        let status = self
            .export_rx
            .as_ref()
            .map(|export| export.receiver.try_recv());
        match status {
            Some(Ok(png)) => {
                if let Some(export) = self.export_rx.take() {
                    self.complete_png_rasterization(export.job, png, export.filename);
                }
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {
                // Poll the rasterizer at a bounded rate instead of spinning the
                // frame loop at full speed for the duration of the export.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                if let Some(export) = self.export_rx.take() {
                    self.session.bridge.finish_local(
                        export.job,
                        Err(BridgeError::browser(
                            "rendering a PNG",
                            "worker disconnected",
                        )),
                    );
                }
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
        if let Some(action) =
            crate::snufflings::menu(ui, &mut self.snuffling_panels, &mut self.session.state)
        {
            self.apply_snuffling_action(action);
        }
        self.help_menu(ui);
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            self.file_open_items(ui);
            self.file_data_items(ui);
            ui.separator();
            self.file_marker_items(ui);
            ui.separator();
            // The way out of any loaded pile — including the boot example: an
            // empty viewer, which is what Qt opens with no arguments.
            if ui
                .button("Close all")
                .on_hover_text("Discard the loaded data and markers, leaving an empty viewer.")
                .clicked()
            {
                self.session.close_all();
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
            let _submission = self.session.bridge.open_files();
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
            let _submission = self.session.bridge.open_directory();
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
            let _submission = self.session.bridge.reload_files();
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
            let _submission = self.session.bridge.list_examples(false);
            match self.session.bridge.examples() {
                None => {
                    ui.label("Loading…");
                }
                Some(Err(error)) => {
                    ui.label(format!("Unavailable: {error}"));
                    if ui.button("Retry").clicked() {
                        let _submission = self.session.bridge.list_examples(true);
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
                            let _submission = self.session.bridge.load_bundle(
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
        if ui
            .button("Open StationXML files…")
            .on_hover_text(
                "Load one or more local StationXML files: station coordinates \
                 (distance/azimuth sorting) + instrument responses \
                 (Options ▸ Deconvolution).",
            )
            .clicked()
        {
            let _submission = self.session.bridge.load_station_xml();
            ui.close();
        }
        if ui
            .button("Open stations…")
            .on_hover_text("Load a local pyrocko station file (coordinates for sorting/cake/map).")
            .clicked()
        {
            let _submission = self.session.bridge.load_stations();
            ui.close();
        }
        if ui
            .button("Open events…")
            .on_hover_text("Load a local pyrocko event file (event markers + active event).")
            .clicked()
        {
            let _submission = self.session.bridge.load_events();
            ui.close();
        }
    }

    fn file_marker_items(&mut self, ui: &mut egui::Ui) {
        if ui.button("Save markers…").clicked() {
            self.session.state.save_markers_requested = Some(false);
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
            self.session.state.save_markers_requested = Some(true);
            ui.close();
        }
        if ui.button("Load markers…").clicked() {
            let _submission = self.session.bridge.load_markers();
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
        self.theme_before_pinning = pin_theme(ctx, theme, self.chosen_theme_preference(ctx));
    }

    /// The light/dark preference the user actually chose, which is not what egui
    /// holds while a canvas-pinning theme has the frontend pinned light — that is
    /// the theme's doing, not a choice, and persisting it would swallow the real one.
    fn chosen_theme_preference(&self, ctx: &egui::Context) -> egui::ThemePreference {
        self.theme_before_pinning
            .unwrap_or_else(|| ctx.options(|o| o.theme_preference))
    }

    /// Acts on the requests the keyboard handler and menus can only flag:
    /// actions that need the bridge, the export machinery, or a DOM call.
    fn consume_keyboard_requests(&mut self) {
        if std::mem::take(&mut self.session.state.open_files_requested) {
            let _submission = self.session.bridge.open_files();
        }
        if let Some(selected) = self.session.state.save_markers_requested.take() {
            let scope = if selected {
                MarkerExportScope::Selected
            } else {
                MarkerExportScope::All
            };
            self.request_marker_export(scope);
        }
        if std::mem::take(&mut self.session.state.export_image_requested) {
            self.screenshot_request = true;
        }
        if std::mem::take(&mut self.session.state.fullscreen_toggle_requested) {
            toggle_fullscreen();
        }
    }

    fn request_marker_export(&mut self, scope: MarkerExportScope) {
        if self.marker_prepare.is_some()
            || matches!(scope, MarkerExportScope::Selected)
                && !self
                    .session
                    .state
                    .markers
                    .iter()
                    .any(|marker| marker.selected)
        {
            return;
        }
        #[cfg(feature = "threads")]
        if self.marker_export_rx.is_some() {
            return;
        }
        let submission = self.session.bridge.begin_local(
            Operation::SerializeMarkers,
            Some(match scope {
                MarkerExportScope::All => "all markers".to_owned(),
                MarkerExportScope::Selected => "selected markers".to_owned(),
            }),
        );
        if let Submission::Accepted(job) = submission {
            self.marker_prepare = Some((job, scope));
        }
    }

    fn complete_marker_serialization(
        &mut self,
        job: JobHandle,
        file: snuffler_web_core::bridge::MarkerFile,
    ) {
        self.session.bridge.finish_local(job, Ok(()));
        let _submission = self.session.bridge.save_markers(file);
    }

    fn process_marker_export(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        #[cfg(feature = "threads")]
        if let Some(export) = self.marker_export_rx.as_ref() {
            match export.receiver.try_recv() {
                Ok(file) => {
                    let job = export.job;
                    self.marker_export_rx = None;
                    self.complete_marker_serialization(job, file);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let job = export.job;
                    self.marker_export_rx = None;
                    self.session.bridge.finish_local(
                        job,
                        Err(BridgeError::browser(
                            "serializing markers",
                            "worker disconnected",
                        )),
                    );
                }
            }
            return;
        }

        let Some((job, scope)) = self.marker_prepare.take() else {
            return;
        };
        let markers: Vec<Marker> = match scope {
            MarkerExportScope::Selected => self
                .session
                .state
                .markers
                .iter()
                .filter(|marker| marker.selected)
                .cloned()
                .collect(),
            MarkerExportScope::All => self.session.state.markers.iter().cloned().collect(),
        };

        #[cfg(feature = "threads")]
        if crate::pool::ready() {
            let (tx, rx) = std::sync::mpsc::channel();
            #[expect(
                clippy::disallowed_methods,
                reason = "the SerializeMarkers ledger job accepted above owns this serialization"
            )]
            rayon::spawn(move || {
                let _ = tx.send(markers_to_file(&markers));
            });
            self.marker_export_rx = Some(MarkerExport { receiver: rx, job });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }

        let file = markers_to_file(&markers);
        self.complete_marker_serialization(job, file);
    }

    /// The View menu's drawing toggles.
    ///
    /// Three of them are backed by an `Option` rather than a `bool`: Show boxes,
    /// Color traces and Show track separators each follow the canvas or the theme
    /// until the user clicks them, so the checkbox shows what is actually being
    /// drawn and the click records a choice that outlives a theme switch.
    fn drawing_options(&mut self, ui: &mut egui::Ui, waterfall: bool) {
        let canvas = snuffler_web_render::palette::canvas_theme(
            snuffler_web_core::theme::Theme::from_dark(ui.ctx().theme() == egui::Theme::Dark),
            self.session.state.wiggle_theme,
        );
        ui.add_enabled(
            !waterfall,
            egui::Checkbox::new(&mut self.session.state.clip_traces, "Clip traces"),
        );
        let mut boxes = self.session.state.boxes_visible(canvas);
        if ui.checkbox(&mut boxes, "Show boxes").changed() {
            self.session.state.show_boxes = Some(boxes);
        }
        let mut colored = self.session.state.color_traces(canvas);
        if ui
            .add_enabled(
                !waterfall,
                egui::Checkbox::new(&mut colored, "Color traces"),
            )
            .changed()
        {
            self.session.state.show_color_traces = Some(colored);
        }
        ui.checkbox(&mut self.session.state.antialiasing, "Antialiasing");
        ui.separator();
        ui.add_enabled(
            !waterfall,
            egui::Checkbox::new(
                &mut self.session.state.show_scale_ranges,
                "Show scale ranges",
            ),
        );
        ui.add_enabled(
            !waterfall,
            egui::Checkbox::new(&mut self.session.state.show_scale_axes, "Show scale axes"),
        );
        ui.add_enabled(
            !waterfall,
            egui::Checkbox::new(&mut self.session.state.show_zero_lines, "Show zero lines"),
        );
        ui.checkbox(&mut self.session.state.show_time_grid, "Show time grid");
        let mut separators = self.session.state.track_separators();
        if ui
            .checkbox(&mut separators, "Show track separators")
            .changed()
        {
            self.session.state.show_track_separators = Some(separators);
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            self.theme_menu_items(ui);
            ui.separator();

            if ui
                .button("Fullscreen")
                .on_hover_text("Toggle full screen (also on `f`; the browser owns F11)")
                .clicked()
            {
                self.session.state.fullscreen_toggle_requested = true;
                ui.close();
            }
            ui.separator();

            // Waterfall mode disables the wiggle-only options, exactly Qt's
            // interlock set (`viewmode_change`, pile_viewer.py:3956): the
            // visible-length group and the five draw toggles below.
            let waterfall = self.session.state.view_mode == ViewMode::Waterfall;
            ui.label("Visible length");
            ui.add_enabled_ui(!waterfall, |ui| {
                for vl in VisibleLength::ALL {
                    // A target sample count, not a time cap: picking one never moves
                    // the window. It feeds `min_deltat_allow` in the bridge, exactly
                    // as snuffler's radio group feeds `prepare_cutout2` — except for
                    // Adaptive, which is this frontend's own and feeds it nothing.
                    ui.selectable_value(&mut self.session.state.visible_length, vl, vl.label())
                        .on_hover_text(vl.hint());
                }
            });
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
                    .on_hover_text(theme.description())
                    .clicked()
                    && theme != previous
                {
                    self.apply_theme_pinning(ui.ctx(), theme);
                }
            }
            ui.separator();

            self.drawing_options(ui, waterfall);
            ui.separator();
            // The Main Controls and Markers togglers live in the Snufflings
            // menu, where Qt files all of its panel togglers.
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

    fn theme_menu_items(&mut self, ui: &mut egui::Ui) {
        ui.label("Theme");
        // A single-canvas theme owns its polarity (see `WiggleTheme::pinned_polarity`).
        // The control stays visible and says why rather than vanishing under some themes.
        let wiggle = self.session.state.wiggle_theme;
        let pinned = wiggle.pinned_polarity();
        let canvas = match pinned {
            Some(snuffler_web_core::theme::Theme::Dark) => "dark",
            _ => "light",
        };
        ui.add_enabled_ui(pinned.is_none(), egui::global_theme_preference_buttons)
            .response
            .on_disabled_hover_text(format!(
                "{} was designed for a {canvas} canvas only. Pick a wiggle theme that \
                 has both light and dark to choose.",
                wiggle.label()
            ));
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
                .on_hover_text(
                    "Filter in the frequency domain: the same 4th-order Butterworth \
                     response, applied via FFT in one pass instead of recursively.",
                );
            ui.checkbox(
                &mut self.session.state.bandpass_lphp,
                "Bandpass is Low- + Highpass",
            );
            ui.separator();
            ui.checkbox(&mut self.session.state.watch_files, "Watch Files")
                .on_hover_text(
                    "Automatically reload opened files as they change on disk \
                     (polls every 1 s; File System Access, Chromium).",
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
            // Qt's Help ▸ Snuffling Help: one page per snuffling, rendered from
            // its docstring; here one window indexing the built-ins' own
            // descriptions.
            if ui.button("Snuffling Help").clicked() {
                self.session.state.show_snuffling_help = true;
                ui.close();
            }
            // Only for a dataset that came with a description — the shipped
            // examples, and any bundle whose author wrote one. Waveforms opened
            // from disk have nothing to say about themselves, and an entry that
            // opened an empty window would be worse than no entry.
            if self.session.bridge.has_dataset_note()
                && ui
                    .button("About this dataset")
                    .on_hover_text("What the loaded dataset is, and what is unique about it.")
                    .clicked()
            {
                self.session.state.show_dataset_note = true;
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
            let _submission = self.session.bridge.load_file(name, bytes.to_vec(), format);
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

    /// The Cancel and Restart Python buttons beside the status line, when the
    /// active job or the runtime is in a state the user can act on.
    fn show_recovery_actions(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &RuntimeState,
        active: Option<(JobHandle, &JobRecord)>,
    ) {
        // A failed runtime no longer recreates workers on its own (a persistent
        // boot failure would loop forever), so recovery needs a button here.
        if matches!(runtime, RuntimeState::Failed(_)) && ui.small_button("Restart Python").clicked()
        {
            self.session.bridge.restart_runtime();
        }
        if matches!(runtime, RuntimeState::Ready)
            && let Some((handle, record)) = active
        {
            let queued = matches!(record.state(), JobState::Queued);
            let cancellable = queued
                || (record.operation().activity().cancellation
                    != CancellationCapability::NotSupported
                    && !matches!(record.state(), JobState::Cancelling { .. }));
            if cancellable && ui.small_button("Cancel").clicked() {
                let _disposition = self.session.bridge.cancel(handle);
            }
            // Restarting is the recovery of last resort, so it appears only for
            // a cancellation the worker stopped honoring, or a non-cancellable
            // rpc running long enough to look wedged — that one has no Cancel
            // button and would otherwise hold the job lane with no way out
            // short of a page reload.
            let stuck = match record.state() {
                JobState::Cancelling { since_s, .. } => {
                    js_sys::Date::now() / 1000.0 - since_s > 5.0
                }
                JobState::Running { since_s, .. } => {
                    record.operation().activity().cancellation
                        == CancellationCapability::NotSupported
                        && js_sys::Date::now() / 1000.0 - since_s > 30.0
                }
                _ => false,
            };
            if stuck && ui.small_button("Restart Python").clicked() {
                self.session.bridge.restart_runtime();
            }
        }
    }

    /// What the app is doing right now: the status line, the picking badge, and
    /// the time under the pointer.
    fn show_activity(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, total_tracks: usize) {
        // The status line is the one place the app tells you that something
        // finished, failed, or is still running, and none of it is tied to a
        // widget you would be focused on — so it is announced as it changes.
        let (runtime, jobs) = self.session.bridge.activity_snapshot();
        let jobs: &[(JobHandle, JobRecord)] = &jobs;
        let now_s = js_sys::Date::now() / 1000.0;
        if let Some(delay) = next_activity_reveal_delay(jobs, now_s) {
            ctx.request_repaint_after(delay);
        }
        let active = active_job(jobs, now_s);
        let (status, urgency) = show_primary_activity(ui, &runtime, jobs, active, total_tracks);
        self.show_recovery_actions(ui, &runtime, active);
        crate::a11y::announce_changes(ctx, &status, urgency);
        if let Some(error) = self.session.bridge.diagnostic() {
            ui.separator();
            let label = ui.colored_label(
                egui::Color32::from_rgb(0xC0, 0x60, 0x20),
                format!("⚠ {error}"),
            );
            let label = match error.traceback() {
                Some(traceback) => label.on_hover_text(traceback),
                None => label,
            };
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Urgent);
        }
        // Processing failures the view survived (a filter or restitution that
        // threw, drawn unprocessed). Snuffler lets these exceptions reach the
        // user; here the bridge sends them with the view they affected, and the
        // status line is where the user is told rather than the worker console.
        let notices = self
            .session
            .bridge
            .view()
            .map(|view| view.meta.notices.clone())
            .unwrap_or_default();
        if let Some(first) = notices.first() {
            ui.separator();
            let extra = notices.len().saturating_sub(1);
            let text = if extra > 0 {
                format!("⚠ {first} (+{extra} more)")
            } else {
                format!("⚠ {first}")
            };
            let label = ui.colored_label(egui::Color32::from_rgb(0xC0, 0x80, 0x20), text);
            let label = if extra > 0 {
                label.on_hover_text(notices.join("\n"))
            } else {
                label
            };
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
        // Qt's passband_check posts this to the window's permanent status area
        // (pile_viewer.py:4023) — not to the controls panel, which `c` hides —
        // and says what the app did about it: the highpass is dropped from
        // processing while the corners stay inverted.
        if let (Some(hp), Some(lp)) = (self.session.state.highpass, self.session.state.lowpass)
            && hp >= lp
        {
            ui.separator();
            let label = ui.colored_label(
                egui::Color32::from_rgb(0xCF, 0x8E, 0x00),
                "⚠ Corner frequency of highpass greater than corner frequency of lowpass. \
                 Highpass deactivated.",
            );
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
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
        // The hovered marker's metadata, as snuffler posts `hoover_message()` to
        // its status bar (pile_viewer.py:2200) — an event's origin fields or a
        // pick's polarity, readable without opening the marker table.
        let hover_msg = self
            .session
            .state
            .hovered_marker
            .and_then(|id| self.session.state.markers.by_id(id))
            .map(snuffler_web_core::markers::Marker::hover_message)
            .filter(|msg| !msg.is_empty());
        if let Some(msg) = hover_msg {
            ui.separator();
            let label = ui.label(msg);
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
        self.show_marker_hit_overload(ui, ctx);
        self.show_cursor_time(ui, ctx);
    }

    fn show_marker_hit_overload(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(count) = self.session.state.marker_hit_overload else {
            return;
        };
        ui.separator();
        let label = ui.label(format!(
            "{count} marker endpoints under the pointer; zoom in to select one"
        ));
        crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
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
        let deltatmin = self.session.scene_deltatmin();
        if self.session.state.show_controls {
            let deltat_range = self.session.scene_deltat_range();
            egui::Panel::bottom("controls").show_inside(ui, |ui| {
                controls::show(
                    ui,
                    &mut self.session.state,
                    total_tracks.max(1),
                    deltat_range,
                );
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
                    deltatmin,
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
        if let Some(job) = self.svg_prepare_job.take() {
            let theme = ctx.theme();
            // Qt's SVG export is a deliberate print canvas, not a window grab:
            // 842x595 (A4 landscape in points) with a 2.5 % margin framed into
            // the viewBox (`savesvg`, pile_viewer.py:2884).
            let size = egui::vec2(842.0, 595.0);
            let margin = size.max_elem() * 0.025;
            let svg = self.session.bridge.view().map(|view| {
                waveform::render_svg(ctx, &self.session.state, &view, size, theme, margin)
            });
            if let Some(svg) = svg {
                self.session.bridge.finish_local(job, Ok(()));
                let _submission = self
                    .session
                    .bridge
                    .save_image_svg(svg, export_filename("svg"));
            } else {
                self.session.bridge.finish_local(
                    job,
                    Err(BridgeError::browser(
                        "rendering an SVG",
                        "no waveform view is available",
                    )),
                );
            }
        }
        if std::mem::take(&mut self.svg_request) && self.svg_prepare_job.is_none() {
            let submission = self
                .session
                .bridge
                .begin_local(Operation::ExportSvg, Some("waveform view".to_owned()));
            if let Submission::Accepted(job) = submission {
                self.svg_prepare_job = Some(job);
                ctx.request_repaint();
            }
        }

        // Poll BEFORE consuming a new request: a rasterization that already
        // finished must free its slot first, or a click landing in that gap
        // would be rejected by the one-export-at-a-time guard for no reason.
        #[cfg(all(feature = "svg-raster", feature = "threads"))]
        self.poll_png_export(ctx);
        #[cfg(feature = "svg-raster")]
        if let Some(job) = self.png_prepare_job.take() {
            self.start_png_export(ctx, job);
        }
        if std::mem::take(&mut self.screenshot_request) {
            #[cfg(feature = "svg-raster")]
            {
                #[cfg(feature = "threads")]
                let rasterizing = self.export_rx.is_some();
                #[cfg(not(feature = "threads"))]
                let rasterizing = false;
                // One PNG export at a time: a second start while the first is
                // preparing or rasterizing would overwrite `export_rx` and leak
                // the displaced ledger job as permanently running.
                if self.png_prepare_job.is_none() && !rasterizing {
                    let submission = self
                        .session
                        .bridge
                        .begin_local(Operation::ExportPng, Some("waveform view".to_owned()));
                    if let Submission::Accepted(job) = submission {
                        self.png_prepare_job = Some(job);
                        ctx.request_repaint();
                    }
                }
            }
            #[cfg(not(feature = "svg-raster"))]
            {
                self.screenshot_crop = self.last_waveform_rect;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_awaiting = true;
            }
        }
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
            let _submission = self
                .session
                .bridge
                .save_image_png(png, export_filename("png"));
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
        // The pile's Nyquist, for Qt's corner-range narrowing; 0 deltat = no data.
        let deltatmin = self.session.scene_deltatmin();
        let nyquist = (deltatmin > 0.0).then(|| 0.5 / deltatmin);
        if let Some(action) = crate::snufflings::show_panel(
            ctx,
            &mut self.snuffling_panels,
            &mut self.session.snufflings,
            self.session.state.highpass,
            self.session.state.lowpass,
            nyquist,
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
            SnufflingAction::Run(name) => {
                let _submission = self.session.bridge.run_snuffling(name.into(), params);
            }
            SnufflingAction::ReloadSnufflings => {
                self.session.state.reload_snufflings_requested = true;
            }
            SnufflingAction::GeofonPreset(minmag) => {
                // The visible time range, as Qt's presets query it. FDSN wants
                // ISO stamps; `format_full` writes a space, so swap in the 'T'.
                let iso =
                    |t: f64| snuffler_web_core::timeaxis::format_full(t).replacen(' ', "T", 1);
                let _submission =
                    self.session
                        .bridge
                        .catalog_search(snuffler_web_core::bridge::CatalogRequest {
                            datacenter: "geofon".to_owned(),
                            start: iso(self.session.state.tmin),
                            end: iso(self.session.state.tmax),
                            minmag,
                            with_mt: self.session.state.cat_with_mt,
                            marker_budget: Some(
                                snuffler_web_core::marker_store::MarkerBudget::standard().into(),
                            ),
                        });
            }
            SnufflingAction::Rms => {
                let _submission = self.session.bridge.rms_traces(params);
            }
            SnufflingAction::CakePlotModel => {
                self.session.snufflings.cake.open_model_plot();
                self.figures.invalidate("cake_model");
            }
            SnufflingAction::CakePlotRays => {
                self.session.snufflings.cake.open_rays_plot();
                self.figures.invalidate("cake_rays");
            }
            SnufflingAction::AddCakeModel => {
                let _submission = self.session.bridge.add_cake_model();
            }
            SnufflingAction::LoadGfStoreUrl => {
                let url = self.session.snufflings.seismosizer.load_url();
                if !url.trim().is_empty() {
                    let _submission = self.session.bridge.load_gf_store_url(url);
                }
            }
            #[cfg(feature = "gf")]
            SnufflingAction::AddGfStore => {
                let _submission = self.session.bridge.add_gf_store();
            }
        }
    }

    fn show_auxiliary_windows(&mut self, ctx: &egui::Context) {
        crate::view::help::show(ctx, &mut self.session.state);
        crate::view::help::show_snuffling_help(ctx, &mut self.session.state);
        // Only crosses the JS seam while the window is actually open.
        let runtime = self
            .session
            .state
            .show_about
            .then(|| self.session.bridge.runtime_info())
            .flatten();
        crate::view::about::show(ctx, &mut self.session.state, runtime.as_ref());
        // Cloned out of the bridge only while the window is open, like the
        // runtime info above.
        if self.session.state.show_dataset_note
            && let Some(note) = self.session.bridge.dataset_note()
        {
            crate::view::dataset_note::show(ctx, &mut self.session.state, &note);
        }
        let downloads = crate::view::forms::download(ctx, &mut self.session.state);
        if let Some(request) = downloads.waveforms {
            let _submission = self.session.bridge.download_waveforms(request);
        }
        if let Some(request) = downloads.stations {
            let _submission = self.session.bridge.fetch_stations(request);
        }
        if let Some(request) = downloads.responses {
            let _submission = self.session.bridge.fetch_responses(request);
        }
        if downloads.save {
            let _submission = self.session.bridge.save_waveforms_and_stations();
        }

        let seedlink = crate::view::forms::seedlink(
            ctx,
            &mut self.session.state,
            self.session.bridge.seedlink_active(),
        );
        match seedlink {
            Some(crate::view::forms::SeedlinkIntent::Start(params)) => {
                self.session.start_seedlink(params);
            }
            Some(crate::view::forms::SeedlinkIntent::Stop) => {
                self.session.bridge.stop_seedlink();
            }
            Some(crate::view::forms::SeedlinkIntent::SaveBuffer) => {
                let _submission = self.session.bridge.save_waveforms();
            }
            None => {}
        }
        if let Some(request) = crate::view::forms::catalog(ctx, &mut self.session.state) {
            let _submission = self.session.bridge.catalog_search(request);
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
    for item in &example.characteristics {
        text.push_str("\n• ");
        text.push_str(item);
    }
    text
}

fn active_job(jobs: &[(JobHandle, JobRecord)], now_s: f64) -> Option<(JobHandle, &JobRecord)> {
    jobs.iter()
        .filter(|(_, record)| {
            !record.state().is_terminal() && record.feedback_delay(now_s).is_none()
        })
        .max_by_key(|(handle, record)| {
            (
                !record.operation().activity().secondary_feedback,
                matches!(
                    record.state(),
                    JobState::Running { .. } | JobState::Dispatching | JobState::Cancelling { .. }
                ),
                handle.id().get(),
            )
        })
        .map(|(handle, record)| (*handle, record))
}

fn next_activity_reveal_delay(
    jobs: &[(JobHandle, JobRecord)],
    now_s: f64,
) -> Option<std::time::Duration> {
    jobs.iter()
        .filter(|(_, record)| !record.state().is_terminal())
        .filter_map(|(_, record)| record.feedback_delay(now_s))
        .min()
}

// One pass: an outcome is resolved by any later submission of the same
// operation, so the newest record per operation decides.
fn newest_per_operation(jobs: &[(JobHandle, JobRecord)]) -> Vec<(u64, &JobRecord)> {
    let mut newest: std::collections::HashMap<Operation, (u64, &JobRecord)> =
        std::collections::HashMap::new();
    for (handle, record) in jobs {
        let id = handle.id().get();
        let entry = newest.entry(record.operation()).or_insert((id, record));
        if id > entry.0 {
            *entry = (id, record);
        }
    }
    newest.into_values().collect()
}

fn unresolved_failure(jobs: &[(JobHandle, JobRecord)]) -> Option<&JobError> {
    newest_per_operation(jobs)
        .into_iter()
        .filter_map(|(id, record)| match record.state() {
            JobState::Done(Outcome::Failed(error)) => Some((id, error)),
            _ => None,
        })
        .max_by_key(|(id, _)| *id)
        .map(|(_, error)| error)
}

/// A partial or lost terminal outcome no later submission superseded. These are
/// not failures — committed work remains installed — but staying silent about
/// them would leave the user with 3 of 400 files and a status line that says
/// only "Ready".
fn unresolved_stop_notice(jobs: &[(JobHandle, JobRecord)]) -> Option<String> {
    newest_per_operation(jobs)
        .into_iter()
        .filter_map(|(id, record)| {
            let label = record.operation().activity().label;
            match record.state() {
                // `skipped` is a lower bound: a lazy directory iteration only
                // knows one more page existed, not how many entries it held.
                JobState::Done(Outcome::Stopped { committed, skipped }) => Some((
                    id,
                    format!("{label} stopped: {committed} committed, ≥{skipped} skipped"),
                )),
                JobState::Done(Outcome::Lost(_)) => {
                    Some((id, format!("{label} was interrupted by a Python restart")))
                }
                _ => None,
            }
        })
        .max_by_key(|(id, _)| *id)
        .map(|(_, text)| text)
}

fn active_job_text(record: &JobRecord, queued: usize) -> String {
    let mut text = record.operation().activity().label.to_owned();
    if let Some(detail) = record.detail() {
        text.push_str(" · ");
        text.push_str(detail);
    }
    match record.state() {
        JobState::Running {
            progress: Progress::Fraction {
                fraction, phase, ..
            },
            ..
        } => {
            text = format!(
                "{phase} · {:.0}%",
                (fraction * 100.0).round().clamp(0.0, 100.0)
            );
        }
        JobState::Running {
            progress: Progress::Count { done, total, phase },
            ..
        } => {
            text = total.map_or_else(
                || format!("{phase} · {done}"),
                |total| format!("{phase} · {done}/{total}"),
            );
        }
        JobState::Running {
            since_s,
            progress: Progress::Indeterminate,
            ..
        } => {
            let elapsed = (js_sys::Date::now() / 1000.0 - since_s).max(0.0);
            if elapsed >= 5.0 {
                text = format!("{text} · still working ({elapsed:.0}s)");
            }
        }
        _ => {}
    }
    if matches!(record.state(), JobState::Cancelling { .. }) {
        text = format!("Cancelling {text}…");
    }
    if queued > 0 {
        let _ = write!(text, " · {queued} queued");
    }
    text
}

fn show_primary_activity(
    ui: &mut egui::Ui,
    runtime: &RuntimeState,
    jobs: &[(JobHandle, JobRecord)],
    active: Option<(JobHandle, &JobRecord)>,
    total_tracks: usize,
) -> (egui::Response, crate::a11y::Urgency) {
    match runtime {
        RuntimeState::Booting => {
            ui.spinner();
            (
                ui.label("Starting Python (Pyodide)…"),
                crate::a11y::Urgency::Polite,
            )
        }
        RuntimeState::Restarting => {
            ui.spinner();
            (
                ui.label("Restarting Python (Pyodide)…"),
                crate::a11y::Urgency::Urgent,
            )
        }
        RuntimeState::Failed(error) => {
            let label = ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
            let label = error
                .traceback()
                .map_or(label.clone(), |traceback| label.on_hover_text(traceback));
            (label, crate::a11y::Urgency::Urgent)
        }
        RuntimeState::Ready => {
            if let Some((_, record)) = active {
                ui.spinner();
                let queued = jobs
                    .iter()
                    .filter(|(_, record)| matches!(record.state(), JobState::Queued))
                    .count();
                return (
                    ui.label(active_job_text(record, queued)),
                    crate::a11y::Urgency::Polite,
                );
            }
            if let Some(error) = unresolved_failure(jobs) {
                let label = ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
                let label = match error {
                    JobError::Bridge(error) => error
                        .traceback()
                        .map_or(label.clone(), |traceback| label.on_hover_text(traceback)),
                    JobError::Rejected(_) | JobError::RuntimeLost => label,
                };
                return (label, crate::a11y::Urgency::Urgent);
            }
            if let Some(notice) = unresolved_stop_notice(jobs) {
                return (
                    ui.colored_label(egui::Color32::from_rgb(0xC0, 0x60, 0x20), notice),
                    crate::a11y::Urgency::Urgent,
                );
            }
            (
                ui.label(format!("Ready · {total_tracks} traces")),
                crate::a11y::Urgency::Polite,
            )
        }
    }
}

/// A timestamped export name (`snuffler-20260816-142530.png`): a fixed name
/// collides into `snuffler (1).png` on the second export, and the stamp says
/// which session a file in the downloads folder came from.
fn export_filename(ext: &str) -> String {
    let now_s = js_sys::Date::now() / 1000.0;
    let stamp: String = snuffler_web_core::timeaxis::format_full(now_s)
        .chars()
        .take_while(|&c| c != '.')
        .filter(|c| c.is_ascii_digit() || *c == ' ')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    format!("snuffler-{stamp}.{ext}")
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
        if self.session.take_annotation_cache_reset() {
            self.clear_annotation_caches();
        }
        self.process_marker_export(&ctx);
        self.consume_keyboard_requests();
        self.load_dropped_files(&ctx);
        self.apply_pending_view();
        // A dataset that describes itself introduces itself, once.
        if self.session.bridge.take_unseen_dataset_note() {
            self.session.state.show_dataset_note = true;
        }

        let scene = self.session.bridge.scene();
        let total_tracks = scene
            .as_ref()
            .map_or(0, |scene| usize::try_from(scene.track_count).unwrap_or(0));
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
        self.publish_readiness();
        crate::persist::save_if_changed(
            &self.session.state,
            self.chosen_theme_preference(&ctx),
            &mut self.settings_saved,
        );
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}

/// Toggle browser fullscreen on the document element.
///
/// The DOM call, not eframe's `ViewportCommand::Fullscreen`: the web backend
/// drops that command in its unhandled arm, which is how the `f` key managed to
/// be documented, bound and inert at the same time. `request_fullscreen` needs a
/// user gesture, which the key press and the menu click both are.
#[cfg(target_arch = "wasm32")]
fn toggle_fullscreen() {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if document.fullscreen_element().is_some() {
        document.exit_fullscreen();
    } else if let Some(root) = document.document_element() {
        let _ = root.request_fullscreen();
    }
}

/// Host builds have no browser viewport; the request is acknowledged and dropped.
#[cfg(not(target_arch = "wasm32"))]
fn toggle_fullscreen() {}
