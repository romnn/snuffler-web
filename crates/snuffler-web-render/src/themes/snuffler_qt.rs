//! Snuffler Qt: snuffler's own look, reproduced number for number.
//!
//! The canvas is `QPalette.Base` white, the trace pen and axis `aluminium5`, the
//! boxes `box_styles` (`pile_viewer.py:198`) and the strokes `graph_colors`
//! (plot/__init__.py:95); markers are `g_color_b` (marker.py:53).
//! Snuffler only ever draws these on white and the pairing is built for it — a pale
//! wash under a dark outline — so the theme is light only
//! (`WiggleTheme::pinned_polarity`).

use ecolor::Color32;
use snuffler_web_core::markers::MarkerPalette;

use super::{Rgb, rgb};
use snuffler_web_core::theme::Theme;

use crate::palette::{BoxStyle, Palette};

/// `graph_colors`: scarletred2, skyblue3, chameleon3, orange2, plum2, chocolate2,
/// butter2, in that order.
///
/// Past the seventh key `pyrocko.plot.color` returns black rather than wrapping,
/// which the painter applies from `WiggleTheme::mirrors_qt_coloring`.
const GRAPH_COLORS: [Rgb; 7] = [
    (204, 0, 0),
    (32, 74, 135),
    (78, 154, 6),
    (245, 121, 0),
    (117, 80, 123),
    (193, 125, 17),
    (237, 212, 0),
];

/// Snuffler's `box_alpha`: the fill's alpha over the white canvas.
const BOX_ALPHA: u8 = 100;

/// `box_styles`: the light Tango "1" fill at `box_alpha`, outlined in the matching
/// "3", in snuffler's own order — orange, skyblue, butter, chameleon, chocolate,
/// plum, scarletred.
const BOX_STYLES: [(Rgb, Rgb); 7] = [
    ((252, 175, 62), (206, 92, 0)),
    ((114, 159, 207), (32, 74, 135)),
    ((252, 233, 79), (196, 160, 0)),
    ((138, 226, 52), (78, 154, 6)),
    ((233, 185, 110), (143, 89, 2)),
    ((173, 127, 168), (92, 53, 102)),
    ((239, 41, 41), (164, 0, 0)),
];

/// Snuffler's palette, white canvas only.
#[must_use]
pub fn palette() -> Palette {
    let aluminium5 = rgb((85, 87, 83));
    Palette {
        theme: Theme::Light,
        background: Color32::WHITE,
        trace: aluminium5,
        // The zero line is a data reference and the grid is decoration, so the zero
        // line always carries at least the grid's contrast.
        // Here it is snuffler's own: Qt draws the zero line with the current trace
        // pen (`pile_viewer.py:3431`), aluminium5 by default.
        grid: rgb((236, 236, 236)),
        zero: aluminium5,
        axis_text: aluminium5,
        separator: rgb((214, 214, 214)),
        // The white-at-alpha-100 label brush (`pile_viewer.py:3230`), unpadded and
        // square as `draw_label` fills it.
        label_background: Color32::from_rgba_unmultiplied(255, 255, 255, 100),
        label_padding: 0.0,
        label_rounding: 0.0,
        cursor: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
        strokes: GRAPH_COLORS.map(rgb),
        boxes: BOX_STYLES.map(|(fill, frame)| BoxStyle {
            fill: Color32::from_rgba_unmultiplied(fill.0, fill.1, fill.2, BOX_ALPHA),
            frame: Some(rgb(frame)),
        }),
        markers: MarkerPalette::SNUFFLER_QT,
        // Qt's `draw_label` paints a marker label on white whatever lies beneath
        // (`QColor(255, 255, 255)`, gui/util.py:293).
        marker_label_background: Color32::WHITE,
    }
}
