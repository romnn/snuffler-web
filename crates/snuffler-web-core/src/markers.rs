//! Marker model + colors, mirroring pyrocko `gui/snuffler/marker.py`.
//!
//! A marker is a time (or time span) annotation, optionally tied to specific NSLC
//! channels, optionally a named seismic phase.
//! `kind` (0..=7) selects a color, exactly like snuffler's number-key colouring.

use ecolor::Color32;

/// The colours a theme gives the eight marker kinds.
///
/// Snuffler colours a marker by its `kind` alone, and a marker reads as selected
/// or hovered by changing *shade* within its kind's hue, never by changing hue
/// (`select_color`, marker.py:306).
/// Every palette keeps that contract: index `kind` is the same colour family on
/// every theme (0 red, 1 green, 2 blue, 3 orange, 4 purple, 5 brown, 6 yellow,
/// 7 grey), so a kind-2 pick is blue whatever the canvas, and only the shades
/// move to suit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerPalette {
    /// What a marker wears when it is neither selected nor hovered.
    pub idle: [Color32; 8],
    /// The shade a selected or hovered marker lifts to.
    pub lit: [Color32; 8],
}

impl MarkerPalette {
    /// Snuffler's own palette (`g_color_b`, marker.py:53): the Tango "3" shades
    /// idle (`colorlist[kind*3 + 2]`) and the "2" shades lit
    /// (`colorlist[kind*3 + 1]`).
    ///
    /// Built for snuffler's white canvas and only coherent on it: skyblue3 and
    /// plum3 sit below 2:1 against a dark one.
    pub const SNUFFLER_QT: Self = Self {
        idle: [
            Color32::from_rgb(164, 0, 0),   // 0 scarletred3
            Color32::from_rgb(78, 154, 6),  // 1 chameleon3
            Color32::from_rgb(32, 74, 135), // 2 skyblue3
            Color32::from_rgb(206, 92, 0),  // 3 orange3
            Color32::from_rgb(92, 53, 102), // 4 plum3
            Color32::from_rgb(143, 89, 2),  // 5 chocolate3
            Color32::from_rgb(196, 160, 0), // 6 butter3
            Color32::from_rgb(85, 87, 83),  // 7 aluminium5
        ],
        lit: [
            Color32::from_rgb(204, 0, 0),     // 0 scarletred2
            Color32::from_rgb(115, 210, 22),  // 1 chameleon2
            Color32::from_rgb(52, 101, 164),  // 2 skyblue2
            Color32::from_rgb(245, 121, 0),   // 3 orange2
            Color32::from_rgb(117, 80, 123),  // 4 plum2
            Color32::from_rgb(193, 125, 17),  // 5 chocolate2
            Color32::from_rgb(237, 212, 0),   // 6 butter2
            Color32::from_rgb(136, 138, 133), // 7 aluminium4
        ],
    };

    /// The Snuffler Web palette for its light canvas.
    ///
    /// Each kind is fitted in `OKLab` at the hue angle of its Tango family, so a
    /// kind keeps the colour it has in snuffler; its lightness and chroma are what
    /// the fit chooses.
    /// What the fit maximises is the closest of the 28 pairs, because that is what
    /// decides whether a kind can be read off its colour at all: it holds 14.4 here,
    /// where snuffler's own shades manage 10.7.
    /// Idle shades hold 3.1-11.3:1 against the paper and lit ones 2.3-7.3:1.
    /// A lit shade may sit lower than the idle one it replaces because on white
    /// lighter *is* less contrast; selection reads as a brightening of 1.35-1.57
    /// against the kind's own idle shade, which is the signal snuffler gives too
    /// (its butter2 sits at 1.5 against the paper).
    pub const SNUFFLER_WEB_LIGHT: Self = Self {
        idle: [
            Color32::from_rgb(0xD3, 0x3E, 0x31), // 0 red
            Color32::from_rgb(0x2D, 0x5A, 0x0B), // 1 green
            Color32::from_rgb(0x27, 0x66, 0xBE), // 2 blue
            Color32::from_rgb(0x64, 0x29, 0x00), // 3 orange
            Color32::from_rgb(0xBB, 0x65, 0xD1), // 4 purple
            Color32::from_rgb(0xCD, 0x81, 0x00), // 5 brown
            Color32::from_rgb(0x85, 0x6B, 0x00), // 6 yellow
            Color32::from_rgb(0x8B, 0x8B, 0x8B), // 7 grey
        ],
        lit: [
            Color32::from_rgb(0xFA, 0x4F, 0x3F),
            Color32::from_rgb(0x37, 0x72, 0x00),
            Color32::from_rgb(0x48, 0x87, 0xE3),
            Color32::from_rgb(0x91, 0x3E, 0x00),
            Color32::from_rgb(0xD9, 0x7A, 0xF1),
            Color32::from_rgb(0xEF, 0x97, 0x00),
            Color32::from_rgb(0x9E, 0x80, 0x00),
            Color32::from_rgb(0xA1, 0xA4, 0x9E),
        ],
    };

    /// The Snuffler Web palette for its dark canvas: the same families, lifted.
    ///
    /// Idle shades hold 4.3-12.5:1 against the dark canvas and lit ones 5.9-15.4:1,
    /// and the closest of the 28 pairs is 16.2.
    /// Selection brightens by 1.22-1.49: on a dark ground "lighter" soon runs into
    /// white, where every kind ends up the same pale tint, so the lit shades lift
    /// less than on paper and keep their chroma instead.
    pub const SNUFFLER_WEB_DARK: Self = Self {
        idle: [
            Color32::from_rgb(0xFF, 0x84, 0x74), // 0 red
            Color32::from_rgb(0x88, 0xDC, 0x53), // 1 green
            Color32::from_rgb(0x61, 0x96, 0xE6), // 2 blue
            Color32::from_rgb(0xC8, 0x59, 0x00), // 3 orange
            Color32::from_rgb(0xE3, 0x9E, 0xF5), // 4 purple
            Color32::from_rgb(0xFF, 0xCB, 0x91), // 5 brown
            Color32::from_rgb(0xC1, 0x9E, 0x00), // 6 yellow
            Color32::from_rgb(0x79, 0x7C, 0x77), // 7 grey
        ],
        lit: [
            Color32::from_rgb(0xFF, 0xA1, 0x92),
            Color32::from_rgb(0xB0, 0xF6, 0x8A),
            Color32::from_rgb(0x7D, 0xB1, 0xFF),
            Color32::from_rgb(0xE5, 0x6F, 0x24),
            Color32::from_rgb(0xF2, 0xBE, 0xFF),
            Color32::from_rgb(0xFF, 0xE6, 0xCA),
            Color32::from_rgb(0xE4, 0xBA, 0x00),
            Color32::from_rgb(0x97, 0x9A, 0x95),
        ],
    };

    /// The colour of a marker kind, idle or lit.
    ///
    /// `kind & 7` always lands inside the palette, so every kind has a colour and
    /// the fallback is unreachable.
    #[must_use]
    pub fn kind(&self, kind: u8, lit: bool) -> Color32 {
        let table = if lit { &self.lit } else { &self.idle };
        table
            .get(usize::from(kind & 7))
            .copied()
            .unwrap_or(Color32::from_rgb(204, 0, 0))
    }
}

/// Phase names bound to F1..=F9: snuffler's `default_phase_key_mapping`
/// (config.py:33) with its own `Undefined` fall-through for the unmapped keys.
/// Qt reads the map from `~/.pyrocko/snuffler.pf`, which a browser cannot; this
/// is the same built-in default a fresh snuffler ships, so a pick made with F3
/// carries the same name — and goes into the same marker file — on both sides.
pub const PHASE_KEYS: [&str; 9] = [
    "P",
    "S",
    "R",
    "Q",
    "?",
    "Undefined",
    "Undefined",
    "Undefined",
    "Undefined",
];

/// A time annotation, optional seismic phase pick, or catalog event.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    /// Start time (epoch seconds).
    pub tmin: f64,
    /// End time (== tmin for an instantaneous pick).
    pub tmax: f64,
    /// Color group 0..=7.
    pub kind: u8,
    /// The channels the marker applies to — a *list*, as pyrocko's `nslc_ids` is
    /// (`marker.py:168`): a Qt pick binds to every channel gathered onto the clicked
    /// track, and a loaded file legally carries several.
    /// Empty = applies to all tracks.
    /// Elements may contain shell wildcards.
    pub nslc_ids: Vec<(String, String, String, String)>,
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
    /// Phase-pick time uncertainty (seconds); drawn as translucent ± brackets around the
    /// pick.
    /// Set by shift-dragging on a selected phase marker (mirrors pyrocko
    /// `PhaseMarker._uncertainty`); `None`/0 hides the brackets.
    pub uncertainty: Option<f64>,
    /// Catalog/loaded event identity (matches the Python event registry), used to
    /// pick this event as the active reference and to key Kagan updates.
    pub event_hash: Option<String>,
    /// Event magnitude, or `None` on non-event markers.
    pub magnitude: Option<f64>,
    /// Event depth in kilometres, or `None` on non-event markers.
    pub depth_km: Option<f64>,
    /// Event latitude in degrees north, or `None` on non-event markers.
    pub lat: Option<f64>,
    /// Event longitude in degrees east, or `None` on non-event markers.
    pub lon: Option<f64>,
    /// The single active event marker (the distance/Kagan/cake/seismosizer reference);
    /// exactly one marker is active at a time.
    /// Set by 'e', cleared by 'E'.
    pub active: bool,
    /// `PhaseMarker._automatic`: machine-made picks carry `Some(true)` and wear an `@` in
    /// their label; a hand pick saves `False`, never `None` (`marker.py:707`).
    /// `None` on plain and event markers.
    pub automatic: Option<bool>,
    /// Phase incidence angle (degrees); the `SeisBench` picker rides the pick
    /// probability here, exactly as Qt's does (`seisbench_picker.py:341`).
    pub incidence_angle: Option<f64>,
    /// Phase take-off angle (degrees).
    pub takeoff_angle: Option<f64>,
    /// The catalog `Event.name` — what Qt writes into a saved event row and
    /// composes the caption from; the display label in `phase` is derived.
    pub event_name: Option<String>,
    /// The event's `region` field, second element of Qt's caption ladder.
    pub region: Option<String>,
    /// The event's `catalog` field (e.g. the FDSN datacenter).
    pub catalog: Option<String>,
    /// The associated event's origin time — pyrocko's association fallback when the hash
    /// is absent (`associate_phases_to_events`).
    /// On phase picks only.
    pub event_time: Option<f64>,
    /// Distance from this *event* to the active event, in km — the marker table's
    /// `Dist [km]` (`marker_editor.py:502`), precomputed in Python with pyrocko's
    /// orthodrome.
    /// `None` on non-events and while nothing is active.
    pub dist_active_km: Option<f64>,
    /// The snuffling that produced this marker, when one did — a re-run of the same
    /// snuffling replaces its previous markers, Qt's `cleanup()` contract
    /// (snuffling.py:1689).
    /// `None` on user picks and loaded files, which accumulate.
    pub origin: Option<String>,
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
    /// Distance from this event to the active event.
    Dist,
    /// Phase-pick time uncertainty in seconds.
    Uncert,
    /// The event's identity hash.
    Hash,
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

/// Parse a `:markers ...` command, mirroring snuffler `pile_viewer.py`'s `markers` arm:
/// `markers all` shows kinds 0..=7; `markers <digits>` shows exactly the digit-kinds in
/// the single token; bare `markers` hides all; anything else is a no-op.
/// Returns `None` when the line is not a `markers` command.
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
            nslc_ids: Vec::new(),
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
            automatic: None,
            incidence_angle: None,
            takeoff_angle: None,
            event_name: None,
            region: None,
            catalog: None,
            event_time: None,
            dist_active_km: None,
            origin: None,
        }
    }

    /// Composes an event caption from magnitude, region, and raw event name.
    ///
    /// Fields are space-joined in Qt's order, with `(Event)` when all three are
    /// absent (`EventMarker.label`, `marker.py:533`).
    #[must_use]
    pub fn event_label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(magnitude) = self.magnitude {
            parts.push(format!("M{magnitude:3.1}"));
        }
        for value in [&self.region, &self.event_name] {
            if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
                parts.push(value.to_owned());
            }
        }
        if parts.is_empty() {
            "(Event)".to_owned()
        } else {
            parts.join(" ")
        }
    }

    /// The marker's colour in snuffler's state semantics (`select_color`,
    /// marker.py:306): the palette's lit shade when selected or hovered, its idle
    /// shade otherwise.
    #[must_use]
    pub fn color_in(&self, palette: &MarkerPalette, hovered: bool) -> Color32 {
        palette.kind(self.kind, self.selected || hovered)
    }

    /// True if this marker spans a time range (vs. a single instant).
    #[must_use]
    pub fn is_span(&self) -> bool {
        self.tmax > self.tmin
    }

    /// The status-line text snuffler posts while a marker is hovered (`hoover_message`,
    /// marker.py:583, :779) — the only way to read a marker's metadata without opening
    /// the table: an event's origin fields, or a phase pick's angles and polarity, in
    /// Qt's field order.
    /// A field the marker does not carry simply does not appear.
    /// Empty for a plain marker.
    #[must_use]
    pub fn hover_message(&self) -> String {
        let mut toks: Vec<String> = Vec::new();
        if self.is_event {
            // Qt's field order: magnitude lat lon depth name region catalog.
            if let Some(v) = self.magnitude {
                toks.push(format!("magnitude = {v}"));
            }
            if let Some(v) = self.lat {
                toks.push(format!("lat = {v}"));
            }
            if let Some(v) = self.lon {
                toks.push(format!("lon = {v}"));
            }
            if let Some(v) = self.depth_km {
                toks.push(format!("depth = {v} km"));
            }
            for (key, v) in [
                ("name", &self.event_name),
                ("region", &self.region),
                ("catalog", &self.catalog),
            ] {
                if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                    toks.push(format!("{key} = {v}"));
                }
            }
        } else if self.phase.is_some() {
            // Qt's PhaseMarker order: incidence_angle, takeoff_angle, polarity.
            if let Some(v) = self.incidence_angle {
                toks.push(format!("incidence_angle = {v}"));
            }
            if let Some(v) = self.takeoff_angle {
                toks.push(format!("takeoff_angle = {v}"));
            }
            if let Some(p) = self.polarity {
                toks.push(format!("polarity = {p}"));
            }
        }
        toks.join(", ")
    }

    /// First-motion glyph, snuffler's own arrows (`polarity_symbols`,
    /// `marker.py:21`): ↑ up, ↓ down, ↕ undecided.
    /// The bundled `JetBrains Mono` fallback covers all three.
    #[must_use]
    pub fn polarity_symbol(&self) -> &'static str {
        match self.polarity {
            Some(1) => "\u{2191}",
            Some(-1) => "\u{2193}",
            Some(0) => "\u{2195}",
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// snuffler's `g_color_b` (marker.py:51): the Tango triples its marker
    /// palette indexes into with `colorlist[(kind*3 + x) % 24]`, transcribed
    /// from pyrocko's `color.g_tango_colors`.
    const G_COLOR_B: [(u8, u8, u8); 24] = [
        (239, 41, 41),
        (204, 0, 0),
        (164, 0, 0),
        (138, 226, 52),
        (115, 210, 22),
        (78, 154, 6),
        (114, 159, 207),
        (52, 101, 164),
        (32, 74, 135),
        (252, 175, 62),
        (245, 121, 0),
        (206, 92, 0),
        (173, 127, 168),
        (117, 80, 123),
        (92, 53, 102),
        (233, 185, 110),
        (193, 125, 17),
        (143, 89, 2),
        (252, 233, 79),
        (237, 212, 0),
        (196, 160, 0),
        (186, 189, 182),
        (136, 138, 133),
        (85, 87, 83),
    ];

    // The Snuffler Qt palette is exactly Qt's `select_color` arithmetic over
    // `g_color_b` — `kind*3 + 1` when selected or hovered, `kind*3 + 2`
    // otherwise.
    #[test]
    fn the_qt_marker_palette_is_select_color_over_g_color_b() {
        let palette = MarkerPalette::SNUFFLER_QT;
        for (kind, (sel, idle)) in palette.lit.iter().zip(&palette.idle).enumerate() {
            assert_eq!(
                G_COLOR_B.get(kind * 3 + 1).copied(),
                Some((sel.r(), sel.g(), sel.b())),
                "selected shade of kind {kind}"
            );
            assert_eq!(
                G_COLOR_B.get(kind * 3 + 2).copied(),
                Some((idle.r(), idle.g(), idle.b())),
                "idle shade of kind {kind}"
            );
        }
    }

    #[test]
    fn color_in_reads_marker_state_the_way_select_color_does() {
        let palette = MarkerPalette::SNUFFLER_QT;
        let mut m = Marker::new(0.0, 0.0, 3);
        // Idle markers wear the darker "3" shade; hovering (Qt's `alerted`) or
        // selecting brightens to the "2" shade — the shade change is how a
        // selected marker reads as selected.
        assert_eq!(m.color_in(&palette, false), Color32::from_rgb(206, 92, 0));
        assert_eq!(m.color_in(&palette, true), Color32::from_rgb(245, 121, 0));
        m.selected = true;
        assert_eq!(m.color_in(&palette, false), Color32::from_rgb(245, 121, 0));
    }

    // The F1..=F9 phase names are snuffler's `default_phase_key_mapping`
    // (config.py:33) with its `Undefined` fall-through — a pick made with F3
    // carries the same name into the same marker file on both sides
    // (`phase-key-names` in tests/parity/known_gaps.toml).
    #[test]
    fn phase_keys_follow_snufflers_default_mapping() {
        assert_eq!(
            PHASE_KEYS,
            [
                "P",
                "S",
                "R",
                "Q",
                "?",
                "Undefined",
                "Undefined",
                "Undefined",
                "Undefined"
            ]
        );
    }

    #[test]
    fn polarity_symbols_are_qt_arrows() {
        let mut m = Marker::new(0.0, 0.0, 0);
        m.polarity = Some(1);
        assert_eq!(m.polarity_symbol(), "\u{2191}");
        m.polarity = Some(-1);
        assert_eq!(m.polarity_symbol(), "\u{2193}");
        m.polarity = Some(0);
        assert_eq!(m.polarity_symbol(), "\u{2195}");
        m.polarity = None;
        assert_eq!(m.polarity_symbol(), "");
    }

    #[test]
    fn event_label_composes_qts_caption_fields() {
        let mut marker = Marker::new(0.0, 0.0, 0);
        marker.is_event = true;
        assert_eq!(marker.event_label(), "(Event)");

        marker.magnitude = Some(4.25);
        marker.region = Some("Eifel".to_owned());
        marker.event_name = Some("event-a".to_owned());
        assert_eq!(marker.event_label(), "M4.2 Eifel event-a");
    }

    #[test]
    fn hover_message_reads_event_origin_or_pick_polarity() {
        let mut ev = Marker::new(10.0, 10.0, 0);
        ev.is_event = true;
        ev.magnitude = Some(5.5);
        ev.lat = Some(27.2);
        ev.lon = Some(142.8);
        ev.depth_km = Some(12.0);
        // The event message lists the origin fields in Qt's order.
        assert_eq!(
            ev.hover_message(),
            "magnitude = 5.5, lat = 27.2, lon = 142.8, depth = 12 km"
        );

        let mut pick = Marker::new(10.0, 10.0, 0);
        pick.phase = Some("P".to_owned());
        pick.polarity = Some(1);
        assert_eq!(pick.hover_message(), "polarity = 1");

        // A plain marker has no metadata to show.
        assert_eq!(Marker::new(10.0, 10.0, 0).hover_message(), "");
    }
}
