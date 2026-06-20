//! UI/application state for the viewer (everything the user can tweak).
//!
//! Field/enum names and defaults mirror pyrocko's snuffler (`pile_viewer.py` +
//! `config.py`) so the egui frontend exposes the same settings.

use crate::view::markers::Marker;

/// Number of tracks shown by default, and the reset target for the "=" key.
pub const DEFAULT_NTRACKS_SHOWN: usize = 12;

/// How per-track amplitude ranges are grouped across tracks (snuffler's "Scaling"
/// grouping radio group, pile_viewer.py:933-946).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalingGroup {
    Individual,
    Common,
    PerStation,
    PerStationLocation,
    PerComponent,
}

impl ScalingGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            ScalingGroup::Individual => "individual",
            ScalingGroup::Common => "common",
            ScalingGroup::PerStation => "per_station",
            ScalingGroup::PerStationLocation => "per_station_location",
            ScalingGroup::PerComponent => "per_component",
        }
    }
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
    MinMax,
    Robust,
    Mean2Std,
    Mean4Std,
}

impl ScalingBase {
    pub fn as_str(self) -> &'static str {
        match self {
            ScalingBase::MinMax => "minmax",
            ScalingBase::Robust => "robust",
            ScalingBase::Mean2Std => "mean2",
            ScalingBase::Mean4Std => "mean4",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ScalingBase::MinMax => "Min / Max",
            ScalingBase::Robust => "Min / Max (robust)",
            ScalingBase::Mean2Std => "Mean ± 2σ",
            ScalingBase::Mean4Std => "Mean ± 4σ",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Wiggle,
    Waterfall,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Wiggle => "wiggle",
            ViewMode::Waterfall => "waterfall",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WiggleTheme {
    SnufflerQt,
    ColorLines,
    NightBands,
}

impl WiggleTheme {
    pub fn label(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => "Snuffler Qt",
            WiggleTheme::ColorLines => "Color lines",
            WiggleTheme::NightBands => "Night bands",
        }
    }

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
    Name,
    Distance,
    Azimuth,
    AzimuthBlocks,
    Backazimuth,
}

impl SortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Distance => "distance",
            SortMode::Azimuth => "azimuth",
            SortMode::AzimuthBlocks => "azimuth_blocks",
            SortMode::Backazimuth => "backazimuth",
        }
    }
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
    pub fn needs_coords(self) -> bool {
        !matches!(self, SortMode::Name)
    }
}

/// Subsort of the NSLC key (snuffler's subsort radio group,
/// pile_viewer.py:1020-1045). Drives both trace ordering and the "Color traces"
/// coloring key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsort {
    Nslc,
    Nscl,
    Sncl,
    Lnsc,
    Cnsl,
    NscGroupL,
    SncGroupL,
}

impl Subsort {
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
    /// Which NSLC element drives the trace color when "Color traces" is on:
    /// 0=net 1=sta 2=loc 3=cha (matches snuffler's coloring key).
    pub fn color_index(self) -> usize {
        match self {
            Subsort::Nslc | Subsort::NscGroupL | Subsort::SncGroupL => 2, // by location
            Subsort::Nscl | Subsort::Sncl | Subsort::Lnsc => 3,           // by channel
            Subsort::Cnsl => 1,                                           // by station
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScale {
    Log,
    Sqrt,
    Lin,
}

impl ColorScale {
    pub fn as_str(self) -> &'static str {
        match self {
            ColorScale::Log => "log",
            ColorScale::Sqrt => "sqrt",
            ColorScale::Lin => "lin",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ColorScale::Log => "log",
            ColorScale::Sqrt => "sqrt",
            ColorScale::Lin => "linear",
        }
    }
}

/// Waterfall colormaps (a subset of snuffler's, util.py:651-663).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colormap {
    Viridis,
    Magma,
    Inferno,
    Gray,
    Rainbow,
}

impl Colormap {
    pub fn label(self) -> &'static str {
        match self {
            Colormap::Viridis => "viridis",
            Colormap::Magma => "magma",
            Colormap::Inferno => "inferno",
            Colormap::Gray => "gray",
            Colormap::Rainbow => "rainbow",
        }
    }
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
    Short,
    Medium,
    Long,
    ExtraLong,
}

impl VisibleLength {
    pub fn seconds(self) -> f64 {
        match self {
            VisibleLength::Short => 20_000.0,
            VisibleLength::Medium => 60_000.0,
            VisibleLength::Long => 120_000.0,
            VisibleLength::ExtraLong => 600_000.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            VisibleLength::Short => "Short",
            VisibleLength::Medium => "Medium",
            VisibleLength::Long => "Long",
            VisibleLength::ExtraLong => "Extra long",
        }
    }
    pub const ALL: [VisibleLength; 4] = [
        VisibleLength::Short,
        VisibleLength::Medium,
        VisibleLength::Long,
        VisibleLength::ExtraLong,
    ];
}

pub struct AppState {
    /// Visible time window (epoch seconds).
    pub tmin: f64,
    pub tmax: f64,

    /// Filters (Hz). `None` = filter disabled.
    pub highpass: Option<f64>,
    pub lowpass: Option<f64>,
    /// Last value of the (possibly-disabled) sliders, so toggling keeps it.
    pub highpass_hz: f64,
    pub lowpass_hz: f64,
    pub gain: f32,
    pub rotate_deg: f64,

    // --- processing toggles (Options menu) -> ViewParams ---
    pub demean: bool,
    pub degap: bool,
    pub allow_downsampling: bool,
    pub fft_filtering: bool,
    pub bandpass_lphp: bool,
    /// Deconvolve instrument response (needs `StationXML`; currently inert).
    pub remove_response: bool,
    /// Use station depth for distances (needs coords; inert without).
    pub distances_3d: bool,

    // --- scaling / sorting ---
    pub scaling_group: ScalingGroup,
    pub scaling_base: ScalingBase,
    pub fix_scale_ranges: bool,
    pub sort: SortMode,
    pub subsort: Subsort,

    // --- view / render toggles (View menu, render-side only) ---
    pub view_mode: ViewMode,
    pub wiggle_theme: WiggleTheme,
    pub visible_length: VisibleLength,
    pub show_boxes: bool,
    pub clip_traces: bool,
    pub color_traces: bool,
    pub antialiasing: bool,
    pub show_scale_ranges: bool,
    pub show_scale_axes: bool,
    pub show_zero_lines: bool,

    // --- waterfall colorbar ---
    pub wf_cmap: Colormap,
    /// Colormap clip as fractions of each track's power range (lo, hi).
    pub wf_clip: (f32, f32),
    pub wf_symmetry: bool,
    pub wf_reverse: bool,
    pub wf_abs: bool,
    pub wf_integrate: bool,
    pub wf_color_scale: ColorScale,

    /// Amplitude-spectrum snuffling window open.
    pub show_spectrum: bool,
    /// Snuffling text output (`MinMax` etc.) + its window visibility.
    pub snuffling_text: String,
    pub show_snuffling: bool,

    /// Vertical track windowing (snuffler's `ntracks_shown_max` + scroll).
    pub ntracks_shown: usize,
    pub track_offset: usize,

    pub markers: Vec<Marker>,
    /// Next color group applied to new markers (snuffler number keys).
    pub marker_kind: u8,
    /// In-progress span-marker drag (start, current) in epoch seconds.
    pub pending_span: Option<(f64, f64)>,

    pub show_controls: bool,
    /// "Snuffler controls" / "About" help windows.
    pub show_help: bool,
    pub show_about: bool,

    // --- FDSN network forms (browser fetch) ---
    pub show_download: bool,
    pub show_catalog: bool,
    pub show_map: bool,
    pub show_marker_editor: bool,
    /// The ':' command line: visible flag, current input, and last feedback.
    pub show_command: bool,
    pub command_text: String,
    pub command_feedback: String,
    pub dl_datacenter: String,
    pub dl_net: String,
    pub dl_sta: String,
    pub dl_loc: String,
    pub dl_cha: String,
    pub dl_start: String,
    pub dl_end: String,
    pub cat_datacenter: String,
    pub cat_minmag: f64,
    pub cat_start: String,
    pub cat_end: String,
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
            show_spectrum: false,
            snuffling_text: String::new(),
            show_snuffling: false,
            ntracks_shown: DEFAULT_NTRACKS_SHOWN,
            track_offset: 0,
            markers: Vec::new(),
            marker_kind: 0,
            pending_span: None,
            show_controls: true,
            show_help: false,
            show_about: false,
            show_download: false,
            show_catalog: false,
            show_map: false,
            show_marker_editor: false,
            show_command: false,
            command_text: String::new(),
            command_feedback: String::new(),
            // Pre-filled with a real example (2023-02-06 Turkey M7.8, GE.WLF).
            dl_datacenter: "geofon".into(),
            dl_net: "GE".into(),
            dl_sta: "WLF".into(),
            dl_loc: "*".into(),
            dl_cha: "BH?".into(),
            dl_start: "2023-02-06T01:17:00".into(),
            dl_end: "2023-02-06T01:47:00".into(),
            cat_datacenter: "usgs".into(),
            cat_minmag: 6.0,
            cat_start: "2023-02-06".into(),
            cat_end: "2023-02-07".into(),
        }
    }
}
