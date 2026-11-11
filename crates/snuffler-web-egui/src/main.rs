//! snuffler-web binary entry point: browser-only, mounted on the page canvas.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("snuffler-web is browser-only. Run `task serve`.");

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;

    init_tracing();
    console_error_panic_hook::set_once();

    let web_options = eframe::WebOptions::default();

    #[expect(
        clippy::disallowed_methods,
        reason = "this is the browser application bootstrap, before an activity ledger exists"
    )]
    wasm_bindgen_futures::spawn_local(async {
        // Bring up the rayon worker pool for the wasm frontend, but do not block
        // startup on it: the egui app and the Pyodide boot must not queue behind
        // the worker spawn.
        // Init runs in the background and flips a ready flag; until it is up (or
        // with no cross-origin isolation, where the init promise rejects) callers
        // run on the main thread.
        #[cfg(feature = "threads")]
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "hardware_concurrency is a small non-negative integer core count"
            )]
            let cores =
                web_sys::window().map_or(0, |w| w.navigator().hardware_concurrency() as usize);
            // Cap the pool: spawning one Worker per core (e.g. 48) at page load,
            // each re-instantiating the wasm, costs far more than it buys for the
            // PNG and marker exports, where 4 is ample.
            let threads = cores.clamp(1, 4);
            #[expect(
                clippy::disallowed_methods,
                reason = "thread-pool initialization is startup infrastructure, not user work"
            )]
            wasm_bindgen_futures::spawn_local(async move {
                let ready = wasm_bindgen_futures::JsFuture::from(
                    snuffler_web_egui::init_thread_pool(threads),
                )
                .await
                .is_ok();
                snuffler_web_egui::set_pool_ready(ready);
                if ready {
                    tracing::info!(threads, "rayon pool up");
                } else {
                    tracing::warn!(
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
            tracing::error!("could not find a canvas element with id #the_canvas_id");
            return;
        };

        if let Err(err) = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(snuffler_web_egui::App::new(cc)))),
            )
            .await
        {
            tracing::error!(?err, "failed to start eframe");
        }
    });
}

/// Sends `tracing` events to the browser console.
///
/// eframe and egui log through the `log` crate; `init` also installs the bridge that turns
/// those records into `tracing` events, so they reach the console the same way.
#[cfg(target_arch = "wasm32")]
fn init_tracing() {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    // The browser has no wall clock for the formatter, and the console adds its own timestamps.
    let console = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);
    tracing_subscriber::registry().with(console).init();
}
