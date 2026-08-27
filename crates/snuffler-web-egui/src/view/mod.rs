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
/// Optional resvg-backed high-DPI PNG export; see the `svg-raster` feature.
#[cfg(feature = "svg-raster")]
pub mod raster;
pub mod waveform;

/// The font every label on the waveform canvas is drawn and measured in.
///
/// Bold, because snuffler's canvas is (see `CANVAS_FAMILY`), and one function so
/// the painter and the SVG export cannot measure in one face and draw in another.
#[must_use]
pub fn canvas_font(size: f32) -> egui::FontId {
    egui::FontId::new(
        size,
        egui::FontFamily::Name(crate::app::CANVAS_FAMILY.into()),
    )
}
