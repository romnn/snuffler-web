//! Theme identity and the behaviours a theme decides for the application: the
//! canvas polarity it renders on, when it colours the wiggle strokes, and whether
//! it mirrors snuffler's own colouring rules.
//!
//! Colours live with the renderer (`snuffler_web_render::themes`), keyed on the
//! identity here.
//! This module holds only what the application state has to carry, persist and act
//! on, so adding a theme is one variant here plus its palette there.

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
    /// This frontend's own: tinted blocks under strokes coloured by key.
    SnufflerWeb,
    /// Earthy: terracotta and ochre through olive and sage, with an aqua counterpoint.
    SnufflerWeb1,
    /// Cool: the muted theme, pale blue, teal and mauve washes.
    SnufflerWeb2,
    /// Sky: airy blues and violets over pale warms.
    SnufflerWeb3,
    /// Forest: lichen and jade greens with warm bark and a sky counterpoint.
    SnufflerWeb4,
    /// Sunset: roses and golds against horizon blue and lilac.
    SnufflerWeb5,
}

impl WiggleTheme {
    /// Themes in menu order.
    pub const ALL: [WiggleTheme; 7] = [
        WiggleTheme::SnufflerQt,
        WiggleTheme::SnufflerWeb,
        WiggleTheme::SnufflerWeb1,
        WiggleTheme::SnufflerWeb2,
        WiggleTheme::SnufflerWeb3,
        WiggleTheme::SnufflerWeb4,
        WiggleTheme::SnufflerWeb5,
    ];

    /// Returns the frontend label for this theme, which is also its persisted form.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            WiggleTheme::SnufflerQt => "Snuffler Qt",
            WiggleTheme::SnufflerWeb => "Snuffler Web",
            WiggleTheme::SnufflerWeb1 => "Snuffler Web 1",
            WiggleTheme::SnufflerWeb2 => "Snuffler Web 2",
            WiggleTheme::SnufflerWeb3 => "Snuffler Web 3",
            WiggleTheme::SnufflerWeb4 => "Snuffler Web 4",
            WiggleTheme::SnufflerWeb5 => "Snuffler Web 5",
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
            WiggleTheme::SnufflerWeb => "Tinted blocks, wiggles coloured by their key.",
            WiggleTheme::SnufflerWeb1 => {
                "Earthy: terracotta and ochre through olive and sage, with an aqua \
                 counterpoint; the wiggles take the colour when the boxes are off or \
                 a row groups several traces."
            }
            WiggleTheme::SnufflerWeb2 => {
                "Cool: the muted theme, pale blue, teal and mauve washes; the wiggles \
                 take the colour when the boxes are off or a row groups several \
                 traces."
            }
            WiggleTheme::SnufflerWeb3 => {
                "Sky: airy blues and violets over pale warms; the wiggles \
                 take the colour when the boxes are off or a row groups several \
                 traces."
            }
            WiggleTheme::SnufflerWeb4 => {
                "Forest: lichen and jade greens with warm bark and a sky counterpoint; the wiggles \
                 take the colour when the boxes are off or a row groups several \
                 traces."
            }
            WiggleTheme::SnufflerWeb5 => {
                "Sunset: roses and golds against horizon blue and lilac; the wiggles \
                 take the colour when the boxes are off or a row groups several \
                 traces."
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

    /// How this theme colours strokes, absent an explicit choice from the user.
    ///
    /// [`Self::SnufflerQt`] claims fidelity, so it boots monochrome as snuffler does.
    /// [`Self::SnufflerWeb`] is built around coloured strokes; the mood themes keep
    /// snuffler's single ink while the box colours carry the mood, and hand the
    /// colouring to the strokes exactly when the boxes stop separating the traces.
    #[must_use]
    pub const fn stroke_policy(self) -> StrokePolicy {
        match self {
            WiggleTheme::SnufflerQt => StrokePolicy::Monochrome,
            WiggleTheme::SnufflerWeb => StrokePolicy::Colored,
            WiggleTheme::SnufflerWeb1
            | WiggleTheme::SnufflerWeb2
            | WiggleTheme::SnufflerWeb3
            | WiggleTheme::SnufflerWeb4
            | WiggleTheme::SnufflerWeb5 => StrokePolicy::ColoredWhenBoxesCannotSeparate,
        }
    }

    /// The canvas polarity this theme pins the frontend to, if it has only one.
    ///
    /// Snuffler has no dark mode: its canvas is `QPalette.Base`, its traces
    /// `aluminium5`, its boxes the light Tango fills at alpha 100 — a palette built
    /// for white and only coherent on it.
    /// A dark variant of that theme would not be snuffler rendered differently, it
    /// would be a different theme wearing the name.
    /// Every web theme is designed for both canvases, each fitted on its own.
    /// A pinned theme takes the whole frontend with it, since a white canvas inside
    /// dark chrome is neither theme; the frontend disables the light/dark control
    /// while one is selected rather than letting the two disagree.
    #[must_use]
    pub const fn pinned_polarity(self) -> Option<Theme> {
        match self {
            WiggleTheme::SnufflerQt => Some(Theme::Light),
            WiggleTheme::SnufflerWeb
            | WiggleTheme::SnufflerWeb1
            | WiggleTheme::SnufflerWeb2
            | WiggleTheme::SnufflerWeb3
            | WiggleTheme::SnufflerWeb4
            | WiggleTheme::SnufflerWeb5 => None,
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
        Self::ALL.into_iter().find(|theme| theme.label() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_through_the_wire_form() {
        // The label is what the settings store persists, so every theme has to come
        // back from its own label and only from it.
        for theme in WiggleTheme::ALL {
            assert_eq!(WiggleTheme::from_wire(theme.label()), Some(theme));
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
