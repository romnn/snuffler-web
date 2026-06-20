//! snuffler-web: pyrocko's snuffler seismic waveform inspector in the browser.
//!
//! A Rust/egui (wasm) frontend that drives real pyrocko + numpy + scipy via
//! Pyodide. Rust renders; all computation happens in Python (see `web/bridge.py`).

mod app;
mod bridge;
mod state;
mod view;

pub use app::App;
