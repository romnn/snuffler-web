//! Frontend-independent application state for the viewer.
//!
//! Field/enum names and defaults mirror pyrocko's snuffler (`pile_viewer.py` +
//! `config.py`) so every frontend exposes the same settings.

use crate::command::CommandLine;
use crate::markers::Marker;

/// Number of tracks the auto-fit track height targets on first load, and the
/// reset target for the "=" key.
///
/// 24, because that is what the `snuffler` application passes its viewer
/// (`snuffler.py:262`). Upstream is internally inconsistent here — the library
/// entry point defaults to 12 and the widget to 20 — but the shell is the
/// program this port is measured against.
pub const DEFAULT_NTRACKS_SHOWN: usize = 24;

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

    /// The inverse of [`ScalingGroup::as_str`], for a grouping restored from
    /// the persisted settings.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        [
            ScalingGroup::Individual,
            ScalingGroup::Common,
            ScalingGroup::PerStation,
            ScalingGroup::PerStationLocation,
            ScalingGroup::PerComponent,
        ]
        .into_iter()
        .find(|group| group.as_str() == value)
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

    /// The inverse of [`ScalingBase::as_str`], for a statistic restored from
    /// the persisted settings.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        [
            ScalingBase::MinMax,
            ScalingBase::Robust,
            ScalingBase::Mean2Std,
            ScalingBase::Mean4Std,
        ]
        .into_iter()
        .find(|base| base.as_str() == value)
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
    /// Snuffler's own look, reproduced: white canvas, Tango trace boxes, its
    /// `graph_colors` strokes. Light only — see [`Self::forces_light`].
    SnufflerQt,
    /// This frontend's own, designed for both light and dark.
    SnufflerWeb,
}

impl WiggleTheme {
    /// Returns the frontend label for this theme.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => "Snuffler Qt",
            WiggleTheme::SnufflerWeb => "Snuffler Web",
        }
    }

    /// Whether this theme draws a rule between rows, absent an explicit choice from
    /// the user. Snuffler draws none, boxes or no boxes.
    #[must_use]
    pub const fn default_track_separators(self) -> bool {
        matches!(self, WiggleTheme::SnufflerWeb)
    }

    /// Whether this theme colors the trace strokes by their coloring key, absent an
    /// explicit choice from the user.
    ///
    /// Snuffler adds `Color Traces` to its View menu without a `setChecked`
    /// (`pile_viewer.py:1076`), so upstream boots monochrome over colored trace
    /// boxes. [`Self::SnufflerQt`] is the theme that claims fidelity, so it boots
    /// the same way. [`Self::SnufflerWeb`] is built the other way round — colored
    /// strokes are how it tells the members of a location-grouped row apart — and
    /// booting it monochrome would hide that until the user found a checkbox.
    #[must_use]
    pub const fn default_color_traces(self) -> bool {
        matches!(self, WiggleTheme::SnufflerWeb)
    }

    /// Whether this theme pins the canvas to light regardless of the app's
    /// light/dark preference.
    ///
    /// Snuffler has no dark mode: its canvas is `QPalette.Base`, its traces
    /// `aluminium5`, its boxes the light Tango fills at alpha 100 — a palette built
    /// for white and only coherent on it. A dark variant of this theme would not be
    /// snuffler rendered differently, it would be a different theme wearing the
    /// name. So the theme owns the canvas, and the light/dark control belongs to
    /// [`Self::SnufflerWeb`], which was designed for both.
    #[must_use]
    pub const fn forces_light(self) -> bool {
        matches!(self, WiggleTheme::SnufflerQt)
    }

    /// The inverse of [`WiggleTheme::label`], for a theme restored from the
    /// persisted settings.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.label() == value)
    }

    /// Themes in menu order.
    pub const ALL: [WiggleTheme; 2] = [WiggleTheme::SnufflerQt, WiggleTheme::SnufflerWeb];
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

    /// The inverse of [`SortMode::as_str`], for an order that arrives as a string —
    /// an example bundle asking to be opened the way it makes sense in.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        [
            SortMode::Name,
            SortMode::Distance,
            SortMode::Azimuth,
            SortMode::AzimuthBlocks,
            SortMode::Backazimuth,
        ]
        .into_iter()
        .find(|mode| mode.as_str() == value)
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

    /// The inverse of [`Subsort::as_str`]. See [`SortMode::from_wire`].
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        [
            Subsort::Nslc,
            Subsort::Nscl,
            Subsort::Sncl,
            Subsort::Lnsc,
            Subsort::Cnsl,
            Subsort::NscGroupL,
            Subsort::SncGroupL,
        ]
        .into_iter()
        .find(|subsort| subsort.as_str() == value)
    }
}

/// Transfer function applied before mapping waterfall power to color.
///
/// Snuffler's `Normalize` is strictly linear (`pile_viewer_waterfall.py:28`) and
/// has no such control, so `Lin` is the default — the sqrt and log compressions
/// are this frontend's own additions and stay inert until asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScale {
    /// Linear scaling, snuffler's own.
    Lin,
    /// Square-root compression.
    Sqrt,
    /// Logarithmic compression.
    Log,
}

impl ColorScale {
    /// Returns the protobuf value understood by pyrocko.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ColorScale::Lin => "lin",
            ColorScale::Sqrt => "sqrt",
            ColorScale::Log => "log",
        }
    }
    /// Returns the frontend label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ColorScale::Lin => "Linear",
            ColorScale::Sqrt => "Sqrt",
            ColorScale::Log => "Log",
        }
    }
    /// Color scales in menu order.
    pub const ALL: [ColorScale; 3] = [ColorScale::Lin, ColorScale::Sqrt, ColorScale::Log];
}

/// Waterfall colormaps.
/// Snuffler's `ColorbarControl` (util.py:651-663) offers a different nine,
/// sharing only viridis and magma with this list — an open gap, tracked as
/// `waterfall-colormap-set` in `tests/parity/known_gaps.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colormap {
    /// Perceptually uniform blue-green-yellow colors.
    Viridis,
    /// Perceptually uniform blue-red-yellow colors.
    Plasma,
    /// Perceptually uniform black-purple-yellow colors.
    Magma,
    /// White-to-black grayscale.
    Binary,
    /// White-to-red sequential colors.
    Reds,
    /// Black-to-copper sequential colors.
    Copper,
    /// Blue-white-red diverging colors.
    Seismic,
    /// Red-white-blue diverging colors.
    RdBu,
    /// Yellow-green sequential colors.
    YlGn,
}

impl Colormap {
    /// Returns matplotlib's colormap name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Colormap::Viridis => "viridis",
            Colormap::Plasma => "plasma",
            Colormap::Magma => "magma",
            Colormap::Binary => "binary",
            Colormap::Reds => "Reds",
            Colormap::Copper => "copper",
            Colormap::Seismic => "seismic",
            Colormap::RdBu => "RdBu",
            Colormap::YlGn => "YlGn",
        }
    }
    /// Colormaps in menu order — snuffler's own nine, in snuffler's own order
    /// (`ColorbarControl.AVAILABLE_CMAPS`, gui/util.py:651).
    pub const ALL: [Colormap; 9] = [
        Colormap::Viridis,
        Colormap::Plasma,
        Colormap::Magma,
        Colormap::Binary,
        Colormap::Reds,
        Colormap::Copper,
        Colormap::Seismic,
        Colormap::RdBu,
        Colormap::YlGn,
    ];
}

/// Snuffler's "Visible length" (`pile_viewer.py:1047`): a target *sample count*
/// per trace, despite the name — never a time span. `prepare_cutout2` turns it
/// into `min_deltat_allow`, the finest sampling worth fetching for the window,
/// and `make_good_looking_time_range` uses it as the minimum width of a jump.
/// Selecting an entry never moves the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleLength {
    /// No budget: fetch every channel whatever its sampling rate and reduce what
    /// comes back to what the canvas can show. See [`VisibleLength::samples`].
    Adaptive,
    /// Up to 20,000 samples.
    Short,
    /// Up to 60,000 samples.
    Medium,
    /// Up to 120,000 samples.
    Long,
    /// Up to 600,000 samples.
    ExtraLong,
}

impl VisibleLength {
    /// Upstream's own default budget (`config.py:73`, the first radio entry),
    /// which is what a jump measures itself against when nothing else does.
    const UPSTREAM_DEFAULT_SAMPLES: f64 = 20_000.0;

    /// Returns the target sample count, or `None` for
    /// [`VisibleLength::Adaptive`], which sets no target.
    ///
    /// Qt's four entries are a *fetch* budget: a channel sampled finer than the
    /// window's share of it is not fetched at all, so widening the window makes
    /// the high-rate channels disappear rather than coarsen. Adaptive spends the
    /// budget the other way round — nothing is refused, and the bridge decimates
    /// each fetched trace to the pixel budget the canvas asked for (`npx`), which
    /// is what keeps a wide window affordable.
    #[must_use]
    pub fn samples(self) -> Option<f64> {
        match self {
            VisibleLength::Adaptive => None,
            VisibleLength::Short => Some(Self::UPSTREAM_DEFAULT_SAMPLES),
            VisibleLength::Medium => Some(60_000.0),
            VisibleLength::Long => Some(120_000.0),
            VisibleLength::ExtraLong => Some(600_000.0),
        }
    }

    /// Returns the sample count a window jump floors its width at
    /// (`make_good_looking_time_range`, `pile_viewer.py:2766`).
    ///
    /// The jump needs a number even where the fetch wants none, so Adaptive lends
    /// it upstream's default: `goto`, `g` and `G` land on the same window they
    /// land on in Qt out of the box.
    #[must_use]
    pub fn jump_samples(self) -> f64 {
        self.samples().unwrap_or(Self::UPSTREAM_DEFAULT_SAMPLES)
    }

    /// Whether this is the policy that fetches everything and decimates.
    #[must_use]
    pub const fn is_adaptive(self) -> bool {
        matches!(self, VisibleLength::Adaptive)
    }

    /// Returns the frontend label for this duration.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            VisibleLength::Adaptive => "Adaptive",
            VisibleLength::Short => "Short",
            VisibleLength::Medium => "Medium",
            VisibleLength::Long => "Long",
            VisibleLength::ExtraLong => "Extra long",
        }
    }

    /// Returns what picking this entry does, for the menu's hover text.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            VisibleLength::Adaptive => {
                "Draw every channel whatever its sampling rate, reduced to what the \
                 canvas can show. The four budgets below are snuffler's own, and drop \
                 channels sampled finer than the window's share of them."
            }
            _ => {
                "Snuffler's own fetch budget: channels sampled finer than this \
                 window's share of it are not drawn at all."
            }
        }
    }

    /// The inverse of [`VisibleLength::label`], for a choice restored from the
    /// persisted settings (the labels are Qt's `snuffler.pf` setting keys, plus
    /// this frontend's own "Adaptive").
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|length| length.label() == value)
    }
    /// Durations in menu order.
    pub const ALL: [VisibleLength; 5] = [
        VisibleLength::Adaptive,
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
    /// Active-event association inherited by the next committed phase pick.
    pub event_hash: Option<String>,
    /// Associated event origin time used by marker-file fallback matching.
    pub event_time: Option<f64>,
    /// The channels of the track under the cursor the pick is scoped to —
    /// every channel gathered onto the row, as Qt's `get_nslc_ids_for_track`
    /// returns them. Empty while no track is under the cursor.
    pub nslc_ids: Vec<(String, String, String, String)>,
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
    /// Deconvolves the instrument response when matching `StationXML` is loaded.
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
    /// The per-trace sample budget the visible-length control asks the bridge for.
    pub visible_length: VisibleLength,
    /// Whether trace extents have visible boxes.
    pub show_boxes: bool,
    /// Whether traces are clipped to their own rows.
    pub clip_traces: bool,
    /// Whether traces use NSLC-derived colors.
    ///
    /// `None` follows the theme, which is what keeps the Snuffler Qt theme booting
    /// the way snuffler boots (see [`WiggleTheme::default_color_traces`]); `Some` is
    /// the user overriding that, honoured under both themes. Read it through
    /// [`AppState::color_traces`], never directly.
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
    /// `None` follows the theme — off under Snuffler Qt, which separates rows with
    /// nothing at all, and on under Snuffler Web. `Some` is the user overriding
    /// that, and is honoured under *both* themes: parity is about what the port does
    /// by default, not about refusing to draw a rule somebody asked for.
    pub show_track_separators: Option<bool>,
    /// Whether the time ticks extend into a full-height grid across the tracks.
    ///
    /// Web-only, and off by default because snuffler has no such grid: `TimeAx.drawit`
    /// (`pile_viewer.py:539`) marks each tick with a 10 px stroke at the axis and draws
    /// nothing across the rows. Reading a long window is easier with one, so the port
    /// offers it — as something the user switches on, not as a difference it imposes.
    pub show_time_grid: bool,

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

    /// Whether the window showing what the loaded dataset says about itself is
    /// open. It opens itself once, collapsed, when a dataset that says something
    /// loads; the text lives in the bridge, with the scene it describes.
    pub show_dataset_note: bool,

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
    /// The controls panel's marker-kind picker highlight: the last kind applied
    /// through it. Fresh picks are always kind 0, as snuffler's are — nothing
    /// reads this as a default for new markers.
    pub marker_kind: u8,
    /// Set by the `r` key; the session consumes it to trigger a bridge file reload
    /// (keyboard handlers only touch `state`, not the bridge).
    pub reload_requested: bool,
    /// Set by Shift+R; the session consumes it to reload the Python snuffling code.
    pub reload_snufflings_requested: bool,
    /// Set by `e`/`E`; the session drains it to send a `SetActiveEvent` to the bridge.
    pub set_active_event_request: Option<ActiveEventReq>,
    /// Event hashes whose final frontend event marker was deleted.
    /// The session drains them into Python's canonical registry before another
    /// scientific consumer can resolve the removed events.
    pub remove_event_hashes: Vec<String>,
    /// Auto-reload opened files as they change on disk (snuffler's "Watch Files").
    pub watch_files: bool,
    /// Fetch a wider, grid-snapped time window so pan/zoom reuses already-fetched
    /// data instead of re-requesting (snuffler's "Liberal Fetch Optimization").
    pub liberal_fetch: bool,
    /// Marker kinds (0..=7) currently drawn/selectable; set by the `:markers`
    /// command (snuffler `pile_viewer.py` `markers` command). Compared against the
    /// raw kind so kind>=8 hides under `markers all`, like pyrocko.
    pub visible_marker_kinds: Vec<u8>,
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
    /// A pending `f`-key or View ▸ Fullscreen request, consumed by the app,
    /// which calls the browser's Fullscreen API on the document element — the
    /// only path that works on the web; eframe's `ViewportCommand::Fullscreen`
    /// lands in an unhandled arm there.
    pub fullscreen_toggle_requested: bool,
    /// A pending Ctrl+O, consumed by the app (snuffler's File ▸ Open accelerator).
    pub open_files_requested: bool,
    /// A pending Ctrl+S / Ctrl+Shift+S, consumed by the app: `Some(true)` saves
    /// the selected markers, `Some(false)` all of them.
    pub save_markers_requested: Option<bool>,
    /// A pending Ctrl+E, consumed by the app (snuffler's Save-as-image accelerator).
    pub export_image_requested: bool,
    /// A bare `s` was pressed: the next key completes one of snuffler's
    /// two-stroke chords (`s i`/`s c`/`s s` scaling, `s n`/`s d` sorting).
    pub pending_s_chord: bool,
    /// The on-screen track row under the pointer, if any — what snuffler reads
    /// off `QCursor.pos()` when rows are too short for every label and only the
    /// hovered one is drawn (`pile_viewer.py:3530`).
    pub hover_track_row: Option<usize>,
    /// Index into `markers` of the marker under the pointer — snuffler's per-
    /// marker `alerted` flag (`pile_viewer.py:2168`): the hovered marker brightens
    /// to its selected shade, its guides go dashed (grey when unselected), and
    /// its metadata is posted to the status line.
    pub hovered_marker: Option<usize>,
    /// "Snuffler controls" / "About" help windows.
    pub show_help: bool,
    /// Whether the about window is open.
    pub show_about: bool,
    /// Whether the per-snuffling help window (Help ▸ Snuffling Help) is open.
    pub show_snuffling_help: bool,

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
    /// Station search by radius around the active event (Qt's primary download
    /// workflow), instead of by code selectors alone.
    pub dl_around_event: bool,
    /// Radius of the around-event station search, in degrees.
    pub dl_radius_deg: f64,
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
    /// Live follow-viewport width in seconds (Qt's `--follow N`).
    pub live_seedlink_follow_s: f64,
    /// Seconds of live data kept before older samples are discarded — the
    /// wasm32 heap is one bounded block, so the buffer needs *a* bound, but
    /// the number is the user's to set.
    pub live_seedlink_retain_s: f64,
}

impl Default for AppState {
    #[expect(
        clippy::too_many_lines,
        reason = "one literal initializer per state field; splitting it hides nothing"
    )]
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
            visible_length: VisibleLength::Adaptive,
            show_boxes: true,
            clip_traces: true,
            // snuffler users expect colour-coded traces; default on.
            show_color_traces: None,
            antialiasing: false,
            show_scale_ranges: false,
            show_scale_axes: false,
            show_zero_lines: false,
            show_track_separators: None,
            show_time_grid: false,
            wf_cmap: Colormap::Viridis,
            wf_clip: (0.0, 1.0),
            // Checked at boot, as snuffler starts it (gui/util.py:709).
            wf_symmetry: true,
            wf_reverse: false,
            wf_abs: false,
            wf_integrate: false,
            wf_color_scale: ColorScale::Lin,
            snuffling_text: String::new(),
            show_snuffling: false,
            show_dataset_note: false,
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
            remove_event_hashes: Vec::new(),
            watch_files: false,
            liberal_fetch: false,
            visible_marker_kinds: (0u8..=7).collect(),
            picking: None,
            uncertainty_drag_last_y: None,
            view_drag: None,
            wheel_pos: 60.0,
            show_controls: true,
            fullscreen_toggle_requested: false,
            open_files_requested: false,
            save_markers_requested: None,
            export_image_requested: false,
            pending_s_chord: false,
            hover_track_row: None,
            hovered_marker: None,
            show_help: false,
            show_about: false,
            show_snuffling_help: false,
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
            dl_around_event: false,
            dl_radius_deg: 5.0,
            // GEOFON, as Qt's catalog search defaults (catalogs.py:56).
            cat_datacenter: "geofon".into(),
            cat_base_url: String::new(),
            // 0, as Qt defaults — a 6.0 floor silently hides most seismicity.
            cat_minmag: 0.0,
            cat_start: "2023-02-06".into(),
            cat_end: "2023-02-07".into(),
            cat_with_mt: true,
            show_live_seedlink: false,
            live_seedlink_host: "rtserve.iris.washington.edu".into(),
            live_seedlink_port: "18000".into(),
            live_seedlink_net: "IU".into(),
            live_seedlink_sta: "ANMO".into(),
            live_seedlink_selectors: "BH?".into(),
            live_seedlink_follow_s: 300.0,
            live_seedlink_retain_s: 1800.0,
        }
    }
}

impl AppState {
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

    /// Whether trace strokes are colored: the user's choice if they made one,
    /// otherwise whatever the current theme does by default.
    #[must_use]
    pub fn color_traces(&self) -> bool {
        self.show_color_traces
            .unwrap_or_else(|| self.wiggle_theme.default_color_traces())
    }

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

#[cfg(test)]
mod tests {
    use crate::markers::Marker;

    use super::AppState;

    fn event(hash: &str, selected: bool, active: bool) -> Marker {
        let mut marker = Marker::new(0.0, 0.0, 0);
        marker.is_event = true;
        marker.selected = selected;
        marker.active = active;
        marker.event_hash = Some(hash.to_owned());
        marker
    }

    #[test]
    fn marker_deletion_queues_only_hashes_without_a_remaining_event() {
        let mut state = AppState {
            markers: vec![event("shared", true, true), event("shared", false, false)],
            ..Default::default()
        };

        state.remove_selected_markers();

        // A duplicate event marker still represents the canonical event.
        assert_eq!(state.markers.len(), 1);
        assert!(state.remove_event_hashes.is_empty());
        assert!(state.markers.first().is_some_and(|marker| marker.active));

        let Some(marker) = state.markers.first_mut() else {
            assert_eq!(state.markers.len(), 1);
            return;
        };
        marker.selected = true;
        state.remove_selected_markers();

        // Removing the final, active marker queues one canonical deletion.
        assert!(state.markers.is_empty());
        assert_eq!(state.remove_event_hashes, ["shared"]);

        state.markers = vec![event("other", false, false), Marker::new(1.0, 1.0, 0)];
        state.clear_markers();

        // Clear-all queues its event while plain markers have no registry state.
        assert!(state.markers.is_empty());
        assert_eq!(state.remove_event_hashes, ["shared", "other"]);
    }
}
