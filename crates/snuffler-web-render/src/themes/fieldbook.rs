//! Fieldbook, the palette behind [`snuffler_web_core::theme::WiggleTheme::Fieldbook`].
//!
//! Mineral blues, brick red and leaf green, balanced by quieter supporting inks.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A neutral grey at snuffler's ink level.
const LIGHT_INK: Rgb = (0x53, 0x56, 0x62);

/// Tints of the seven hues, one per stroke colour.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xA5, 0xCA, 0xFA),
    (0xF8, 0xC0, 0xBB),
    (0xD2, 0xF4, 0xD2),
    (0xDB, 0xCB, 0xF0),
    (0xEB, 0xD9, 0xB3),
    (0xB4, 0xDC, 0xDB),
    (0xFF, 0xDA, 0xE7),
];

/// Inks for the seven coloring keys, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x1C, 0x5A, 0xA3),
    (0xA1, 0x33, 0x32),
    (0x06, 0x5A, 0x17),
    (0x6B, 0x46, 0x90),
    (0x73, 0x55, 0x02),
    (0x06, 0x64, 0x65),
    (0x72, 0x2F, 0x4C),
];

/// The near-white of the dark canvas, the shared chrome ink.
const DARK_INK: Rgb = (0xE8, 0xEA, 0xF0);

/// The same seven hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x1F, 0x37, 0x56),
    (0x5F, 0x34, 0x31),
    (0x1A, 0x37, 0x1C),
    (0x4A, 0x3B, 0x5A),
    (0x47, 0x36, 0x0E),
    (0x22, 0x49, 0x49),
    (0x45, 0x27, 0x32),
];

/// The same seven keys, lifted for the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x71, 0xAF, 0xFE),
    (0xFD, 0x8A, 0x82),
    (0x76, 0xBE, 0x79),
    (0xC6, 0xA2, 0xEF),
    (0xD1, 0xAF, 0x66),
    (0x6C, 0xCD, 0xCD),
    (0xFC, 0xAF, 0xCC),
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
