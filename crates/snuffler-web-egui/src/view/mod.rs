//! The snuffler waveform inspector UI (egui).

pub mod about;
pub mod command;
pub mod controls;
pub mod dataset_note;
pub mod draw;
pub mod figure;
pub mod forms;
pub mod help;
pub mod map;
pub mod marker_table;
/// The resvg-backed high-DPI PNG export.
pub mod raster;
pub mod waveform;

use snuffler_web_core::theme::WiggleTheme;
use snuffler_web_render::palette::{Palette, Theme, canvas_theme, palette};

/// The canvas palette for a frontend theme and wiggle theme.
///
/// One place for the light/dark mapping and for the canvas a wiggle theme may pin,
/// so the widgets that colour markers off the canvas — the kind buttons, the
/// table — take their shades from the same palette the painter draws with.
#[must_use]
pub fn palette_for(theme: egui::Theme, wiggle: WiggleTheme) -> Palette {
    let theme = canvas_theme(Theme::from_dark(theme == egui::Theme::Dark), wiggle);
    palette(theme, wiggle)
}

/// The font every label on the waveform canvas is drawn and measured in.
///
/// Bold, because snuffler's canvas is (see [`crate::fonts::CANVAS_FAMILY`]), and one function so
/// the painter and the SVG export cannot measure in one face and draw in another.
#[must_use]
pub fn canvas_font(size: f32) -> egui::FontId {
    egui::FontId::new(
        size,
        egui::FontFamily::Name(crate::fonts::CANVAS_FAMILY.into()),
    )
}
