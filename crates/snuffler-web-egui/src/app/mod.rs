//! The egui application: the frame loop, the windows it owns, and the glue
//! between the session and the views.

mod export;
mod frame_stats;
mod look;
mod menus;
mod snufflings;
mod status;
mod view_budget;
mod windows;

use self::export::MarkerExportScope;
use self::look::{configure_look, pin_theme};
use crate::view::waveform::WaterfallCache;
use crate::view::{controls, waveform};
use snuffler_web_core::activity::JobHandle;
use snuffler_web_core::bridge::Repaint;
use snuffler_web_core::session::Session;
use snuffler_web_core::state::{SortMode, Subsort};
use snuffler_web_core::theme::WiggleTheme;

/// The dataset the app opens on when the URL asks for nothing in particular.
const BOOT_EXAMPLE: &str = "sumatra-2004-12-26-m9.1-surface-waves";

/// The egui application: UI state plus the bridge to pyrocko (via Pyodide).
pub struct App {
    session: Session,
    snuffling_panels: crate::snufflings::SnufflingPanels,
    waterfall_cache: WaterfallCache,
    figures: crate::view::figure::FigureWindows,
    marker_table: crate::view::marker_table::MarkerTable,
    map: crate::view::map::MapState,
    /// Waveform central-panel rect from the current frame; the crop for PNG export.
    last_waveform_rect: egui::Rect,
    /// Last time under the pointer, retained when it leaves the waveform.
    last_cursor_time: Option<f64>,
    /// A "Save image (PNG)" menu click, handled after layout once the rect is known.
    png_request: bool,
    /// A "Save image (SVG)" menu click, handled after layout once the view is known.
    svg_request: bool,
    /// The ledger job of an SVG export waiting for the bridge to prepare it.
    svg_prepare_job: Option<JobHandle>,
    /// A marker-file export waiting for the bridge to prepare it.
    preparing_marker_export: Option<(JobHandle, MarkerExportScope)>,
    /// The in-flight off-thread marker-file export.
    #[cfg(feature = "threads")]
    pending_marker_export: Option<export::MarkerExport>,
    /// The in-flight off-thread PNG export, which receives the encoded bytes from
    /// the rayon pool.
    ///
    /// Only the threaded build runs the export off-thread; otherwise it rasterizes
    /// synchronously.
    #[cfg(feature = "threads")]
    pending_png_export: Option<export::PngExport>,
    /// The ledger job of a PNG export waiting for the bridge to prepare it.
    png_prepare_job: Option<JobHandle>,
    /// Smoothed egui CPU time per frame, in milliseconds.
    frame_stats_ms: Option<f64>,
    /// The frame rate the control strip is currently showing, and when it last changed.
    frame_stats_shown_fps: Option<u32>,
    frame_stats_shown_at_s: f64,
    /// The light/dark preference to restore when leaving a theme that pins the
    /// canvas.
    ///
    /// Snuffler Qt takes the *whole* frontend light, not just the waveform: a white
    /// canvas inside dark chrome is neither theme, it just looks broken.
    theme_before_pinning: Option<egui::ThemePreference>,
    /// The persisted-settings snapshot last written to `localStorage`, so a
    /// frame that changed none of them writes nothing.
    settings_saved: Option<crate::persist::Snapshot>,
    /// Last readiness value published on the canvas, so unchanged frames do no DOM work.
    published_ready: Option<bool>,
}

impl App {
    /// Build the app, configure egui styling, and kick off loading the example.
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
        configure_look(&cc.egui_ctx);
        // The restored wiggle theme decides what the light/dark preference is
        // allowed to be: pinning it here, before the first frame, is what keeps a
        // white-canvas theme from flashing through dark chrome on the way in.
        let theme_before_pinning = pin_theme(
            &cc.egui_ctx,
            session.state.wiggle_theme,
            restored.theme_preference,
        );
        // What the app opens on.
        // A `?url=` (with optional `?format=`) or a `?example=<slug>` link is what it was opened
        // *for*, so it takes that place rather than replacing something a second later; `?demo=1`
        // asks for the synthetic pile the parity harness needs; and failing all three, a real
        // earthquake.
        // Sumatra 2004 because it is the one that reads as seismology at a glance — surface waves
        // circling the planet twice, visible without touching a single control.
        // All are issued before the worker has booted; the transport queues them.
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
            waterfall_cache: WaterfallCache::default(),
            figures: crate::view::figure::FigureWindows::default(),
            marker_table: crate::view::marker_table::MarkerTable::default(),
            map: crate::view::map::MapState::default(),
            last_waveform_rect: egui::Rect::ZERO,
            last_cursor_time: None,
            png_request: false,
            svg_request: false,
            svg_prepare_job: None,
            preparing_marker_export: None,
            #[cfg(feature = "threads")]
            pending_marker_export: None,
            #[cfg(feature = "threads")]
            pending_png_export: None,
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

    fn clear_annotation_caches(&mut self) {
        self.marker_table = crate::view::marker_table::MarkerTable::default();
        self.figures.clear_beachball();
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
            .unwrap_or_else(|| ctx.options(|options| options.theme_preference))
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
    /// A dataset shipped as an example exists to show something, and the setting that shows it is
    /// part of the example — co-located sensors say nothing until the subsort groups them onto
    /// one row.
    /// Unknown values are ignored, so a bundle built against a later version cannot wedge this
    /// one.
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
            let format = guess_format(&name);
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
            let fps = self.frame_stats_shown_fps;
            let previous_theme = self.session.state.wiggle_theme;
            egui::Panel::bottom("controls").show_inside(ui, |ui| {
                controls::show(
                    ui,
                    &mut self.session.state,
                    total_tracks.max(1),
                    deltat_range,
                    |ui, state| Self::show_build_readout(ui, fps, state),
                );
            });
            if self.session.state.wiggle_theme != previous_theme {
                self.apply_theme_pinning(ui.ctx(), self.session.state.wiggle_theme);
            }
        }
        self.show_marker_info_panel(ui);
        let central = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                let epoch = self.session.bridge.view_epoch();
                let view = self.session.bridge.view();
                waveform::show(
                    ui,
                    &mut self.session.state,
                    view.as_deref(),
                    &mut self.waterfall_cache,
                    epoch,
                    scene_range,
                    deltatmin,
                )
            });
        self.last_waveform_rect = central.response.rect;
        central.inner.context_menu(|ui| self.main_menus(ui));
    }
}

/// The pyrocko format name for a dropped file, from its extension.
fn guess_format(name: &str) -> String {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("mseed")
        || extension.eq_ignore_ascii_case("msd")
        || extension.eq_ignore_ascii_case("seed")
    {
        "mseed".into()
    } else if extension.eq_ignore_ascii_case("sac") {
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
