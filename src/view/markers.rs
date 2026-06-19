//! Marker model + colors, mirroring pyrocko `gui/snuffler/marker.py`.
//!
//! A marker is a time (or time span) annotation, optionally tied to specific
//! NSLC channels, optionally a named seismic phase. `kind` (0..=7) selects a
//! color, exactly like snuffler's number-key colouring.

use egui::Color32;

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

#[derive(Clone, Debug)]
#[allow(dead_code)] // nslc/is_event support channel-scoped & event markers
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
    pub selected: bool,
}

impl Marker {
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
        }
    }

    pub fn color(&self) -> Color32 {
        MARKER_COLORS[(self.kind & 7) as usize]
    }

    /// True if this marker spans a time range (vs. a single instant).
    pub fn is_span(&self) -> bool {
        self.tmax > self.tmin
    }

    /// First-motion glyph using pyrocko's ASCII symbol set (`+` up, `-` down,
    /// `0` undecided): the Unicode arrows snuffler prefers aren't in egui's
    /// default font, and `+`/`-` is standard first-motion notation anyway.
    pub fn polarity_symbol(&self) -> &'static str {
        match self.polarity {
            Some(1) => "+",
            Some(-1) => "-",
            Some(0) => "0",
            _ => "",
        }
    }
}
