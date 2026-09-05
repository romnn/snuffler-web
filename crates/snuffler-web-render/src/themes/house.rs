//! Snuffler Web: this frontend's own look, coloured strokes over tinted blocks.
//!
//! The seven hues are this frontend's own, evenly spaced around the wheel, and the fit
//! places them on the lightness axis for the widest worst pair.
//! Every stroke is coloured by its key, which is what separates this theme from the
//! moods: they hand the colouring to the strokes only when the boxes stop doing it.
//!
//! On white the wiggle is a grey at 7.3:1, snuffler's own level rather than the near-
//! black it started as, and it holds 3.6:1 over blocks running 1.19-2.04 against the
//! ground.
//! The strokes run 7.5-11.8:1.
//! The dark canvas has the whole range to itself, so its blocks keep their depth:
//! 1.21-2.28 under a 15.4:1 ink at 6.8:1 over them, strokes at 8.3-15.2:1.
//! The closest of the 21 box pairs is 12 light and 12 dark, and of the stroke pairs 12
//! and 13 — in `OKLab` distance, where about 2 is the smallest difference the eye
//! catches.

use snuffler_web_core::theme::Theme;

use super::{Rgb, boxes, canvas};
use crate::palette::Palette;

/// A neutral grey at snuffler's ink level.
const LIGHT_INK: Rgb = (0x53, 0x56, 0x62);

/// Tints of the seven hues, one per stroke colour.
const LIGHT_FILLS: [Rgb; 7] = [
    (0xB3, 0xCD, 0xFF),
    (0xBB, 0xBC, 0x76),
    (0xF3, 0x9E, 0xB0),
    (0x57, 0xC4, 0xD8),
    (0xB4, 0xEF, 0xCE),
    (0xFF, 0xC6, 0x9C),
    (0xF7, 0xE6, 0xFF),
];

/// Saturated inks, one per coloring key.
const LIGHT_STROKES: [Rgb; 7] = [
    (0x00, 0x3F, 0xDC),
    (0x58, 0x57, 0x00),
    (0x91, 0x00, 0x3F),
    (0x00, 0x5E, 0x6C),
    (0x03, 0x43, 0x2A),
    (0x59, 0x2C, 0x00),
    (0x88, 0x00, 0xAF),
];

/// The near-white of the dark canvas, the shared chrome ink.
const DARK_INK: Rgb = (0xE8, 0xEA, 0xF0);

/// The same seven hues as blocks on the dark canvas.
const DARK_FILLS: [Rgb; 7] = [
    (0x10, 0x23, 0x4C),
    (0x52, 0x51, 0x00),
    (0x7F, 0x36, 0x49),
    (0x00, 0x58, 0x65),
    (0x00, 0x3E, 0x26),
    (0x3D, 0x1C, 0x00),
    (0x4F, 0x29, 0x60),
];

/// The same seven keys, lifted for the dark canvas.
const DARK_STROKES: [Rgb; 7] = [
    (0x85, 0xAE, 0xFF),
    (0xEA, 0xE8, 0x00),
    (0xFF, 0x8A, 0xA5),
    (0xB9, 0xF2, 0xFE),
    (0x24, 0xFF, 0xAB),
    (0xFF, 0xAF, 0x72),
    (0xE4, 0xB6, 0xF9),
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
