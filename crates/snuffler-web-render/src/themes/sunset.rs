//! Afterglow, the palette behind [`snuffler_web_core::theme::WiggleTheme::SnufflerWeb5`].
//!
//! Ember red and dusk blue, with rose, honey and muted green.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A mauve grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x61, 0x53, 0x55);

/// The sunset hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xF3, 0xBB, 0xBA),
    (0xB7, 0xCB, 0xF5),
    (0xC5, 0xDC, 0xC0),
    (0xEB, 0xC9, 0xF2),
    (0xF9, 0xD5, 0xB4),
    (0xC9, 0xEE, 0xF2),
    (0xFF, 0xE0, 0xEB),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0xA7, 0x39, 0x41),
    (0x35, 0x55, 0xA4),
    (0x30, 0x61, 0x25),
    (0x7D, 0x4C, 0x89),
    (0x80, 0x4B, 0x03),
    (0x0B, 0x6A, 0x73),
    (0x72, 0x2F, 0x4C),
];

/// A warm off-white, the ink of the dark canvas.
const DARK_INK: Rgb = (0xF0, 0xDC, 0xD8);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x66, 0x3A, 0x3A),
    (0x20, 0x2F, 0x51),
    (0x20, 0x33, 0x1C),
    (0x4A, 0x34, 0x4F),
    (0x4D, 0x34, 0x1B),
    (0x1F, 0x41, 0x44),
    (0x40, 0x23, 0x2E),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xF4, 0x7D, 0x80),
    (0x82, 0xA9, 0xFF),
    (0xA2, 0xD4, 0x96),
    (0xC5, 0x8E, 0xD1),
    (0xE5, 0xA6, 0x66),
    (0x73, 0xC5, 0xCF),
    (0xF5, 0xA6, 0xC4),
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
