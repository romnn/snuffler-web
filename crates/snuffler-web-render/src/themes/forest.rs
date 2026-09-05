//! Snuffler Web 4, forest: lichen and jade greens with warm bark and a sky
//! counterpoint, under a green-grey ink.
//!
//! Seeded from everforest's accents, so the seven hues and the chroma relationships
//! between them come from a palette designed as a family.
//! One of the bold themes, with the chroma on the greens: the lichen and jade carry the
//! mood and the warms sit behind them.
//!
//! On white the wiggle is a grey at 7.2:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.3:1 over blocks running 1.35-2.22 against the
//! ground.
//! The strokes run 6.9-12.2:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.36-2.48 under a 14.3:1 ink at 5.8:1 over them, strokes at 8.0-14.3:1.
//! The closest of the 21 box pairs is 12 light and 10 dark, and of the stroke pairs 10
//! and 11 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A green-grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x4E, 0x5A, 0x51);

/// The forest hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xF4, 0xB1, 0xDC),
    (0xB8, 0xE3, 0xFF),
    (0xE4, 0xAE, 0x47),
    (0x4E, 0xC2, 0x94),
    (0xCB, 0xE0, 0x6A),
    (0xFF, 0xD5, 0xBD),
    (0xFE, 0x8F, 0x86),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0xAA, 0x00, 0x86),
    (0x00, 0x3D, 0x5A),
    (0x77, 0x54, 0x00),
    (0x00, 0x67, 0x48),
    (0x3F, 0x48, 0x00),
    (0x5A, 0x27, 0x00),
    (0x86, 0x2B, 0x28),
];

/// Pale sage, the ink of the dark canvas.
const DARK_INK: Rgb = (0xD8, 0xE6, 0xD6);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x75, 0x3F, 0x63),
    (0x1D, 0x59, 0x79),
    (0x59, 0x3E, 0x00),
    (0x00, 0x56, 0x3B),
    (0x2A, 0x31, 0x00),
    (0x8B, 0x3F, 0x00),
    (0x68, 0x08, 0x0F),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xFF, 0x8A, 0xDA),
    (0x69, 0xC7, 0xFF),
    (0xED, 0xBF, 0x6D),
    (0x31, 0xFF, 0xBA),
    (0xC9, 0xE2, 0x2D),
    (0xF9, 0x8F, 0x49),
    (0xDA, 0xA8, 0xA2),
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
