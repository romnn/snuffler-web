//! snuffler-web: pyrocko's snuffler seismic waveform inspector in the browser.
//!
//! A Rust/egui (wasm) frontend that drives real pyrocko + numpy + scipy via
//! Pyodide. Rust renders; all computation happens in Python (see `web/bridge.py`).

// Web accessibility layer: app policy over the workspace's generic `accesskit-dom`
// and `egui-a11y` crates; needs the patched eframe (see the `[patch.crates-io]`
// block in the workspace manifest).
#[cfg(target_arch = "wasm32")]
mod a11y;
#[cfg(target_arch = "wasm32")]
mod app;
// `?example=<slug>` links: read one on boot, publish one after a load.
#[cfg(target_arch = "wasm32")]
mod deep_link;
// Settings that survive a reload (the browser's `snuffler.pf`).
#[cfg(target_arch = "wasm32")]
mod persist;
#[cfg(target_arch = "wasm32")]
mod snufflings;
#[cfg(target_arch = "wasm32")]
mod view;

#[cfg(target_arch = "wasm32")]
pub use a11y::snuffler_a11y_action;
#[cfg(target_arch = "wasm32")]
pub use app::App;

// Off-main-thread work runs on a wasm-bindgen-rayon worker pool (the `threads`
// feature; see `app` + `AGENTS.md`). The `pub use` is what makes wasm-bindgen-rayon
// generate the worker entry point, so it must live in the crate root and be public.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;

/// Whether the rayon worker pool came up (it needs cross-origin isolation, so it
/// is absent on the plain, non-isolated load). `main` sets it after attempting
/// [`init_thread_pool`]; callers read it to decide between the pooled (off-thread)
/// and the inline (main-thread) path.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
mod pool {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    static READY: AtomicBool = AtomicBool::new(false);
    static WORKER_PNG_EXPORT_COUNT: AtomicU32 = AtomicU32::new(0);

    /// Record whether the rayon pool initialized successfully.
    pub fn set_ready(ready: bool) {
        READY.store(ready, Ordering::Relaxed);
    }

    /// True once the rayon pool is available for `rayon::spawn`.
    pub(crate) fn ready() -> bool {
        READY.load(Ordering::Relaxed)
    }

    /// Records that PNG rasterization completed in the rayon pool.
    pub(crate) fn record_worker_png_export() {
        WORKER_PNG_EXPORT_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    /// Number of PNG rasterizations completed in the rayon pool.
    pub(crate) fn worker_png_export_count() -> u32 {
        WORKER_PNG_EXPORT_COUNT.load(Ordering::Relaxed)
    }
}

#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use pool::set_ready as set_pool_ready;

/// Reports whether the wasm rayon pool is ready for browser regressions.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
#[wasm_bindgen::prelude::wasm_bindgen]
#[must_use]
pub fn snuffler_rayon_ready() -> bool {
    pool::ready()
}

/// Reports completed worker-side PNG rasterizations for browser regressions.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
#[wasm_bindgen::prelude::wasm_bindgen]
#[must_use]
pub fn snuffler_worker_png_export_count() -> u32 {
    pool::worker_png_export_count()
}
