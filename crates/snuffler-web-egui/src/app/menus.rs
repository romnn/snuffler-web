//! The menu bar, shared with the canvas context menu.

use snuffler_web_core::bridge::ExampleDataset;
use snuffler_web_core::state::{
    ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength,
};
use snuffler_web_core::theme::WiggleTheme;

use super::App;

impl App {
    /// The full menu set, shared by the top menubar and the canvas right-click
    /// context menu so both stay in sync (Qt pops the same menu object for both).
    pub(super) fn main_menus(&mut self, ui: &mut egui::Ui) {
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
                self.png_request = true;
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
    /// Nothing here ships in the app.
    /// The index is fetched when this submenu is first opened and a dataset's bytes only when
    /// someone picks it, so a menu full of examples costs a page load nothing — which is the
    /// point, since the people who most need example data are the ones who have none of their
    /// own.
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
                for length in VisibleLength::ALL {
                    // A target sample count, not a time cap: picking one never moves the window.
                    // It feeds `min_deltat_allow` in the bridge, exactly as snuffler's radio group
                    // feeds `prepare_cutout2` — except for Adaptive, which is this frontend's own
                    // and feeds it nothing.
                    ui.selectable_value(
                        &mut self.session.state.visible_length,
                        length,
                        length.label(),
                    )
                    .on_hover_text(length.hint());
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
            for entry in WiggleTheme::ALL.into_iter().filter(|entry| entry.enabled) {
                let theme = entry.theme;
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
            for mode in [
                SortMode::Name,
                SortMode::Distance,
                SortMode::Azimuth,
                SortMode::AzimuthBlocks,
                SortMode::Backazimuth,
            ] {
                let response =
                    ui.selectable_value(&mut self.session.state.sort, mode, mode.label());
                if mode.needs_coords() {
                    response.on_hover_text(
                        "Needs station coordinates + an active event; \
                         falls back to name order without them.",
                    );
                }
            }
            ui.separator();
            ui.label("Subsort / grouping");
            for subsort in [
                Subsort::Nslc,
                Subsort::Nscl,
                Subsort::Sncl,
                Subsort::Lnsc,
                Subsort::Cnsl,
                Subsort::NscGroupL,
                Subsort::SncGroupL,
            ] {
                ui.selectable_value(&mut self.session.state.subsort, subsort, subsort.label());
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
            // Only for a dataset that came with a description — the shipped examples, and any
            // bundle whose author wrote one.
            // Waveforms opened from disk have nothing to say about themselves, and an entry that
            // opened an empty window would be worse than no entry.
            if self.session.bridge.has_dataset_note()
                && ui
                    .button("About the dataset")
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
/// Only the blurb's opening paragraph — enough to choose by.
/// The whole thing is shown after the load, in a window that has room for it.
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
