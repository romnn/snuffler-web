//! The colours one waveform paint pass draws with, and the theme that picks them.

use ecolor::Color32;
use snuffler_web_core::markers::MarkerPalette;
pub use snuffler_web_core::theme::Theme;
use snuffler_web_core::theme::WiggleTheme;

use crate::themes;

/// One of snuffler's seven trace-box styles: the fill behind a channel's data and,
/// for the Qt theme, the outline around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxStyle {
    /// The fill: translucent under the Qt theme (Tango "1" at alpha 100 over white)
    /// and opaque under the web themes, whose fills are fitted against their canvas.
    pub fill: Color32,
    /// The one-pixel outline, or `None`.
    ///
    /// Only snuffler's own boxes carry one; the web themes are bare fills, since a
    /// border on every row is more line than the wiggle it frames.
    pub frame: Option<Color32>,
}

/// Colors used by one waveform paint pass.
///
/// A theme is this data and nothing else: the painter reads the palette and never
/// asks which theme produced it, so every theme is drawn by the same code.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The canvas polarity this palette was fitted for.
    pub theme: Theme,
    /// Canvas background.
    pub background: Color32,
    /// Default waveform stroke.
    pub trace: Color32,
    /// Time-grid lines.
    pub grid: Color32,
    /// Zero-amplitude reference lines.
    pub zero: Color32,
    /// Axis and tick labels.
    pub axis_text: Color32,
    /// Track separators.
    pub separator: Color32,
    /// Background behind track labels.
    pub label_background: Color32,
    /// Padding around a track label's text.
    ///
    /// Snuffler fills the text's own bounding rect, unpadded (`draw_label`, gui/util.py:294),
    /// which is why the Qt theme carries zero: a plate wider than its text reads as a box drawn
    /// over the trace rather than as a backing the label sits on.
    pub label_padding: f32,
    /// Corner radius of that plate — square for snuffler's, which draws a plain
    /// `fillRect`.
    pub label_rounding: f32,
    /// Pointer crosshair.
    pub cursor: Color32,
    /// The ramp Color Traces numbers the sorted coloring keys into.
    pub strokes: [Color32; 7],
    /// The trace-box styles, indexed by the pile's box style
    /// (`determine_box_styles`, `pile_viewer.py:2947`): one per contiguous run of a
    /// channel's data, stepping at every channel change and gap.
    pub boxes: [BoxStyle; 7],
    /// The marker kinds' colours.
    pub markers: MarkerPalette,
    /// The plate under a marker label.
    ///
    /// Qt paints it white whatever lies beneath (`draw_label`, gui/util.py:293), so
    /// a label stays readable over any wiggle.
    /// The web themes paint it in their canvas colour instead: their marker shades
    /// are chosen against that canvas, and a white plate would erase a light
    /// marker on a dark one.
    pub marker_label_background: Color32,
}

impl Palette {
    /// The box style for a pile style index, wrapping past the seventh the way
    /// snuffler's `box_styles[istyle % len(box_styles)]` does.
    #[must_use]
    pub fn box_style(&self, style: usize) -> BoxStyle {
        // The modulo keeps the fetch inside the array; the fallback is unreachable.
        self.boxes
            .get(style % self.boxes.len())
            .copied()
            .unwrap_or(BoxStyle {
                fill: self.background,
                frame: None,
            })
    }

    /// The stroke for a numbered coloring key, wrapping past the seventh.
    ///
    /// The Qt theme does not wrap — `pyrocko.plot.color` goes black past its list —
    /// which the painter handles from [`WiggleTheme::mirrors_qt_coloring`].
    #[must_use]
    pub fn stroke(&self, index: usize) -> Color32 {
        self.strokes
            .get(index % self.strokes.len())
            .copied()
            .unwrap_or(self.trace)
    }
}

/// The canvas polarity a wiggle theme actually renders on.
///
/// Usually the app's own light/dark preference, but a theme may pin it (see
/// [`WiggleTheme::pinned_polarity`]), and the frontend disables the light/dark
/// control while such a theme is selected rather than letting the two disagree.
#[must_use]
pub fn canvas_theme(preference: Theme, wiggle: WiggleTheme) -> Theme {
    wiggle.pinned_polarity().unwrap_or(preference)
}

/// Selects the waveform palette for the frontend color scheme and wiggle theme.
///
/// A pinned theme returns its one palette whatever `theme` says, so a caller that
/// has not gone through [`canvas_theme`] still gets the canvas the theme owns.
#[must_use]
pub fn palette(theme: Theme, wiggle: WiggleTheme) -> Palette {
    let theme = canvas_theme(theme, wiggle);
    match wiggle {
        WiggleTheme::SnufflerQt => themes::snuffler_qt::palette(),
        WiggleTheme::Fieldbook => themes::fieldbook::palette(theme),
        WiggleTheme::Kiln => themes::kiln::palette(theme),
        WiggleTheme::Fjord => themes::fjord::palette(theme),
        WiggleTheme::Daybreak => themes::daybreak::palette(theme),
        WiggleTheme::Herbarium => themes::herbarium::palette(theme),
        WiggleTheme::Afterglow => themes::afterglow::palette(theme),
    }
}
