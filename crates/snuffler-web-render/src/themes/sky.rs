//! Snuffler Web 3, sky: airy blues and violets over pale warms, under a cool grey ink.
//!
//! Seeded from catppuccin's accents, so the seven hues and the chroma relationships
//! between them come from a palette designed as a family.
//! The airy theme: the chroma sits on the blues and violets, and the warms stay light,
//! so the set reads as a sky rather than a wheel.
//!
//! On white the wiggle is a grey at 7.3:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.6:1 over blocks running 1.18-2.04 against the
//! ground.
//! The strokes run 6.4-12.3:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.20-2.30 under a 14.5:1 ink at 6.3:1 over them, strokes at 7.1-13.8:1.
//! The closest of the 21 box pairs is 10 light and 11 dark, and of the stroke pairs 13
//! and 13 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A cool grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x52, 0x56, 0x62);

/// The sky hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xBD, 0xF0, 0xB3),
    (0xF9, 0x9C, 0x9D),
    (0x8F, 0xB6, 0xFF),
    (0xA1, 0xCD, 0xD0),
    (0xFF, 0xE7, 0xDE),
    (0xDB, 0xCD, 0xFF),
    (0xE9, 0xC6, 0xA1),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x12, 0x55, 0x00),
    (0xC0, 0x00, 0x33),
    (0x00, 0x2B, 0x8A),
    (0x00, 0x6A, 0x70),
    (0x53, 0x2C, 0x1D),
    (0x5C, 0x05, 0xAA),
    (0x73, 0x56, 0x37),
];

/// A pale sky, the ink of the dark canvas.
const DARK_INK: Rgb = (0xDC, 0xE4, 0xF5);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x11, 0x3E, 0x06),
    (0x6E, 0x1F, 0x26),
    (0x00, 0x2F, 0x8E),
    (0x00, 0x59, 0x5E),
    (0x3B, 0x1C, 0x10),
    (0x35, 0x18, 0x5F),
    (0x62, 0x4C, 0x33),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x4A, 0xFF, 0x03),
    (0xFF, 0x75, 0x7C),
    (0x70, 0xA1, 0xFF),
    (0x00, 0xD2, 0xDD),
    (0xF0, 0xB4, 0x9D),
    (0xDE, 0xD2, 0xFF),
    (0xE4, 0x91, 0x18),
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
