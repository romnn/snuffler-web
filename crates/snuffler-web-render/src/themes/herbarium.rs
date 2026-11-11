//! Herbarium, the palette behind [`snuffler_web_core::theme::WiggleTheme::Herbarium`].
//!
//! Fern green, berry red and violet, with olive and dried-petal accents.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A green-grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x4E, 0x5A, 0x51);

/// The forest hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xBC, 0xED, 0xC5),
    (0xFA, 0xC2, 0xC9),
    (0xC5, 0xD4, 0xFD),
    (0xCC, 0xF7, 0xF7),
    (0xD8, 0xD2, 0xAF),
    (0xF7, 0xD9, 0xF8),
    (0xFF, 0xDD, 0xC7),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x02, 0x57, 0x25),
    (0x93, 0x2D, 0x46),
    (0x41, 0x54, 0x9D),
    (0x02, 0x6B, 0x6B),
    (0x6C, 0x60, 0x02),
    (0x84, 0x49, 0x88),
    (0x72, 0x38, 0x01),
];

/// Pale sage, the ink of the dark canvas.
const DARK_INK: Rgb = (0xD8, 0xE6, 0xD6);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x1A, 0x3D, 0x23),
    (0x60, 0x35, 0x3C),
    (0x33, 0x3F, 0x63),
    (0x23, 0x4C, 0x4B),
    (0x48, 0x42, 0x22),
    (0x43, 0x2C, 0x44),
    (0x45, 0x2A, 0x18),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x83, 0xD9, 0x96),
    (0xEB, 0x80, 0x92),
    (0x93, 0xAD, 0xFE),
    (0x61, 0xC0, 0xBF),
    (0xC1, 0xB4, 0x66),
    (0xDA, 0x9D, 0xDD),
    (0xFF, 0xBA, 0x8C),
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
