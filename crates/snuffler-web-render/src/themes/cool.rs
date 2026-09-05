//! Snuffler Web 2, cool: the muted theme — pale blue, teal and mauve washes under a
//! slate grey ink.
//!
//! Seeded from nord's frost and aurora, so the seven hues and the chroma relationships
//! between them come from a palette designed as a family.
//! The muted theme: the washes are the lightest of the set, and the chroma sits on the
//! blues and teals while the warms fall back to a dusty neutral.
//!
//! On white the wiggle is a grey at 7.3:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.6:1 over blocks running 1.18-2.05 against the
//! ground.
//! The strokes run 6.3-11.8:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.20-2.28 under a 14.5:1 ink at 6.4:1 over them, strokes at 7.1-11.6:1.
//! The closest of the 21 box pairs is 7 light and 7 dark, and of the stroke pairs 13
//! and 13 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A slate grey at snuffler's ink level, tinted towards the palette.
const LIGHT_INK: Rgb = (0x53, 0x56, 0x62);

/// The cool hues as washes on white.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xD8, 0xCA, 0xAE),
    (0xFE, 0xE5, 0xF9),
    (0xC1, 0xDA, 0xFA),
    (0xDF, 0xF0, 0xD2),
    (0xFE, 0xCC, 0xCE),
    (0xA5, 0xC9, 0xC8),
    (0xC9, 0xB0, 0xA9),
];

/// The same hues as inks, dark enough to read over any wash.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x49, 0x34, 0x00),
    (0xAE, 0x00, 0xA0),
    (0x00, 0x5A, 0xAF),
    (0x3D, 0x67, 0x00),
    (0x7D, 0x15, 0x2B),
    (0x00, 0x54, 0x54),
    (0xB1, 0x30, 0x01),
];

/// A cool off-white, the ink of the dark canvas.
const DARK_INK: Rgb = (0xDE, 0xE4, 0xF0);

/// The same hues as washes on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x3C, 0x30, 0x19),
    (0x5E, 0x49, 0x5A),
    (0x28, 0x43, 0x63),
    (0x45, 0x53, 0x3A),
    (0x3E, 0x19, 0x1D),
    (0x15, 0x36, 0x36),
    (0x4E, 0x3D, 0x38),
];

/// The same hues lifted to read on the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0xE0, 0xA8, 0x00),
    (0xFF, 0x5E, 0xED),
    (0x61, 0xA8, 0xFF),
    (0x8E, 0xE3, 0x00),
    (0xC7, 0x95, 0x98),
    (0x00, 0xD9, 0xD8),
    (0xFF, 0xBA, 0xA7),
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
