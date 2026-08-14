//! Frontend-independent application state for the viewer.
//!
//! Field/enum names and defaults mirror pyrocko's snuffler (`pile_viewer.py` +
//! `config.py`) so every frontend exposes the same settings.

use crate::command::CommandLine;
use crate::markers::Marker;

/// Number of tracks the auto-fit track height targets on first load, and the
/// reset target for the "=" key.
pub const DEFAULT_NTRACKS_SHOWN: usize = 12;

/// Bounds for the track-height slider (pixels per track row).
pub const MIN_TRACK_H: f32 = 8.0;
/// Maximum user-selected track-row height in logical pixels.
pub const MAX_TRACK_H: f32 = 400.0;

/// How per-track amplitude ranges are grouped across tracks (snuffler's "Scaling"
/// grouping radio group, pile_viewer.py:933-946).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalingGroup {
    /// Derives an independent range for every trace.
    Individual,
    /// Uses one range across all visible traces.
    Common,
    /// Shares ranges among channels from the same station.
    PerStation,
    /// Shares ranges among channels from the same station and location.
    PerStationLocation,
    /// Shares ranges among matching channel components.
    PerComponent,
}

impl ScalingGroup {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScalingGroup::Individual => "individual",
            ScalingGroup::Common => "common",
            ScalingGroup::PerStation => "per_station",
            ScalingGroup::PerStationLocation => "per_station_location",
            ScalingGroup::PerComponent => "per_component",
        }
    }
    /// Returns the frontend label for this grouping.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ScalingGroup::Individual => "Individual",
            ScalingGroup::Common => "Common",
            ScalingGroup::PerStation => "Per station",
            ScalingGroup::PerStationLocation => "Per sta+loc",
            ScalingGroup::PerComponent => "Per component",
        }
    }
}

/// Statistic used to derive each track's amplitude range (snuffler's "Scaling
/// based on ..." radio group, pile_viewer.py:958-968). Applied in the bridge over
/// the raw samples (where they're available).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalingBase {
    /// Uses the exact minimum and maximum.
    MinMax,
    /// Rejects outliers before choosing the minimum and maximum.
    Robust,
    /// Uses the mean plus or minus two standard deviations.
    Mean2Std,
    /// Uses the mean plus or minus four standard deviations.
    Mean4Std,
}

impl ScalingBase {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScalingBase::MinMax => "minmax",
            ScalingBase::Robust => "robust",
            ScalingBase::Mean2Std => "mean2",
            ScalingBase::Mean4Std => "mean4",
        }
    }
    /// Returns the frontend label for this statistic.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ScalingBase::MinMax => "Min / Max",
            ScalingBase::Robust => "Min / Max (robust)",
            ScalingBase::Mean2Std => "Mean ± 2σ",
            ScalingBase::Mean4Std => "Mean ± 4σ",
        }
    }
}

/// Waveform representation requested from pyrocko and shown by the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    /// Time-domain seismogram traces.
    Wiggle,
    /// Time-frequency waterfall cells.
    Waterfall,
}

impl ViewMode {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Wiggle => "wiggle",
            ViewMode::Waterfall => "waterfall",
        }
    }
}

/// Visual treatment applied to time-domain traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WiggleTheme {
    /// Colored trace boxes matching the Qt frontend.
    SnufflerQt,
    /// Colored trace strokes without row tinting.
    ColorLines,
    /// Alternating color-tinted row bands.
    NightBands,
}

impl WiggleTheme {
    /// Returns the frontend label for this theme.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => "Snuffler Qt",
            WiggleTheme::ColorLines => "Color lines",
            WiggleTheme::NightBands => "Night bands",
        }
    }

    /// Themes in menu order.
    pub const ALL: [WiggleTheme; 3] = [
        WiggleTheme::SnufflerQt,
        WiggleTheme::ColorLines,
        WiggleTheme::NightBands,
    ];
}

/// Station sort order (snuffler's "Sorting" station radio group,
/// pile_viewer.py:986-1013). Distance/azimuth variants need station coordinates
/// and an active event; without them the bridge falls back to name order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    /// Lexicographic NSLC order.
    Name,
    /// Great-circle distance from the active event.
    Distance,
    /// Station azimuth from the active event.
    Azimuth,
    /// Distance ordered within twelve azimuth sectors.
    AzimuthBlocks,
    /// Backazimuth from each station to the active event.
    Backazimuth,
}

impl SortMode {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Distance => "distance",
            SortMode::Azimuth => "azimuth",
            SortMode::AzimuthBlocks => "azimuth_blocks",
            SortMode::Backazimuth => "backazimuth",
        }
    }
    /// Returns the frontend label for this order.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "Names",
            SortMode::Distance => "Distance",
            SortMode::Azimuth => "Azimuth",
            SortMode::AzimuthBlocks => "Distance in 12 azimuth blocks",
            SortMode::Backazimuth => "Backazimuth",
        }
    }
    /// True for sorts that require station coords + an active event.
    #[must_use]
    pub fn needs_coords(self) -> bool {
        !matches!(self, SortMode::Name)
    }
}

/// Subsort of the NSLC key (snuffler's subsort radio group,
/// pile_viewer.py:1020-1045). Drives both trace ordering and the "Color traces"
/// coloring key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsort {
    /// Network, station, location, then channel.
    Nslc,
    /// Network, station, channel, then location.
    Nscl,
    /// Station, network, channel, then location.
    Sncl,
    /// Location, network, station, then channel.
    Lnsc,
    /// Channel, network, station, then location.
    Cnsl,
    /// Network, station, and channel grouped by location.
    NscGroupL,
    /// Station, network, and channel grouped by location.
    SncGroupL,
}

impl Subsort {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Subsort::Nslc => "nslc",
            Subsort::Nscl => "nscl",
            Subsort::Sncl => "sncl",
            Subsort::Lnsc => "lnsc",
            Subsort::Cnsl => "cnsl",
            Subsort::NscGroupL => "nsc_l",
            Subsort::SncGroupL => "snc_l",
        }
    }
    /// Returns the frontend label for this order.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Subsort::Nslc => "Network, Station, Location, Channel",
            Subsort::Nscl => "Network, Station, Channel, Location",
            Subsort::Sncl => "Station, Network, Channel, Location",
            Subsort::Lnsc => "Location, Network, Station, Channel",
            Subsort::Cnsl => "Channel, Network, Station, Location",
            Subsort::NscGroupL => "Network, Station, Channel (by Location)",
            Subsort::SncGroupL => "Station, Network, Channel (by Location)",
        }
    }
}

/// Transfer function applied before mapping waterfall power to color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScale {
    /// Logarithmic power scaling.
    Log,
}

impl ColorScale {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ColorScale::Log => "log",
        }
    }
}

/// Waterfall colormaps.
/// Snuffler's `ColorbarControl` (util.py:651-663) offers a different nine,
/// sharing only viridis and magma with this list — an open gap, tracked as
/// `waterfall-colormap-set` in `tests/parity/known_gaps.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colormap {
    /// Perceptually uniform blue-green-yellow colors.
    Viridis,
    /// Perceptually uniform black-purple-yellow colors.
    Magma,
    /// Perceptually uniform black-red-yellow colors.
    Inferno,
    /// Grayscale colors.
    Gray,
    /// Cyclic spectral colors.
    Rainbow,
}

impl Colormap {
    /// Returns matplotlib's colormap name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Colormap::Viridis => "viridis",
            Colormap::Magma => "magma",
            Colormap::Inferno => "inferno",
            Colormap::Gray => "gray",
            Colormap::Rainbow => "rainbow",
        }
    }
    /// Colormaps in menu order.
    pub const ALL: [Colormap; 5] = [
        Colormap::Viridis,
        Colormap::Magma,
        Colormap::Inferno,
        Colormap::Gray,
        Colormap::Rainbow,
    ];
}

/// Maximum visible time span (snuffler's "Visible length", `pile_viewer.py:1047`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleLength {
    /// Up to 20,000 seconds.
    Short,
    /// Up to 60,000 seconds.
    Medium,
    /// Up to 120,000 seconds.
    Long,
    /// Up to 600,000 seconds.
    ExtraLong,
}

impl VisibleLength {
    /// Returns the maximum visible duration in seconds.
    #[must_use]
    pub fn seconds(self) -> f64 {
        match self {
            VisibleLength::Short => 20_000.0,
            VisibleLength::Medium => 60_000.0,
            VisibleLength::Long => 120_000.0,
            VisibleLength::ExtraLong => 600_000.0,
        }
    }
    /// Returns the frontend label for this duration.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            VisibleLength::Short => "Short",
            VisibleLength::Medium => "Medium",
            VisibleLength::Long => "Long",
            VisibleLength::ExtraLong => "Extra long",
        }
    }
    /// Durations in menu order.
    pub const ALL: [VisibleLength; 4] = [
        VisibleLength::Short,
        VisibleLength::Medium,
        VisibleLength::Long,
        VisibleLength::ExtraLong,
    ];
}

/// Pointer origin and time window captured when a waveform drag starts.
#[derive(Clone, Copy, Debug)]
pub struct ViewDrag {
    /// Pointer x-coordinate in logical pixels.
    pub x: f32,
    /// Pointer y-coordinate in logical pixels.
    pub y: f32,
    /// Visible lower time bound captured at drag start, in epoch seconds.
    pub tmin: f64,
    /// Visible upper time bound captured at drag start, in epoch seconds.
    pub tmax: f64,
    /// Whether the drag translates time without vertical zooming.
    pub pan_only: bool,
}

/// State of an in-progress pick in batch/interactive picking mode (Escape).
#[derive(Clone, Debug, Default)]
pub struct PickingState {
    /// Batch mode re-arms a new pick after each commit (Escape starts batch).
    pub batch: bool,
    /// Time of the pointer press that began the current pick; `None` while the
    /// floating pick merely follows the cursor before a press.
    pub down_t: Option<f64>,
    /// Current floating pick span (epoch seconds); `tmin == tmax` is an instant.
    pub tmin: f64,
    /// Upper bound of the floating pick span, in epoch seconds.
    pub tmax: f64,
    /// Inherited marker attributes carried across re-armed picks.
    pub kind: u8,
    /// Phase name inherited by the next committed marker.
    pub phase: Option<String>,
    /// First-motion polarity inherited by the next committed marker.
    pub polarity: Option<i8>,
    /// Channel under the cursor the pick is scoped to.
    pub nslc: Option<(String, String, String, String)>,
}

/// A pending "set active event" request the session drains and sends to the bridge
/// (input handlers set it; they cannot touch the bridge directly). The session
/// converts it to a `SetActiveEventRequest` proto message.
#[derive(Clone, Debug, Default)]
pub struct ActiveEventReq {
    /// Whether the active event should be cleared instead of replaced.
    pub deactivate: bool,
    /// Stable event identifier when the marker came from a catalog.
    pub event_hash: Option<String>,
    /// Event origin time in epoch seconds.
    pub t: f64,
    /// Optional channel identifying the relevant marker.
    pub nslc: Option<(String, String, String, String)>,
}

/// Complete frontend-independent state edited and rendered by a viewer frontend.
pub struct AppState {
    /// Visible time window (epoch seconds).
    pub tmin: f64,
    /// Upper edge of the visible time window, in epoch seconds.
    pub tmax: f64,

    /// Filters (Hz). `None` = filter disabled.
    pub highpass: Option<f64>,
    /// Low-pass cutoff in hertz, or `None` when disabled.
    pub lowpass: Option<f64>,
    /// Last value of the (possibly-disabled) sliders, so toggling keeps it.
    pub highpass_hz: f64,
    /// Last enabled low-pass value in hertz.
    pub lowpass_hz: f64,
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

    // --- processing toggles (Options menu) -> ViewParams ---
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
    /// Deconvolve instrument response (needs `StationXML`; currently inert).
    pub remove_response: bool,
    /// Use station depth for distances (needs coords; inert without).
    pub distances_3d: bool,

    // --- scaling / sorting ---
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

    // --- view / render toggles (View menu, render-side only) ---
    /// Active waveform representation.
    pub view_mode: ViewMode,
    /// Active time-domain trace theme.
    pub wiggle_theme: WiggleTheme,
    /// Maximum duration allowed by the visible-length control.
    pub visible_length: VisibleLength,
    /// Whether trace extents have visible boxes.
    pub show_boxes: bool,
    /// Whether traces are clipped to their own rows.
    pub clip_traces: bool,
    /// Whether traces use NSLC-derived colors.
    pub color_traces: bool,
    /// Whether line tessellation uses feathered antialiasing.
    pub antialiasing: bool,
    /// Whether each row displays its numeric amplitude range.
    pub show_scale_ranges: bool,
    /// Whether each row displays its scale axis.
    pub show_scale_axes: bool,
    /// Whether each row displays its zero-amplitude line.
    pub show_zero_lines: bool,

    // --- waterfall colorbar ---
    /// Matplotlib colormap used by pyrocko for waterfall cells.
    pub wf_cmap: Colormap,
    /// Colormap clip as fractions of each track's power range (lo, hi).
    pub wf_clip: (f32, f32),
    /// Whether the waterfall range is symmetric around zero.
    pub wf_symmetry: bool,
    /// Whether the selected colormap is reversed.
    pub wf_reverse: bool,
    /// Whether absolute amplitudes are used before the spectral transform.
    pub wf_abs: bool,
    /// Whether traces are integrated before the spectral transform.
    pub wf_integrate: bool,
    /// Transfer function used to map power to color.
    pub wf_color_scale: ColorScale,

    /// Snuffling text output (`MinMax` etc.) + its window visibility.
    pub snuffling_text: String,
    /// Whether the text-output window is open.
    pub show_snuffling: bool,

    /// Vertical track layout. `track_height` (pixels per row, `0.0` until the
    /// first layout auto-fits [`DEFAULT_NTRACKS_SHOWN`]) and `snap_tracks` (align
    /// rows to perfect trace boundaries) are the user-facing controls;
    /// `track_scroll` is the fractional index of the topmost visible row.
    /// `ntracks_shown`/`track_offset` are caches the waveform layout recomputes
    /// each frame; the view request reads them to fetch only on-screen tracks.
    pub track_height: f32,
    /// Whether visible rows are aligned to complete trace boundaries.
    pub snap_tracks: bool,
    /// Fractional index of the first visible track row.
    pub track_scroll: f32,
    /// Number of rows requested from pyrocko for the current layout.
    pub ntracks_shown: usize,
    /// Index of the first row requested from pyrocko.
    pub track_offset: usize,

    /// User, catalog, and snuffling annotations in insertion order.
    pub markers: Vec<Marker>,
    /// Next color group applied to new markers (snuffler number keys).
    pub marker_kind: u8,
    /// Set by the `r` key; the session consumes it to trigger a bridge file reload
    /// (keyboard handlers only touch `state`, not the bridge).
    pub reload_requested: bool,
    /// Set by Shift+R; the session consumes it to reload the Python snuffling code.
    pub reload_snufflings_requested: bool,
    /// Set by `e`/`E`; the session drains it to send a `SetActiveEvent` to the bridge.
    pub set_active_event_request: Option<ActiveEventReq>,
    /// Auto-reload opened files as they change on disk (snuffler's "Watch Files").
    pub watch_files: bool,
    /// Fetch a wider, grid-snapped time window so pan/zoom reuses already-fetched
    /// data instead of re-requesting (snuffler's "Liberal Fetch Optimization").
    pub liberal_fetch: bool,
    /// Marker kinds (0..=7) currently drawn/selectable; set by the `:markers`
    /// command (snuffler `pile_viewer.py` `markers` command). Compared against the
    /// raw kind so kind>=8 hides under `markers all`, like pyrocko.
    pub visible_marker_kinds: Vec<u8>,
    /// In-progress span-marker drag (start, current) in epoch seconds.
    pub pending_span: Option<(f64, f64)>,
    /// Active batch/interactive picking mode (Escape toggles it); `None` when not
    /// picking. Mirrors snuffler's picking mode: a floating pick follows the
    /// cursor, press-drag-release commits a marker, and batch mode re-arms a new
    /// pick inheriting kind/phase/polarity.
    pub picking: Option<PickingState>,
    /// Last pointer y during a shift-drag that adjusts phase-marker uncertainty
    /// (`Some` = a latched uncertainty drag is active); mirrors pyrocko's
    /// `set_uncertainty_y`/`_last_y`.
    pub uncertainty_drag_last_y: Option<f32>,
    /// Active mouse drag used for snuffler-style pan/zoom interaction.
    pub view_drag: Option<ViewDrag>,
    /// Accumulates wheel motion into Qt-style 120-unit notches.
    pub wheel_pos: f32,

    /// Whether the bottom processing-control panel is visible.
    pub show_controls: bool,
    /// Whether the browser viewport is in fullscreen mode.
    pub fullscreen: bool,
    /// "Snuffler controls" / "About" help windows.
    pub show_help: bool,
    /// Whether the about window is open.
    pub show_about: bool,

    // --- FDSN network forms (browser fetch) ---
    /// Whether the waveform-download window is open.
    pub show_download: bool,
    /// Whether the catalog-search window is open.
    pub show_catalog: bool,
    /// Whether the station/event map is open.
    pub show_map: bool,
    /// Whether the marker table is open.
    pub show_marker_editor: bool,
    /// The ':' command line — visibility, input, history, and last error.
    pub command: CommandLine,
    /// Selected FDSN waveform data-center preset.
    pub dl_datacenter: String,
    /// Optional custom FDSN waveform endpoint overriding the preset.
    pub dl_base_url: String,
    /// FDSN waveform network selector.
    pub dl_net: String,
    /// FDSN waveform station selector.
    pub dl_sta: String,
    /// FDSN waveform location selector.
    pub dl_loc: String,
    /// FDSN waveform channel selector.
    pub dl_cha: String,
    /// FDSN waveform interval start in UTC text form.
    pub dl_start: String,
    /// FDSN waveform interval end in UTC text form.
    pub dl_end: String,
    /// Selected event-catalog data-center preset.
    pub cat_datacenter: String,
    /// Optional custom event-catalog endpoint overriding the preset.
    pub cat_base_url: String,
    /// Minimum catalog magnitude.
    pub cat_minmag: f64,
    /// Catalog interval start in UTC text form.
    pub cat_start: String,
    /// Catalog interval end in UTC text form.
    pub cat_end: String,
    /// Request full `QuakeML` with moment tensors (beachball + Kagan) on catalog
    /// search, rather than the lightweight text summary.
    pub cat_with_mt: bool,
    /// Whether the live `SeedLink` window is open.
    pub show_live_seedlink: bool,
    /// Live `SeedLink` server host.
    pub live_seedlink_host: String,
    /// Live `SeedLink` server port.
    pub live_seedlink_port: String,
    /// Live `SeedLink` network selector.
    pub live_seedlink_net: String,
    /// Live `SeedLink` station selector.
    pub live_seedlink_sta: String,
    /// Live `SeedLink` channel selectors.
    pub live_seedlink_selectors: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tmin: 0.0,
            tmax: 1.0,
            highpass: None,
            lowpass: None,
            highpass_hz: 0.1,
            lowpass_hz: 10.0,
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
            wiggle_theme: WiggleTheme::SnufflerQt,
            visible_length: VisibleLength::Short,
            show_boxes: true,
            clip_traces: true,
            // snuffler users expect colour-coded traces; default on.
            color_traces: true,
            antialiasing: false,
            show_scale_ranges: false,
            show_scale_axes: false,
            show_zero_lines: false,
            wf_cmap: Colormap::Viridis,
            wf_clip: (0.0, 1.0),
            wf_symmetry: false,
            wf_reverse: false,
            wf_abs: false,
            wf_integrate: false,
            wf_color_scale: ColorScale::Log,
            snuffling_text: String::new(),
            show_snuffling: false,
            // 0.0 = auto-fit DEFAULT_NTRACKS_SHOWN on the first layout.
            track_height: 0.0,
            snap_tracks: true,
            track_scroll: 0.0,
            ntracks_shown: DEFAULT_NTRACKS_SHOWN,
            track_offset: 0,
            markers: Vec::new(),
            marker_kind: 0,
            reload_requested: false,
            reload_snufflings_requested: false,
            set_active_event_request: None,
            watch_files: false,
            liberal_fetch: false,
            visible_marker_kinds: (0u8..=7).collect(),
            pending_span: None,
            picking: None,
            uncertainty_drag_last_y: None,
            view_drag: None,
            wheel_pos: 60.0,
            show_controls: true,
            fullscreen: false,
            show_help: false,
            show_about: false,
            show_download: false,
            show_catalog: false,
            show_map: false,
            show_marker_editor: false,
            command: CommandLine::default(),
            // Pre-filled with a real example (2023-02-06 Turkey M7.8, GE.WLF).
            dl_datacenter: "geofon".into(),
            dl_base_url: String::new(),
            dl_net: "GE".into(),
            dl_sta: "WLF".into(),
            dl_loc: "*".into(),
            dl_cha: "BH?".into(),
            dl_start: "2023-02-06T01:17:00".into(),
            dl_end: "2023-02-06T01:47:00".into(),
            cat_datacenter: "usgs".into(),
            cat_base_url: String::new(),
            cat_minmag: 6.0,
            cat_start: "2023-02-06".into(),
            cat_end: "2023-02-07".into(),
            cat_with_mt: true,
            show_live_seedlink: false,
            live_seedlink_host: "rtserve.iris.washington.edu".into(),
            live_seedlink_port: "18000".into(),
            live_seedlink_net: "IU".into(),
            live_seedlink_sta: "ANMO".into(),
            live_seedlink_selectors: "BH?".into(),
        }
    }
}

impl AppState {
    /// Whether markers of this kind are currently drawn/selectable (the
    /// `:markers` command). Raw-kind comparison so kind>=8 hides under
    /// `markers all`, matching pyrocko `pile_viewer.py`.
    #[must_use]
    pub fn marker_kind_visible(&self, kind: u8) -> bool {
        self.visible_marker_kinds.contains(&kind)
    }

    /// Set the visible marker kinds (from the `:markers` command). Deselects all
    /// markers, mirroring snuffler's `markers` command which also clears the
    /// selection.
    pub fn set_visible_marker_kinds(&mut self, mut kinds: Vec<u8>) {
        kinds.sort_unstable();
        kinds.dedup();
        for m in &mut self.markers {
            m.selected = false;
        }
        self.visible_marker_kinds = kinds;
    }
}
