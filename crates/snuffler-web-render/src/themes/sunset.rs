//! Snuffler Web 5, sunset: roses and golds against horizon blue and lilac, under a
//! mauve grey ink.
//!
//! Seeded from rose pine's accents, so the seven hues and the chroma relationships
//! between them come from a palette designed as a family.
//! One of the bold themes: the roses and golds carry the chroma and the horizon blues
//! sit behind them, the way a sunset holds both ends of the wheel.
//!
//! On white the wiggle is a grey at 7.3:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.3:1 over blocks running 1.35-2.24 against the
//! ground.
//! The strokes run 7.0-12.2:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.37-2.50 under a 14.1:1 ink at 5.6:1 over them, strokes at 7.8-13.1:1.
//! The closest of the 21 box pairs is 9 light and 9 dark, and of the stroke pairs 12
//! and 12 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A mauve grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x61, 0x53, 0x55);

/// The sunset hues as blocks on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xDE, 0xA8, 0xA4),
    (0xBF, 0xE3, 0xF4),
    (0xB9, 0xA5, 0xD1),
    (0xE3, 0xA0, 0x4C),
    (0xA1, 0xC1, 0xC7),
    (0xFF, 0xCD, 0xD8),
    (0xF5, 0xAC, 0xE0),
];

/// The same hues as inks, dark enough to read over any block.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x8D, 0x00, 0x18),
    (0x00, 0x61, 0x7F),
    (0x84, 0x00, 0xCD),
    (0x79, 0x51, 0x1C),
    (0x00, 0x41, 0x4B),
    (0x53, 0x28, 0x34),
    (0x77, 0x00, 0x64),
];

/// A warm off-white, the ink of the dark canvas.
const DARK_INK: Rgb = (0xF0, 0xDC, 0xD8);

/// The same hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x89, 0x3C, 0x3B),
    (0x47, 0x58, 0x5F),
    (0x37, 0x27, 0x48),
    (0x4E, 0x2F, 0x00),
    (0x27, 0x42, 0x46),
    (0x5A, 0x11, 0x2D),
    (0x69, 0x2F, 0x5B),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xFF, 0x83, 0x7E),
    (0xA3, 0xE2, 0xFF),
    (0xC6, 0x8F, 0xFF),
    (0xFE, 0xA3, 0x00),
    (0x00, 0xC7, 0xE0),
    (0xFF, 0xCA, 0xD6),
    (0xCE, 0xA6, 0xC2),
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
