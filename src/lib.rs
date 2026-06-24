//! snuffler-web: pyrocko's snuffler seismic waveform inspector in the browser.
//!
//! A Rust/egui (wasm) frontend that drives real pyrocko + numpy + scipy via
//! Pyodide. Rust renders; all computation happens in Python (see `web/bridge.py`).

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod bridge;
// Progress overlay shown while an off-thread job runs (threaded build).
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
mod job;
#[cfg(target_arch = "wasm32")]
mod snufflings;
#[cfg(target_arch = "wasm32")]
mod state;
#[cfg(target_arch = "wasm32")]
mod view;

#[cfg(target_arch = "wasm32")]
pub use app::App;

// Off-main-thread work runs on a wasm-bindgen-rayon worker pool (the `threads`
// feature; see `job` + `AGENTS.md`). The `pub use` is what makes wasm-bindgen-rayon
// generate the worker entry point, so it must live in the crate root and be public.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;

/// Whether the rayon worker pool came up (it needs cross-origin isolation, so it
/// is absent on the plain, non-isolated load). `main` sets it after attempting
/// [`init_thread_pool`]; callers read it to decide between the pooled (off-thread)
/// and the inline (main-thread) path.
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
mod pool {
    use std::sync::atomic::{AtomicBool, Ordering};

    static READY: AtomicBool = AtomicBool::new(false);

    /// Record whether the rayon pool initialized successfully.
    pub fn set_ready(ready: bool) {
        READY.store(ready, Ordering::Relaxed);
    }

    /// True once the rayon pool is available for `rayon::spawn`.
    pub(crate) fn ready() -> bool {
        READY.load(Ordering::Relaxed)
    }
}

#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use pool::set_ready as set_pool_ready;
