//! Frontend-independent application state for the viewer.
//!
//! Field/enum names and defaults mirror pyrocko's snuffler (`pile_viewer.py` +
//! `config.py`) so every frontend exposes the same settings.
//!
//! - this module: [`AppState`] itself;
//! - `settings`: the processing and display choices pyrocko and the renderer read;
//! - `forms`: what the download, catalog and live dialogs hold between openings;
//! - `interaction`: in-progress drags and picks, and requests the session drains.

use crate::command::CommandLine;
use crate::markers::{MarkerId, MarkerStore};
use crate::theme::{Theme, WiggleTheme};

mod forms;
mod interaction;
mod settings;

pub use forms::{CatalogForm, DownloadForm, SeedlinkForm};
pub use interaction::{ActiveEventRequest, PickingState, ViewDrag};
pub use settings::{
    ColorScale, Colormap, ScalingBase, ScalingGroup, SortMode, Subsort, ViewMode, VisibleLength,
    WaterfallSettings,
};

/// Number of visible tracks on first load, and the
/// reset target for the "=" key.
///
/// 24, because that is what the `snuffler` application passes its viewer
/// (`snuffler.py:262`).
/// Upstream is internally inconsistent here (the library entry point defaults to 12 and
/// the widget to 20), but the shell is the program this port is measured against.
pub const DEFAULT_NTRACKS_SHOWN: usize = 24;

/// Marker details and their color identity for the information strip.
#[derive(Debug, PartialEq, Eq)]
pub struct MarkerInfo {
    /// Metadata for one marker, or a count for multiple selected markers.
    pub text: String,
    /// The displayed marker's kind, or `None` when describing a multiple selection.
    pub kind: Option<u8>,
}

/// Complete frontend-independent state edited and rendered by a viewer frontend.
pub struct AppState {
    /// Lower edge of the visible time window, in epoch seconds.
    pub tmin: f64,
    /// Upper edge of the visible time window, in epoch seconds.
    pub tmax: f64,

    /// High-pass cutoff in hertz, or `None` when disabled.
    pub highpass: Option<f64>,
    /// Low-pass cutoff in hertz, or `None` when disabled.
    pub lowpass: Option<f64>,
    /// Last high-pass slider value in hertz, kept while the filter is off so
    /// re-enabling it restores the value.
    pub last_highpass_hz: f64,
    /// Last low-pass slider value in hertz, kept while the filter is off.
    pub last_lowpass_hz: f64,
    /// Display gain applied after pyrocko scaling.
    pub gain: f32,
    /// Horizontal-component rotation in degrees.
    pub rotate_deg: f64,
    /// Editable high-pass field, including the disabled `off` value.
    pub highpass_text: String,
    /// Editable low-pass field, including the disabled `off` value.
    pub lowpass_text: String,
    /// Editable display-gain field.
    pub gain_text: String,
    /// Editable rotation field in degrees.
    pub rotate_text: String,

    /// Removes the mean from each trace in pyrocko.
    pub demean: bool,
    /// Repairs gaps before filtering in pyrocko.
    pub degap: bool,
    /// Permits pyrocko to reduce sample density for the requested pixel budget.
    pub allow_downsampling: bool,
    /// Uses FFT-domain filtering instead of the default time-domain path.
    pub fft_filtering: bool,
    /// Applies low-pass before high-pass when both filters are enabled.
    pub bandpass_lphp: bool,
    /// Deconvolves the instrument response when matching `StationXML` is loaded.
    pub remove_response: bool,
    /// Use station depth for distances (needs coords; inert without).
    pub distances_3d: bool,

    /// Tracks that share an amplitude range.
    pub scaling_group: ScalingGroup,
    /// Statistic used to derive amplitude ranges.
    pub scaling_base: ScalingBase,
    /// Keeps established scale ranges stable across view changes.
    pub fix_scale_ranges: bool,
    /// Primary track ordering.
    pub sort: SortMode,
    /// NSLC ordering within the primary sort.
    pub subsort: Subsort,

    /// Active waveform representation.
    pub view_mode: ViewMode,
    /// Active time-domain trace theme.
    pub wiggle_theme: WiggleTheme,
    /// The per-trace sample budget the visible-length control asks the bridge for.
    pub visible_length: VisibleLength,
    /// Whether trace extents have visible boxes: the user's choice if they made
    /// one, otherwise what the canvas defaults to.
    /// Read it through [`AppState::boxes_visible`], never directly.
    pub show_boxes: Option<bool>,
    /// Whether traces are clipped to their own rows.
    pub clip_traces: bool,
    /// Whether traces use NSLC-derived colors.
    ///
    /// `None` follows the theme's [`crate::theme::StrokePolicy`], which is what keeps
    /// the Snuffler Qt theme booting the way snuffler boots; `Some` is the user
    /// overriding that, honoured under every theme.
    /// Read it through [`AppState::color_traces`], never directly.
    pub show_color_traces: Option<bool>,
    /// Whether line tessellation uses feathered antialiasing.
    pub antialiasing: bool,
    /// Whether each row displays its numeric amplitude range.
    pub show_scale_ranges: bool,
    /// Whether each row displays its scale axis.
    pub show_scale_axes: bool,
    /// Whether each row displays its zero-amplitude line.
    pub show_zero_lines: bool,
    /// Whether a rule is drawn between rows, bands or no bands.
    ///
    /// `None` follows the theme: off under Snuffler Qt, which separates rows with
    /// nothing at all, and on under the web themes.
    /// `Some` is the user overriding that, and is honoured under every theme: parity is
    /// about what the port does by default, not about refusing to draw a rule somebody
    /// asked for.
    pub show_track_separators: Option<bool>,
    /// Whether the time ticks extend into a full-height grid across the tracks.
    ///
    /// Web-only, and off by default because snuffler has no such grid: `TimeAx.drawit`
    /// (`pile_viewer.py:539`) marks each tick with a 10 px stroke at the axis and draws
    /// nothing across the rows.
    /// Reading a long window is easier with one, so the port offers it as something the
    /// user switches on, not as a difference it imposes.
    pub show_time_grid: bool,
    /// How the waterfall view maps spectral power to colour.
    pub waterfall: WaterfallSettings,

    /// Snuffling text output (`MinMax` etc.).
    pub snuffling_text: String,
    /// Whether the snuffling text-output window is open.
    pub show_snuffling_text: bool,
    /// Whether the window showing what the loaded dataset says about itself is open.
    /// It opens itself once, collapsed, when a dataset that says something loads; the
    /// text lives in the bridge, with the scene it describes.
    pub show_dataset_note: bool,

    /// Desired visible track count, with `None` using [`DEFAULT_NTRACKS_SHOWN`].
    /// The layout clamps this to the loaded tracks without replacing the preference.
    pub tracks_per_page: Option<f32>,
    /// Whether the visible count and scroll position snap to whole tracks.
    pub snap_tracks: bool,
    /// Fractional index of the first visible track row.
    pub track_scroll: f32,
    /// Number of rows requested from pyrocko for the current layout.
    /// A cache the waveform layout recomputes each frame, so the view request
    /// fetches only on-screen tracks.
    pub ntracks_shown: usize,
    /// Index of the first row requested from pyrocko; recomputed like `ntracks_shown`.
    pub track_offset: usize,

    /// User, catalog, and snuffling annotations in insertion order.
    pub markers: MarkerStore,
    /// The controls panel's marker-kind picker highlight: the last kind applied through
    /// it.
    /// Fresh picks are always kind 0, as snuffler's are; nothing reads this as a default
    /// for new markers.
    pub marker_kind: u8,
    /// Marker kinds (0..=7) currently drawn and selectable; set by the `:markers`
    /// command (snuffler `pile_viewer.py` `markers` command).
    /// Compared against the raw kind so kind>=8 hides under `markers all`, like pyrocko.
    pub visible_marker_kinds: Vec<u8>,
    /// Auto-reload opened files as they change on disk (snuffler's "Watch Files").
    pub watch_files: bool,
    /// Fetch a wider, grid-snapped time window so pan/zoom reuses already-fetched
    /// data instead of re-requesting (snuffler's "Liberal Fetch Optimization").
    pub liberal_fetch: bool,

    /// Set by the `r` key; the session consumes it to reload the opened files.
    /// Keyboard handlers only touch `state`, never the bridge.
    pub reload_files_requested: bool,
    /// Set by Shift+R; the session consumes it to reload the Python snuffling code.
    pub reload_snufflings_requested: bool,
    /// Set by `e`/`E`; the session drains it to send a `SetActiveEvent` to the bridge.
    pub set_active_event_request: Option<ActiveEventRequest>,
    /// Event hashes whose final frontend event marker was deleted.
    /// The session drains them into Python's canonical registry before another
    /// scientific consumer can resolve the removed events.
    pub remove_event_hashes: Vec<String>,

    /// Active batch/interactive picking mode (Escape toggles it); `None` when not
    /// picking.
    /// Mirrors snuffler's picking mode: a floating pick follows the cursor,
    /// press-drag-release commits a marker, and batch mode re-arms a new pick inheriting
    /// kind/phase/polarity.
    pub picking: Option<PickingState>,
    /// Last pointer y during a shift-drag that adjusts phase-marker uncertainty
    /// (`Some` = a latched uncertainty drag is active); mirrors pyrocko's
    /// `set_uncertainty_y`/`_last_y`.
    pub uncertainty_drag_last_y: Option<f32>,
    /// Active mouse drag used for snuffler-style pan/zoom interaction.
    pub view_drag: Option<ViewDrag>,
    /// Accumulates wheel motion into Qt-style 120-unit notches.
    pub wheel_accumulator: f32,
    /// A bare `s` was pressed: the next key completes one of snuffler's
    /// two-stroke chords (`s i`/`s c`/`s s` scaling, `s n`/`s d` sorting).
    pub pending_s_chord: bool,
    /// The on-screen track row under the pointer, if any: what snuffler reads
    /// off `QCursor.pos()` when rows are too short for every label and only the
    /// hovered one is drawn (`pile_viewer.py:3530`).
    pub hovered_track_row: Option<usize>,
    /// The marker under the pointer, snuffler's per-marker `alerted` flag
    /// (`pile_viewer.py:2168`).
    /// The hovered marker brightens to its selected shade, its guides go dashed
    /// (grey when unselected), and its metadata is posted to the status line.
    pub hovered_marker: Option<MarkerId>,
    /// Number of marker endpoints under the pointer when exact hit-testing is overloaded.
    pub marker_hit_overload: Option<usize>,

    /// A pending `f`-key or View ▸ Fullscreen request, consumed by the app.
    /// The app calls the browser's Fullscreen API on the document element, the
    /// only path that works on the web: eframe's `ViewportCommand::Fullscreen`
    /// lands in an unhandled arm there.
    pub fullscreen_toggle_requested: bool,
    /// A pending Ctrl+O, consumed by the app (snuffler's File ▸ Open accelerator).
    pub open_files_requested: bool,
    /// A pending Ctrl+S / Ctrl+Shift+S, consumed by the app: `Some(true)` saves
    /// the selected markers, `Some(false)` all of them.
    pub save_markers_requested: Option<bool>,
    /// A pending Ctrl+E, consumed by the app (snuffler's Save-as-image accelerator).
    pub export_image_requested: bool,

    /// Whether the bottom processing-control panel is visible.
    pub show_controls: bool,
    /// Whether the "Snuffler controls" help window is open.
    pub show_help: bool,
    /// Whether the about window is open.
    pub show_about: bool,
    /// Whether the per-snuffling help window (Help ▸ Snuffling Help) is open.
    pub show_snuffling_help: bool,
    /// Whether the waveform-download window is open.
    pub show_download: bool,
    /// Whether the catalog-search window is open.
    pub show_catalog: bool,
    /// Whether the station/event map is open.
    pub show_map: bool,
    /// Whether the marker table is open.
    pub show_marker_editor: bool,
    /// Whether the live `SeedLink` window is open.
    pub show_live_seedlink: bool,
    /// The ':' command line: visibility, input, history, and last error.
    pub command: CommandLine,

    /// The waveform-download dialog's fields.
    pub download_form: DownloadForm,
    /// The catalog-search dialog's fields.
    pub catalog_form: CatalogForm,
    /// The live `SeedLink` dialog's fields.
    pub seedlink_form: SeedlinkForm,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tmin: 0.0,
            tmax: 1.0,
            highpass: None,
            lowpass: None,
            last_highpass_hz: 0.1,
            last_lowpass_hz: 10.0,
            gain: 1.0,
            rotate_deg: 0.0,
            highpass_text: "off".to_owned(),
            lowpass_text: "off".to_owned(),
            gain_text: "1".to_owned(),
            rotate_text: "0".to_owned(),
            // snuffler config.py defaults: demean/degap/downsample/lphp = on.
            demean: true,
            degap: true,
            allow_downsampling: true,
            fft_filtering: false,
            bandpass_lphp: true,
            remove_response: false,
            distances_3d: false,
            scaling_group: ScalingGroup::Individual,
            scaling_base: ScalingBase::MinMax,
            fix_scale_ranges: false,
            sort: SortMode::Name,
            subsort: Subsort::Nslc,
            view_mode: ViewMode::Wiggle,
            wiggle_theme: WiggleTheme::Fieldbook,
            visible_length: VisibleLength::Adaptive,
            show_boxes: None,
            clip_traces: true,
            show_color_traces: None,
            antialiasing: false,
            show_scale_ranges: false,
            show_scale_axes: false,
            show_zero_lines: false,
            show_track_separators: None,
            show_time_grid: false,
            waterfall: WaterfallSettings::default(),
            snuffling_text: String::new(),
            show_snuffling_text: false,
            show_dataset_note: false,
            // No preference keeps the default visible count active.
            tracks_per_page: None,
            snap_tracks: true,
            track_scroll: 0.0,
            ntracks_shown: DEFAULT_NTRACKS_SHOWN,
            track_offset: 0,
            markers: MarkerStore::default(),
            marker_kind: 0,
            visible_marker_kinds: (0u8..=7).collect(),
            watch_files: false,
            liberal_fetch: false,
            reload_files_requested: false,
            reload_snufflings_requested: false,
            set_active_event_request: None,
            remove_event_hashes: Vec::new(),
            picking: None,
            uncertainty_drag_last_y: None,
            view_drag: None,
            wheel_accumulator: 60.0,
            pending_s_chord: false,
            hovered_track_row: None,
            hovered_marker: None,
            marker_hit_overload: None,
            fullscreen_toggle_requested: false,
            open_files_requested: false,
            save_markers_requested: None,
            export_image_requested: false,
            show_controls: true,
            show_help: false,
            show_about: false,
            show_snuffling_help: false,
            show_download: false,
            show_catalog: false,
            show_map: false,
            show_marker_editor: false,
            show_live_seedlink: false,
            command: CommandLine::default(),
            download_form: DownloadForm::default(),
            catalog_form: CatalogForm::default(),
            seedlink_form: SeedlinkForm::default(),
        }
    }
}

impl AppState {
    /// Visible track count after clamping to the pile and applying Snap.
    #[must_use]
    pub fn visible_track_count(&self, track_count: usize) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "track counts are within f32's exact integer range"
        )]
        let maximum = track_count.max(1) as f32;
        #[expect(
            clippy::cast_precision_loss,
            reason = "the default count is 24, exactly representable in f32"
        )]
        let default_count = DEFAULT_NTRACKS_SHOWN as f32;
        let count = self
            .tracks_per_page
            .unwrap_or(default_count)
            .clamp(1.0, maximum);
        if self.snap_tracks {
            count.round()
        } else {
            count
        }
    }

    /// Removes selected markers and queues registry removal for orphaned events.
    pub fn remove_selected_markers(&mut self) {
        let removed_active = self
            .markers
            .iter()
            .filter(|marker| marker.selected && marker.is_event && marker.active)
            .filter_map(|marker| marker.event_hash.clone())
            .collect::<std::collections::HashSet<_>>();
        let removed = self
            .markers
            .iter()
            .filter(|marker| marker.selected && marker.is_event)
            .filter_map(|marker| marker.event_hash.clone())
            .collect::<Vec<_>>();
        self.markers.retain(|marker| !marker.selected);
        for event_hash in removed_active {
            if let Some(marker) = self
                .markers
                .iter_mut()
                .find(|marker| marker.is_event && marker.event_hash.as_ref() == Some(&event_hash))
            {
                marker.active = true;
            }
        }
        self.queue_orphaned_event_hashes(removed);
    }

    /// Removes every marker and queues each canonical event exactly once.
    pub fn clear_markers(&mut self) {
        let removed = self
            .markers
            .iter()
            .filter(|marker| marker.is_event)
            .filter_map(|marker| marker.event_hash.clone())
            .collect::<Vec<_>>();
        self.markers.clear();
        self.queue_orphaned_event_hashes(removed);
    }

    fn queue_orphaned_event_hashes(&mut self, removed: Vec<String>) {
        let retained = self
            .markers
            .iter()
            .filter(|marker| marker.is_event)
            .filter_map(|marker| marker.event_hash.clone())
            .collect::<std::collections::HashSet<_>>();
        for event_hash in removed {
            if !retained.contains(&event_hash) && !self.remove_event_hashes.contains(&event_hash) {
                self.remove_event_hashes.push(event_hash);
            }
        }
    }

    /// Whether a rule is drawn between rows: the user's choice if they made one,
    /// otherwise whatever the current theme does by default.
    #[must_use]
    pub fn track_separators(&self) -> bool {
        self.show_track_separators
            .unwrap_or_else(|| self.wiggle_theme.default_track_separators())
    }

    /// Whether trace strokes are colored under the live subsort: the user's choice
    /// if they made one, otherwise what the current theme's
    /// [`crate::theme::StrokePolicy`] says.
    #[must_use]
    pub fn color_traces(&self, theme: Theme) -> bool {
        self.color_traces_for(self.subsort, theme)
    }

    /// [`Self::color_traces`] for a given subsort.
    ///
    /// The painter asks about the subsort a reply was produced under rather than the
    /// live one, so a grouped policy cannot flip the strokes mid round trip.
    /// The boxes are read live, since the ones a policy reasons about are the ones
    /// this frame draws.
    #[must_use]
    pub fn color_traces_for(&self, subsort: Subsort, theme: Theme) -> bool {
        self.show_color_traces.unwrap_or_else(|| {
            self.wiggle_theme
                .stroke_policy()
                .colors(subsort, self.boxes_visible(theme))
        })
    }

    /// Whether the trace boxes are drawn: the user's choice if they made one,
    /// otherwise what the theme defaults to for the current canvas.
    ///
    /// Fieldbook draws boxes on both canvases; other themes draw them on light only.
    /// The user's explicit choice overrides either default.
    #[must_use]
    pub fn boxes_visible(&self, theme: Theme) -> bool {
        self.show_boxes
            .unwrap_or_else(|| self.wiggle_theme.default_boxes(theme))
    }

    /// Whether markers of this kind are currently drawn/selectable (the `:markers`
    /// command).
    /// Raw-kind comparison so kind>=8 hides under `markers all`, matching pyrocko
    /// `pile_viewer.py`.
    #[must_use]
    pub fn marker_kind_visible(&self, kind: u8) -> bool {
        self.visible_marker_kinds.contains(&kind)
    }

    /// Marker details for the information strip, with meaningful hover details taking priority.
    ///
    /// Away from a marker, the sole selected marker supplies its details.
    /// Multiple selected markers show their count instead.
    #[must_use]
    pub fn marker_info(&self) -> Option<MarkerInfo> {
        let details = |marker: &crate::markers::Marker| {
            let text = marker.hover_message();
            (!text.is_empty()).then_some(MarkerInfo {
                text,
                kind: Some(marker.kind),
            })
        };
        if let Some(info) = self
            .hovered_marker
            .and_then(|id| self.markers.by_id(id))
            .and_then(details)
        {
            return Some(info);
        }
        let selection = self.markers.summary();
        if selection.selected > 1 {
            return Some(MarkerInfo {
                text: format!("{} markers selected", selection.selected),
                kind: None,
            });
        }
        selection
            .single_selected
            .and_then(|id| self.markers.by_id(id))
            .and_then(details)
    }

    /// Set the visible marker kinds (from the `:markers` command).
    /// Deselects all markers, mirroring snuffler's `markers` command which also clears
    /// the selection.
    pub fn set_visible_marker_kinds(&mut self, mut kinds: Vec<u8>) {
        kinds.sort_unstable();
        kinds.dedup();
        for marker in &mut self.markers {
            marker.selected = false;
        }
        self.visible_marker_kinds = kinds;
    }
}

#[cfg(test)]
mod tests {
    use crate::markers::Marker;
    use crate::theme::{Theme, WiggleTheme};

    use super::AppState;

    #[test]
    fn fieldbook_defaults_to_filled_boxes_with_monochrome_traces_on_both_canvases() {
        let mut state = AppState::default();
        assert_eq!(state.wiggle_theme, WiggleTheme::Fieldbook);
        for theme in Theme::ALL {
            assert!(state.boxes_visible(theme));
            assert!(!state.color_traces(theme));
        }

        // Explicit drawing choices still override the theme defaults.
        state.show_boxes = Some(false);
        state.show_color_traces = Some(true);
        for theme in Theme::ALL {
            assert!(!state.boxes_visible(theme));
            assert!(state.color_traces(theme));
        }
    }

    #[test]
    fn switching_to_snuffler_qt_restores_its_default_drawing_rules() {
        let state = AppState {
            wiggle_theme: WiggleTheme::SnufflerQt,
            ..Default::default()
        };
        assert!(state.boxes_visible(Theme::Light));
        assert!(!state.color_traces(Theme::Light));
        assert!(!state.track_separators());
    }

    fn event(hash: &str, selected: bool, active: bool) -> Marker {
        let mut marker = Marker::new(0.0, 0.0, 0);
        marker.is_event = true;
        marker.selected = selected;
        marker.active = active;
        marker.event_hash = Some(hash.to_owned());
        marker
    }

    #[test]
    fn selected_marker_details_return_after_a_temporary_hover() {
        let mut state = AppState::default();
        let mut selected = event("selected", true, false);
        selected.event_name = Some("Selected event".to_owned());
        selected.kind = 1;
        state.markers.insert(selected);
        let mut hovered = event("hovered", false, false);
        hovered.event_name = Some("Hovered event".to_owned());
        hovered.kind = 2;
        let hovered_id = state.markers.insert(hovered);
        let plain_id = state.markers.insert(Marker::new(1.0, 1.0, 0));

        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = Selected event")
        );
        assert_eq!(state.marker_info().and_then(|info| info.kind), Some(1));
        state.hovered_marker = Some(hovered_id);
        assert_eq!(state.marker_info().and_then(|info| info.kind), Some(2));
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = Hovered event")
        );

        // Leaving the hovered event, including for a marker without details, restores selection.
        state.hovered_marker = None;
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = Selected event")
        );
        assert_eq!(state.marker_info().and_then(|info| info.kind), Some(1));
        state.hovered_marker = Some(plain_id);
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = Selected event")
        );
    }

    #[test]
    fn marker_details_follow_multiple_selection_and_deselection() {
        let mut state = AppState::default();
        let mut first = event("first", true, false);
        first.event_name = Some("First event".to_owned());
        let first_id = state.markers.insert(first);
        let second_id = state.markers.insert(event("second", true, false));
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("2 markers selected")
        );

        assert!(
            state
                .markers
                .edit(second_id, |marker| marker.selected = false)
                .is_some()
        );
        assert_eq!(state.markers.summary().single_selected, Some(first_id));
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = First event")
        );

        assert!(
            state
                .markers
                .edit(first_id, |marker| marker.selected = false)
                .is_some()
        );
        assert_eq!(state.markers.summary().single_selected, None);
        assert_eq!(state.marker_info(), None);
    }

    #[test]
    fn marker_details_refresh_on_edits_and_clear_on_deletion() {
        let mut state = AppState::default();
        let mut selected = event("selected", true, false);
        selected.event_name = Some("Before edit".to_owned());
        let id = state.markers.insert(selected);
        state.hovered_marker = Some(id);
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = Before edit")
        );

        assert!(
            state
                .markers
                .edit(id, |marker| {
                    marker.event_name = Some("After edit".to_owned());
                    marker.kind = 3;
                })
                .is_some()
        );
        assert_eq!(
            state.marker_info().map(|info| info.text).as_deref(),
            Some("name = After edit")
        );

        assert_eq!(state.marker_info().and_then(|info| info.kind), Some(3));

        // A stale hover identity must not retain details or attach to a reused slot.
        state.remove_selected_markers();
        state.markers.insert(event("replacement", false, false));
        assert_eq!(state.markers.summary().single_selected, None);
        assert_eq!(state.marker_info(), None);
    }

    #[test]
    fn marker_deletion_queues_only_hashes_without_a_remaining_event() {
        let mut state = AppState::default();
        state.markers.insert(event("shared", true, true));
        state.markers.insert(event("shared", false, false));

        state.remove_selected_markers();

        // A duplicate event marker still represents the canonical event.
        assert_eq!(state.markers.len(), 1);
        assert_eq!(state.remove_event_hashes, Vec::<String>::new());
        assert!(
            state
                .markers
                .iter()
                .next()
                .is_some_and(|marker| marker.active)
        );

        let Some(marker) = state.markers.iter_mut().next() else {
            assert_eq!(state.markers.len(), 1);
            return;
        };
        marker.selected = true;
        state.remove_selected_markers();

        // Removing the final, active marker queues one canonical deletion.
        assert!(state.markers.is_empty());
        assert_eq!(state.remove_event_hashes, ["shared"]);

        state.markers.insert(event("other", false, false));
        state.markers.insert(Marker::new(1.0, 1.0, 0));
        state.clear_markers();

        // Clear-all queues its event while plain markers have no registry state.
        assert!(state.markers.is_empty());
        assert_eq!(state.remove_event_hashes, ["shared", "other"]);
    }
}
