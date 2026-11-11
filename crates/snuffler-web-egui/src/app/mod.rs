//! The egui application: the frame loop, the windows it owns, and the glue
//! between the session and the views.

mod export;
mod menus;
mod status;

use std::cell::Cell;

use egui::FontId;
use wasm_bindgen::prelude::wasm_bindgen;

use self::export::MarkerExportScope;
use crate::snufflings::Action as SnufflingAction;
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};
use snuffler_web_core::activity::JobHandle;
use snuffler_web_core::bridge::Repaint;
use snuffler_web_core::session::Session;
use snuffler_web_core::state::{SortMode, Subsort, ViewMode};
use snuffler_web_core::theme::WiggleTheme;

/// The dataset the app opens on when the URL asks for nothing in particular.
const BOOT_EXAMPLE: &str = "sumatra-2004-12-26-m9.1-surface-waves";

/// How often the displayed frame rate is allowed to change. The measurement is
/// smoothed every frame; only the number on screen is held still, because one
/// that rewrites itself at 60 Hz is unreadable.
const FRAME_STATS_REFRESH_INTERVAL_S: f64 = 0.25;
const FRAME_STATS_EMA_ALPHA: f64 = 0.2;

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

/// The egui font family the waveform canvas draws every label in — bold, as
/// snuffler's canvas is. Registered in [`configure_look`].
pub const CANVAS_FAMILY: &str = "canvas";

// `npx` is a per-trace bin budget for Python's min/max envelope.
// The display-scale bin budget keeps transport and renderer work bounded when
// zoomed far out.
//
// One bin per physical pixel is what makes the envelope invisible: the two
// extremes of a bin land in the pixel column they came from, so an Adaptive
// window draws the same trace a fixed visible length draws from raw samples.
// The total-points cap is a backstop for a very tall stack, not the usual
// governor: set below what a screenful of rows at full width costs, it would
// coarsen every trace to a fraction of the canvas's resolution.
const MIN_VIEW_SAMPLE_BINS: f32 = 64.0;
const MAX_VIEW_SAMPLE_BINS_PER_TRACE: f32 = 8192.0;
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
    png_request: bool,
    /// A "Save image (SVG)" menu click, handled after layout once the view is known.
    svg_request: bool,
    svg_prepare_job: Option<JobHandle>,
    marker_prepare: Option<(JobHandle, MarkerExportScope)>,
    #[cfg(feature = "threads")]
    marker_export_rx: Option<export::MarkerExport>,
    /// In-flight off-thread PNG export: receives the encoded bytes from the rayon
    /// pool, paired with the download filename. Only the threaded build runs the
    /// export off-thread; otherwise it rasterizes synchronously (no receiver).
    #[cfg(feature = "threads")]
    export_rx: Option<export::PngExport>,
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
            "../../assets/fonts/Ubuntu-Regular.ttf"
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
            "../../assets/fonts/JetBrainsMono-Regular.ttf"
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
            "../../assets/fonts/Ubuntu-Bold.ttf"
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
    // `set_global_style` writes only whichever is current, so the sizes below would
    // vanish on the first light/dark switch.
    ctx.all_styles_mut(|style| {
        // Some 18 % over egui's defaults, which are tuned for dense tool windows
        // rather than for an app somebody reads for hours.
        // The canvas is *not* scaled with this: its labels are held to snuffler's own
        // metrics (`LABEL_PT`, `AXIS_PT` in the render crate).
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
            png_request: false,
            svg_request: false,
            svg_prepare_job: None,
            marker_prepare: None,
            #[cfg(feature = "threads")]
            marker_export_rx: None,
            #[cfg(feature = "threads")]
            export_rx: None,
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
        let (emitted_per_bin, visible_cap) = match self.session.state.view_mode {
            ViewMode::Wiggle => (WIGGLE_POINTS_PER_BIN, MAX_VISIBLE_WIGGLE_POINTS),
            ViewMode::Waterfall => (WATERFALL_CELLS_PER_BIN, MAX_VISIBLE_WATERFALL_CELLS),
        };
        let per_trace_visible_cap = visible_cap / (visible_tracks * emitted_per_bin);
        let bins = physical_width
            .min(MAX_VIEW_SAMPLE_BINS_PER_TRACE)
            .min(per_trace_visible_cap.max(MIN_VIEW_SAMPLE_BINS))
            .max(MIN_VIEW_SAMPLE_BINS);
        bins.ceil() as u32
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
            self.png_request = true;
        }
        if std::mem::take(&mut self.session.state.fullscreen_toggle_requested) {
            toggle_fullscreen();
        }
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
}

/// Toggle browser fullscreen on the document element.
///
/// The DOM call, not eframe's `ViewportCommand::Fullscreen`: the web backend
/// drops that command in its unhandled arm.
/// `request_fullscreen` needs a user gesture, which the key press and the menu
/// click both are.
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
