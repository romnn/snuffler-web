//! snuffler-web binary entry point: launches the egui app natively (dev shell)
//! or mounts it on the page canvas under wasm (the real target).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

// When compiling natively:
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_min_inner_size([640.0, 400.0]);
    // The icon is cosmetic; skip it rather than abort if it fails to decode.
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon-256.png")[..])
    {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "snuffler",
        native_options,
        Box::new(|cc| Ok(Box::new(snuffler_web::App::new(cc)))),
    )
}

// When compiling to web using trunk:
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;

    // Redirect `log` messages to `console.log` and friends:
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();
    console_error_panic_hook::set_once();

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        // Bring up the rayon worker pool for the wasm frontend — a general-purpose
        // parallelism primitive (the PNG export is the first user; compute-heavy
        // loops can use it later) — but do NOT block startup on it: the egui app +
        // Pyodide boot must not queue behind the worker spawn. Init runs in the
        // background and flips a ready flag; until it is up (or with no cross-origin
        // isolation, where the init promise rejects) callers run on the main thread.
        #[cfg(feature = "threads")]
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "hardware_concurrency is a small non-negative integer core count"
            )]
            let cores = web_sys::window()
                .map(|w| w.navigator().hardware_concurrency() as usize)
                .unwrap_or(0);
            // Cap the pool: spawning one Worker per core (e.g. 48) at page load —
            // each re-instantiating the wasm — costs far more than it buys for the
            // workloads we run today, where 4 is ample. Raise the cap if a future
            // workload actually needs wider parallelism.
            let threads = cores.min(4).max(1);
            wasm_bindgen_futures::spawn_local(async move {
                let ready =
                    wasm_bindgen_futures::JsFuture::from(snuffler_web::init_thread_pool(threads))
                        .await
                        .is_ok();
                snuffler_web::set_pool_ready(ready);
                if ready {
                    log::info!("rayon pool up: {threads} threads");
                } else {
                    log::warn!(
                        "no rayon pool (no cross-origin isolation); running on the main thread"
                    );
                }
            });
        }

        let Some(canvas) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("the_canvas_id"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("could not find a canvas element with id #the_canvas_id");
            return;
        };

        if let Err(err) = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(snuffler_web::App::new(cc)))),
            )
            .await
        {
            log::error!("failed to start eframe: {err:?}");
        }
    });
}
