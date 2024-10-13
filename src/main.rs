#![warn(clippy::all, rust_2018_idioms)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release


include!("/Users/roman/dev/PyOxidizer/embedtest/default_python_config.rs");

// When compiling natively:
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_min_inner_size([300.0, 220.0])
            .with_icon(
                // NOTE: Adding an icon is optional
                eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon-256.png")[..])
                    .expect("Failed to load icon"),
            ),
        ..Default::default()
    };


    let config = default_python_config();

    let interp = pyembed::MainPythonInterpreter::new(config).unwrap();

    // is a instance.
    interp.with_gil(|py| {
        py.run("print('hello, world')", None, None).unwrap();
    });

    // interpreter.with_gil(|py| {
    //      match py.eval("print('hello, world')") {
    //         Ok(_) => println!("python code executed successfully"),
    //         Err(e) => println!("python error: {:?}", e),
    //     }
    // });

    eframe::run_native(
        "snuffler",
        native_options,
        Box::new(|cc| Box::new(snuffler::App::new(cc))),
    )
}

// When compiling to web using trunk:
#[cfg(target_arch = "wasm32")]
fn main() {
    // Redirect `log` message to `console.log` and friends:
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        eframe::WebRunner::new()
            .start(
                "the_canvas_id", // hardcode it
                web_options,
                Box::new(|cc| Box::new(snuffler::App::new(cc))),
            )
            .await
            .expect("failed to start eframe");
    });
}
