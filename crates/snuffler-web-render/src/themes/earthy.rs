//! Snuffler Web 1, earthy: terracotta and ochre through olive and sage, with an aqua
//! counterpoint, under a warm grey ink.
//!
//! Seeded from gruvbox's accents, so the seven hues and the chroma relationships
//! between them come from a palette designed as a family.
//! One of the bold themes, and the warm end of the wheel carries the chroma while the
//! cool end falls back to a wash.
//!
//! On white the wiggle is a grey at 7.2:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.2:1 over blocks running 1.36-2.24 against the
//! ground.
//! The strokes run 7.0-12.0:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.37-2.49 under a 14.3:1 ink at 5.7:1 over them, strokes at 7.7-15.4:1.
//! The closest of the 21 box pairs is 10 light and 10 dark, and of the stroke pairs 11
//! and 11 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A warm grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x60, 0x55, 0x4C);

/// The earth hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xF6, 0xAA, 0xCB),
    (0x9F, 0xB6, 0x9F),
    (0xFF, 0x8D, 0x7E),
    (0xFF, 0xCC, 0xB4),
    (0xE1, 0xA6, 0x40),
    (0xC0, 0xE4, 0xE6),
    (0xD0, 0xD2, 0x7B),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0xAE, 0x00, 0x6D),
    (0x00, 0x4E, 0x10),
    (0x74, 0x00, 0x02),
    (0x93, 0x3D, 0x00),
    (0x4C, 0x33, 0x00),
    (0x00, 0x5B, 0x60),
    (0x5B, 0x5C, 0x10),
];

/// Cream, the warm light ink of the dark canvas.
const DARK_INK: Rgb = (0xEC, 0xE0, 0xD0);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x7E, 0x3E, 0x5C),
    (0x39, 0x5D, 0x3B),
    (0x6B, 0x00, 0x01),
    (0x8B, 0x3F, 0x14),
    (0x5C, 0x3D, 0x00),
    (0x26, 0x43, 0x45),
    (0x30, 0x30, 0x00),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xFF, 0x7A, 0xBB),
    (0x8E, 0xFF, 0x95),
    (0xFF, 0xCB, 0xC3),
    (0xE9, 0x95, 0x6C),
    (0xFD, 0xB1, 0x00),
    (0x5C, 0xF6, 0xFF),
    (0xF2, 0xF2, 0x00),
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
