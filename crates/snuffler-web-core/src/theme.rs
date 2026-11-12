//! Theme identity and the behaviours a theme decides for the application: the
//! canvas polarity it renders on, when it colours the wiggle strokes, and whether
//! it mirrors snuffler's own colouring rules.
//!
//! Colours live with the renderer (`snuffler_web_render::themes`), keyed on the
//! identity here.
//! This module holds only what the application state has to carry, persist and act
//! on, so adding a theme needs its variant and availability entry here plus its palette there.

use crate::state::Subsort;

/// Canvas polarity: the light/dark preference the frontend resolves the app to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// Dark surfaces and light foregrounds.
    Dark,
    /// Light surfaces and dark foregrounds.
    Light,
}

impl Theme {
    /// Both polarities, for sweeps over every canvas a theme can render on.
    pub const ALL: [Theme; 2] = [Theme::Light, Theme::Dark];

    /// Maps a frontend-specific dark-mode flag into this vocabulary.
    #[must_use]
    pub const fn from_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }
}

/// When a theme colours the wiggle strokes by their coloring key, absent an explicit
/// choice from the user.
///
/// This is the theme's default for snuffler's Color Traces toggle, which the user
/// overrides by clicking it (`AppState::show_color_traces`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokePolicy {
    /// Never: every stroke is the theme's ink, the way snuffler boots (it adds Color
    /// Traces to its View menu without a `setChecked`, `pile_viewer.py:1076`).
    Monochrome,
    /// Always, so a coloring key reads at a glance under every sort.
    Colored,
    /// Only where the trace boxes cannot do the separating themselves: with the
    /// boxes hidden, or under a location-grouped subsort where several traces share
    /// one row and one box colour covers them all.
    /// The ink otherwise, so a theme that carries its palette in the boxes is not
    /// also carrying it in the wiggles.
    ColoredWhenBoxesCannotSeparate,
}

impl StrokePolicy {
    /// Whether strokes are coloured under `subsort`, given whether the trace boxes
    /// are drawn.
    #[must_use]
    pub const fn colors(self, subsort: Subsort, boxes_visible: bool) -> bool {
        match self {
            StrokePolicy::Monochrome => false,
            StrokePolicy::Colored => true,
            StrokePolicy::ColoredWhenBoxesCannotSeparate => {
                !boxes_visible || subsort.groups_locations()
            }
        }
    }
}

/// Visual treatment applied to time-domain traces.
///
/// Every theme keeps snuffler's colour *meanings* and changes only the colours: a
/// trace box is coloured by the pile's box style (`determine_box_styles`,
/// `pile_viewer.py:2947`), a coloured stroke by its coloring key, a marker by its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WiggleTheme {
    /// Snuffler's own look, reproduced: white canvas, Tango trace boxes, its
    /// `graph_colors` strokes.
    /// Light only, see [`Self::pinned_polarity`].
    SnufflerQt,
    /// Mineral blues, brick red and leaf green, balanced by quieter
    /// supporting inks.
    /// The one web theme that colours every stroke, see [`Self::stroke_policy`].
    Fieldbook,
    /// Fired clay and ochre, balanced by indigo, sage and glazed teal.
    Kiln,
    /// Slate blue and sea glass, with dry amber and heather counterpoints.
    Fjord,
    /// Periwinkle and soft gold, with coral, cloud blue and mint.
    Daybreak,
    /// Fern green, berry red and violet, with olive and dried-petal
    /// accents.
    Herbarium,
    /// Ember red and dusk blue, with rose, honey and muted green.
    Afterglow,
}

/// A theme's availability in the frontend and persisted settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WiggleThemeEntry {
    /// The palette and drawing policies this entry identifies.
    pub theme: WiggleTheme,
    /// Whether users can select or restore this theme.
    pub enabled: bool,
}

impl WiggleTheme {
    /// Theme definitions in menu order, including disabled palettes.
    pub const ALL: [WiggleThemeEntry; 7] = [
        WiggleThemeEntry {
            theme: WiggleTheme::SnufflerQt,
            enabled: true,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Fieldbook,
            enabled: true,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Kiln,
            enabled: false,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Fjord,
            enabled: false,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Daybreak,
            enabled: false,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Herbarium,
            enabled: false,
        },
        WiggleThemeEntry {
            theme: WiggleTheme::Afterglow,
            enabled: false,
        },
    ];

    /// All palettes, including disabled themes, for rendering and validation.
    pub fn all() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().map(|entry| entry.theme)
    }

    /// Whether this theme can be selected or restored from settings.
    #[must_use]
    pub fn enabled(self) -> bool {
        Self::ALL
            .into_iter()
            .any(|entry| entry.theme == self && entry.enabled)
    }

    /// Returns the frontend label for this theme, which is also its persisted form.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => "Snuffler Qt",
            WiggleTheme::Fieldbook => "Fieldbook",
            WiggleTheme::Kiln => "Kiln",
            WiggleTheme::Fjord => "Fjord",
            WiggleTheme::Daybreak => "Daybreak",
            WiggleTheme::Herbarium => "Herbarium",
            WiggleTheme::Afterglow => "Afterglow",
        }
    }

    /// One line on what the theme looks like and how it colours strokes, for the
    /// menu's hover text.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => {
                "Snuffler's own: white canvas, Tango boxes, grey wiggles unless Color \
                 traces is on. Light only."
            }
            WiggleTheme::Fieldbook => {
                "Balanced mineral blues, brick red and leaf green, with quieter supporting inks. \
                 Coloured strokes without trace boxes by default."
            }
            WiggleTheme::Kiln => {
                "Fired clay and ochre, balanced by indigo, sage and glazed teal. \
                 Strokes take colour when boxes are hidden or locations share a row."
            }
            WiggleTheme::Fjord => {
                "Slate blue and sea glass, with dry amber and heather counterpoints. \
                 Strokes take colour when boxes are hidden or locations share a row."
            }
            WiggleTheme::Daybreak => {
                "Periwinkle and soft gold, with coral, cloud blue and mint. \
                 Strokes take colour when boxes are hidden or locations share a row."
            }
            WiggleTheme::Herbarium => {
                "Fern green, berry red and violet, with olive and dried-petal accents. \
                 Strokes take colour when boxes are hidden or locations share a row."
            }
            WiggleTheme::Afterglow => {
                "Ember red and dusk blue, with rose, honey and muted green. \
                 Strokes take colour when boxes are hidden or locations share a row."
            }
        }
    }

    /// Whether this theme draws a rule between rows, absent an explicit choice from
    /// the user.
    /// Snuffler draws none, boxes or no boxes.
    #[must_use]
    pub const fn default_track_separators(self) -> bool {
        !matches!(self, WiggleTheme::SnufflerQt)
    }

    /// Whether trace boxes are drawn when the user has not chosen an override.
    /// Fieldbook leaves its coloured strokes on the plain canvas in both polarities.
    #[must_use]
    pub const fn default_boxes(self, theme: Theme) -> bool {
        !matches!(self, Self::Fieldbook) && matches!(theme, Theme::Light)
    }

    /// How this theme colours strokes, absent an explicit choice from the user.
    ///
    /// [`Self::SnufflerQt`] claims fidelity, so it boots monochrome as snuffler does.
    /// [`Self::Fieldbook`] is built around coloured strokes; the mood themes keep
    /// snuffler's single ink while the box colours carry the mood, and hand the
    /// colouring to the strokes exactly when the boxes stop separating the traces.
    #[must_use]
    pub const fn stroke_policy(self) -> StrokePolicy {
        match self {
            WiggleTheme::SnufflerQt => StrokePolicy::Monochrome,
            WiggleTheme::Fieldbook => StrokePolicy::Colored,
            WiggleTheme::Kiln
            | WiggleTheme::Fjord
            | WiggleTheme::Daybreak
            | WiggleTheme::Herbarium
            | WiggleTheme::Afterglow => StrokePolicy::ColoredWhenBoxesCannotSeparate,
        }
    }

    /// The canvas polarity this theme pins the frontend to, if it has only one.
    ///
    /// Snuffler has no dark mode: its canvas is `QPalette.Base`, its traces
    /// `aluminium5`, its boxes the light Tango fills at alpha 100 — a palette built
    /// for white and only coherent on it.
    /// A dark variant of that theme would not be snuffler rendered differently, it
    /// would be a different theme wearing the name.
    /// Every web theme has tones for both canvases, fitted jointly around one hue order.
    /// A pinned theme takes the whole frontend with it, since a white canvas inside
    /// dark chrome is neither theme; the frontend disables the light/dark control
    /// while one is selected rather than letting the two disagree.
    #[must_use]
    pub const fn pinned_polarity(self) -> Option<Theme> {
        match self {
            WiggleTheme::SnufflerQt => Some(Theme::Light),
            WiggleTheme::Fieldbook
            | WiggleTheme::Kiln
            | WiggleTheme::Fjord
            | WiggleTheme::Daybreak
            | WiggleTheme::Herbarium
            | WiggleTheme::Afterglow => None,
        }
    }

    /// Whether this theme reproduces snuffler's own colouring *rules* rather than
    /// this frontend's: strokes past the seventh coloring key go black rather than
    /// wrapping (`pyrocko.plot.color`), and an empty coloring key stays uncoloured
    /// rather than falling back to the track label.
    #[must_use]
    pub const fn mirrors_qt_coloring(self) -> bool {
        matches!(self, WiggleTheme::SnufflerQt)
    }

    /// The inverse of [`WiggleTheme::label`], for a theme restored from the
    /// persisted settings.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        // Settings persist labels, so the themes' former numbered labels still restore.
        let former_label = match value {
            "Snuffler Web" => Some(Self::Fieldbook),
            "Snuffler Web 1" => Some(Self::Kiln),
            "Snuffler Web 2" => Some(Self::Fjord),
            "Snuffler Web 3" => Some(Self::Daybreak),
            "Snuffler Web 4" => Some(Self::Herbarium),
            "Snuffler Web 5" => Some(Self::Afterglow),
            _ => None,
        };
        former_label.or_else(|| Self::all().find(|theme| theme.label() == value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fieldbook_and_snuffler_qt_are_enabled() {
        let enabled: Vec<_> = WiggleTheme::all().filter(|theme| theme.enabled()).collect();
        assert_eq!(enabled, [WiggleTheme::SnufflerQt, WiggleTheme::Fieldbook]);
    }

    #[test]
    fn labels_round_trip_through_the_wire_form() {
        // The label is what the settings store persists, so every theme has to come
        // back from its own label.
        for theme in WiggleTheme::all() {
            assert_eq!(WiggleTheme::from_wire(theme.label()), Some(theme));
        }
        for (old, theme) in [
            ("Snuffler Web", WiggleTheme::Fieldbook),
            ("Snuffler Web 1", WiggleTheme::Kiln),
            ("Snuffler Web 2", WiggleTheme::Fjord),
            ("Snuffler Web 3", WiggleTheme::Daybreak),
            ("Snuffler Web 4", WiggleTheme::Herbarium),
            ("Snuffler Web 5", WiggleTheme::Afterglow),
        ] {
            assert_eq!(WiggleTheme::from_wire(old), Some(theme));
        }
        assert_eq!(WiggleTheme::from_wire("Snuffler Web 9"), None);
    }

    #[test]
    fn boxes_policy_colours_only_where_the_boxes_stop_separating() {
        // With the boxes drawn and one trace to a row, the box colour is the class
        // and the wiggle stays snuffler's ink.
        let policy = StrokePolicy::ColoredWhenBoxesCannotSeparate;
        let (boxes, no_boxes) = (true, false);
        assert!(!policy.colors(Subsort::Nslc, boxes));
        assert!(!policy.colors(Subsort::Cnsl, boxes));

        // A grouped subsort puts several traces under one box colour, so the
        // strokes take the class over.
        assert!(policy.colors(Subsort::NscGroupL, boxes));
        assert!(policy.colors(Subsort::SncGroupL, boxes));

        // Hiding the boxes removes the only thing carrying the class, under every
        // subsort, so the strokes take it over there too.
        assert!(policy.colors(Subsort::Nslc, no_boxes));
        assert!(policy.colors(Subsort::NscGroupL, no_boxes));

        // The other two policies answer the same however the view is set up: the Qt
        // theme keeps its grey wiggle with the boxes off, as snuffler does.
        assert!(!StrokePolicy::Monochrome.colors(Subsort::NscGroupL, no_boxes));
        assert!(StrokePolicy::Colored.colors(Subsort::Nslc, boxes));
    }
}
