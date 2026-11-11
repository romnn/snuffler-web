//! The viewer settings that choose how pyrocko processes and the renderer draws the
//! traces: scaling, sorting, view mode, waterfall colours and the visible length.
//!
//! Each setting carries the string pyrocko expects on the wire (`as_str`) and its
//! inverse (`from_wire`), which also restores a persisted choice.

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

/// Statistic used to derive each track's amplitude range (snuffler's "Scaling based on
/// ..." radio group, pile_viewer.py:958-968).
/// Applied in the bridge over the raw samples (where they're available).
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

/// Station sort order (snuffler's "Sorting" station radio group,
/// pile_viewer.py:986-1013).
/// Distance/azimuth variants need station coordinates and an active event; without them
/// the bridge falls back to name order.
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

/// Subsort of the NSLC key (snuffler's subsort radio group, pile_viewer.py:1020-1045).
/// Drives both trace ordering and the "Color traces" coloring key.
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

    /// Whether this subsort gathers a channel's locations onto one row, so a row can
    /// carry several traces that only colour tells apart.
    #[must_use]
    pub const fn groups_locations(self) -> bool {
        matches!(self, Subsort::NscGroupL | Subsort::SncGroupL)
    }

    /// The inverse of [`Subsort::as_str`].
    /// See [`SortMode::from_wire`].
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

/// Waterfall colormaps, named as matplotlib names them.
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

/// Snuffler's "Visible length" (`pile_viewer.py:1047`): a target *sample count* per
/// trace, despite the name — never a time span.
/// `prepare_cutout2` turns it into `min_deltat_allow`, the finest sampling worth fetching
/// for the window, and `make_good_looking_time_range` uses it as the minimum width of a
/// jump.
/// Selecting an entry never moves the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleLength {
    /// No budget: fetch every channel whatever its sampling rate and reduce what comes
    /// back to what the canvas can show.
    /// See [`VisibleLength::samples`].
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
    /// Qt's four entries are a *fetch* budget: a channel sampled finer than the window's
    /// share of it is not fetched at all, so widening the window makes the high-rate
    /// channels disappear rather than coarsen.
    /// Adaptive spends the budget the other way round — nothing is refused, and the
    /// bridge decimates each fetched trace to the pixel budget the canvas asked for
    /// (`bins_per_trace`), which is what keeps a wide window affordable.
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

/// How the waterfall view maps spectral power to colour (snuffler's colorbar
/// controls, `gui/util.py:651`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterfallSettings {
    /// Matplotlib colormap used by pyrocko for waterfall cells.
    pub colormap: Colormap,
    /// Lower colormap clip, as a fraction of each track's power range.
    pub clip_low: f32,
    /// Upper colormap clip, as a fraction of each track's power range.
    pub clip_high: f32,
    /// Whether the colour range is symmetric around zero.
    pub symmetric: bool,
    /// Whether the selected colormap is reversed.
    pub reverse_colormap: bool,
    /// Whether absolute amplitudes are used before the spectral transform.
    pub absolute_amplitudes: bool,
    /// Whether traces are integrated before the spectral transform.
    pub integrate: bool,
    /// Transfer function used to map power to colour.
    pub color_scale: ColorScale,
}

impl Default for WaterfallSettings {
    fn default() -> Self {
        Self {
            colormap: Colormap::Viridis,
            clip_low: 0.0,
            clip_high: 1.0,
            // Checked at boot, as snuffler starts it (gui/util.py:709).
            symmetric: true,
            reverse_colormap: false,
            absolute_amplitudes: false,
            integrate: false,
            color_scale: ColorScale::Lin,
        }
    }
}
