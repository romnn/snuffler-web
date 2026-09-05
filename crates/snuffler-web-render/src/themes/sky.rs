//! Daybreak, the palette behind [`snuffler_web_core::theme::WiggleTheme::SnufflerWeb3`].
//!
//! Periwinkle and soft gold, with coral, cloud blue and mint.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A cool grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x52, 0x56, 0x62);

/// The sky hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xC2, 0xC6, 0xF3),
    (0xE5, 0xDF, 0xB7),
    (0xFF, 0xCD, 0xD0),
    (0xBB, 0xE2, 0xFA),
    (0xCF, 0xF6, 0xE7),
    (0xFE, 0xE4, 0xFF),
    (0xE0, 0xC3, 0xAC),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x43, 0x42, 0x94),
    (0x6C, 0x60, 0x06),
    (0x81, 0x19, 0x2F),
    (0x03, 0x66, 0x8E),
    (0x04, 0x61, 0x4B),
    (0x7F, 0x4B, 0x82),
    (0x65, 0x3D, 0x17),
];

/// A pale sky, the ink of the dark canvas.
const DARK_INK: Rgb = (0xDC, 0xE4, 0xF5);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x3D, 0x40, 0x66),
    (0x50, 0x4A, 0x26),
    (0x61, 0x37, 0x3B),
    (0x0D, 0x38, 0x4D),
    (0x25, 0x46, 0x3B),
    (0x44, 0x2D, 0x45),
    (0x47, 0x31, 0x1E),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x95, 0x9A, 0xF4),
    (0xD5, 0xC6, 0x6C),
    (0xF5, 0x86, 0x91),
    (0x69, 0xC1, 0xF2),
    (0x75, 0xC6, 0xAB),
    (0xD8, 0x9F, 0xDB),
    (0xCD, 0x96, 0x68),
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
