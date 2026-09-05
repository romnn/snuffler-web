//! Kiln, the palette behind [`snuffler_web_core::theme::WiggleTheme::SnufflerWeb1`].
//!
//! Fired clay and ochre, balanced by indigo, sage and glazed teal.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A warm grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x60, 0x55, 0x4C);

/// The earth hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xF2, 0xBF, 0xAD),
    (0xB6, 0xDC, 0xFD),
    (0xBF, 0xDE, 0xBF),
    (0xF6, 0xE5, 0xC1),
    (0xFF, 0xD2, 0xDB),
    (0xCC, 0xF0, 0xED),
    (0xD3, 0xC3, 0xDF),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0xA6, 0x3D, 0x10),
    (0x00, 0x61, 0x9B),
    (0x1F, 0x6E, 0x28),
    (0x75, 0x58, 0x06),
    (0x81, 0x32, 0x49),
    (0x04, 0x59, 0x55),
    (0x70, 0x51, 0x86),
];

/// Cream, the warm light ink of the dark canvas.
const DARK_INK: Rgb = (0xEC, 0xE0, 0xD0);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x49, 0x20, 0x10),
    (0x1A, 0x3C, 0x58),
    (0x1C, 0x35, 0x1D),
    (0x46, 0x39, 0x1B),
    (0x58, 0x33, 0x3C),
    (0x2C, 0x4C, 0x49),
    (0x36, 0x29, 0x40),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xEB, 0x88, 0x66),
    (0x65, 0xBA, 0xFF),
    (0x78, 0xBC, 0x7A),
    (0xDA, 0xB8, 0x6F),
    (0xFF, 0x9F, 0xB5),
    (0x80, 0xD0, 0xCB),
    (0xC3, 0xA1, 0xDC),
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
