//! Marker model + colors, mirroring pyrocko `gui/snuffler/marker.py`.
//!
//! A marker is a time (or time span) annotation, optionally tied to specific
//! NSLC channels, optionally a named seismic phase. `kind` (0..=7) selects a
//! color, exactly like snuffler's number-key colouring.

use ecolor::Color32;

/// Tango palette indexed by marker "kind" (snuffler keys 0..=7).
pub const MARKER_COLORS: [Color32; 8] = [
    Color32::from_rgb(204, 0, 0),     // 0 scarlet red
    Color32::from_rgb(115, 210, 22),  // 1 chameleon (green)
    Color32::from_rgb(52, 101, 164),  // 2 sky blue
    Color32::from_rgb(245, 121, 0),   // 3 orange
    Color32::from_rgb(117, 80, 123),  // 4 plum
    Color32::from_rgb(143, 89, 2),    // 5 chocolate
    Color32::from_rgb(237, 212, 0),   // 6 butter
    Color32::from_rgb(136, 138, 133), // 7 aluminium
];

/// Phase names bound to F1..=F10, following snuffler's defaults.
pub const PHASE_KEYS: [&str; 10] = ["P", "S", "PP", "SS", "PcP", "ScS", "PKP", "SKS", "Pg", "Sg"];

/// A time annotation, optional seismic phase pick, or catalog event.
#[derive(Clone, Debug)]
pub struct Marker {
    /// Start time (epoch seconds).
    pub tmin: f64,
    /// End time (== tmin for an instantaneous pick).
    pub tmax: f64,
    /// Color group 0..=7.
    pub kind: u8,
    /// Restrict to a single channel; `None` = applies to all tracks.
    pub nslc: Option<(String, String, String, String)>,
    /// Named seismic phase (e.g. "P", "S"); `None` for a plain marker.
    pub phase: Option<String>,
    /// First-motion polarity of a phase pick: `Some(1)` up, `Some(-1)` down,
    /// `Some(0)` explicitly undecided, `None` unset (mirrors pyrocko's
    /// `PhaseMarker.set_polarity`, which only accepts 1/-1/0/None).
    pub polarity: Option<i8>,
    /// Event markers are drawn as a triangle on the time axis.
    pub is_event: bool,
    /// Whether the marker participates in the next selection-based operation.
    pub selected: bool,
    /// Focal-mechanism moment tensor (NED m6: mnn, mee, mdd, mne, mnd, med) for a
    /// catalog event, used to render a beachball; `None` when the event has no MT.
    pub moment_tensor: Option<[f64; 6]>,
    /// Kagan angle (degrees) between this event's MT and the active event's MT,
    /// precomputed in Python; `None` when either lacks an MT.
    pub kagan_deg: Option<f64>,
    /// Phase-pick time uncertainty (seconds); drawn as translucent ± brackets
    /// around the pick. Set by shift-dragging on a selected phase marker (mirrors
    /// pyrocko `PhaseMarker._uncertainty`); `None`/0 hides the brackets.
    pub uncertainty: Option<f64>,
    /// Catalog/loaded event identity (matches the Python event registry), used to
    /// pick this event as the active reference and to key Kagan updates.
    pub event_hash: Option<String>,
    /// Event-origin fields for the marker table (from the catalog Event); `depth`
    /// is kilometres. `None` on non-event markers.
    pub magnitude: Option<f64>,
    /// Event depth in kilometres, or `None` on non-event markers.
    pub depth_km: Option<f64>,
    /// Event latitude in degrees north, or `None` on non-event markers.
    pub lat: Option<f64>,
    /// Event longitude in degrees east, or `None` on non-event markers.
    pub lon: Option<f64>,
    /// The single active event marker (the distance/Kagan/cake/seismosizer
    /// reference); exactly one marker is active at a time. Set by 'e', cleared by 'E'.
    pub active: bool,
}

/// A sortable column in the marker-editor table (snuffler's clickable headers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerColumn {
    /// Plain, phase, or event marker type.
    Type,
    /// Marker start time.
    Time,
    /// Marker duration.
    Span,
    /// Phase name.
    Phase,
    /// First-motion polarity.
    Pol,
    /// Numeric color kind.
    Kind,
    /// Optional NSLC channel scope.
    Channel,
    /// Distance from the active event.
    Dist,
    /// Focal-mechanism Kagan angle.
    Kagan,
    /// Event magnitude.
    Mag,
    /// Event depth.
    Depth,
    /// Event latitude.
    Lat,
    /// Event longitude.
    Lon,
}

/// Result of parsing a `:markers ...` command line.
pub enum MarkersCommand {
    /// Set the visible marker kinds (empty = hide all).
    Set(Vec<u8>),
    /// Recognised as a `markers` command but with no effect (e.g. too many args).
    NoChange,
}

/// Parse a `:markers ...` command, mirroring snuffler `pile_viewer.py`'s `markers`
/// arm: `markers all` shows kinds 0..=7; `markers <digits>` shows exactly the
/// digit-kinds in the single token; bare `markers` hides all; anything else is a
/// no-op. Returns `None` when the line is not a `markers` command.
pub fn parse_markers_command(line: &str) -> Option<MarkersCommand> {
    let mut toks = line.split_whitespace();
    let cmd = toks.next()?;
    if !cmd.eq_ignore_ascii_case("markers") {
        return None;
    }
    let arg = toks.next();
    // A `markers` command takes at most one argument; more tokens are a no-op.
    if toks.next().is_some() {
        return Some(MarkersCommand::NoChange);
    }
    Some(match arg {
        None => MarkersCommand::Set(Vec::new()),
        Some("all") => MarkersCommand::Set((0u8..=7).collect()),
        Some(spec) => MarkersCommand::Set(
            spec.bytes()
                .filter(u8::is_ascii_digit)
                .map(|b| b - b'0')
                .collect(),
        ),
    })
}

impl Marker {
    /// Creates a plain marker and normalizes reversed time bounds.
    #[must_use]
    pub fn new(tmin: f64, tmax: f64, kind: u8) -> Self {
        Self {
            tmin: tmin.min(tmax),
            tmax: tmin.max(tmax),
            kind,
            nslc: None,
            phase: None,
            polarity: None,
            is_event: false,
            selected: false,
            moment_tensor: None,
            kagan_deg: None,
            uncertainty: None,
            event_hash: None,
            magnitude: None,
            depth_km: None,
            lat: None,
            lon: None,
            active: false,
        }
    }

    /// Returns the Tango palette color selected by the low three bits of `kind`.
    #[must_use]
    pub fn color(&self) -> Color32 {
        // `kind & 7` is always in 0..=7, matching `MARKER_COLORS`'s length;
        // the fallback (color for kind 0) is therefore unreachable.
        MARKER_COLORS
            .get((self.kind & 7) as usize)
            .copied()
            .unwrap_or(Color32::from_rgb(204, 0, 0))
    }

    /// True if this marker spans a time range (vs. a single instant).
    #[must_use]
    pub fn is_span(&self) -> bool {
        self.tmax > self.tmin
    }

    /// First-motion glyph using pyrocko's ASCII symbol set (`+` up, `-` down,
    /// `0` undecided): the Unicode arrows snuffler prefers aren't in the bundled
    /// default font, and `+`/`-` is standard first-motion notation anyway.
    #[must_use]
    pub fn polarity_symbol(&self) -> &'static str {
        match self.polarity {
            Some(1) => "+",
            Some(-1) => "-",
            Some(0) => "0",
            _ => "",
        }
    }
}
