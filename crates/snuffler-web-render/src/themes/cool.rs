//! Fjord, the palette behind [`snuffler_web_core::theme::WiggleTheme::SnufflerWeb2`].
//!
//! Slate blue and sea glass, with dry amber and heather counterpoints.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A slate grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x53, 0x56, 0x62);

/// The cool hues as washes on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xB1, 0xD1, 0xF2),
    (0xF7, 0xD1, 0xB8),
    (0xBF, 0xE6, 0xCB),
    (0xE1, 0xDA, 0xFF),
    (0xC9, 0xF6, 0xFC),
    (0xE5, 0xBE, 0xCC),
    (0xF1, 0xEF, 0xCA),
];

/// The same hues as inks, dark enough to read over any wash.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x01, 0x62, 0xAA),
    (0x95, 0x4C, 0x04),
    (0x04, 0x56, 0x2F),
    (0x59, 0x44, 0x8A),
    (0x0F, 0x68, 0x70),
    (0x8B, 0x46, 0x63),
    (0x61, 0x5B, 0x01),
];

/// A cool off-white, the ink of the dark canvas.
const DARK_INK: Rgb = (0xDE, 0xE4, 0xF0);

/// The same hues as washes on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x20, 0x3B, 0x57),
    (0x43, 0x26, 0x11),
    (0x19, 0x3C, 0x27),
    (0x4A, 0x42, 0x63),
    (0x25, 0x4F, 0x53),
    (0x4E, 0x30, 0x3C),
    (0x46, 0x43, 0x24),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x56, 0xA4, 0xF2),
    (0xFA, 0xAE, 0x7A),
    (0x5E, 0xBC, 0x83),
    (0xB8, 0xA5, 0xF2),
    (0x62, 0xC6, 0xD1),
    (0xED, 0x9D, 0xBB),
    (0xBC, 0xB6, 0x68),
];

/// The palette for `theme`.
#[must_use]
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Light => canvas(theme)
            .with_ink(LIGHT_INK)
            .palette(LIGHT_STROKES, boxes(LIGHT_FILLS)),
        Theme::Dark => canvas(theme)
            .with_ink(DARK_INK)
            .palette(DARK_STROKES, boxes(DARK_FILLS)),
    }
}
