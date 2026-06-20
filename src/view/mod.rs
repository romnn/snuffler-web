//! The snuffler waveform inspector UI (egui).

pub mod controls;
pub mod draw;
pub mod map;
pub mod markers;
/// Optional resvg-backed high-DPI PNG export; see the `svg-raster` feature.
#[cfg(feature = "svg-raster")]
pub mod raster;
pub mod timeaxis;
pub mod waveform;
