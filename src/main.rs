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
