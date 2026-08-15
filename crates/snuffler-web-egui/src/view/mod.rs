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
