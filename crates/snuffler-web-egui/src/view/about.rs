//! The About window: what this is, what it was built from, and what it runs on.

use snuffler_web_core::bridge::RuntimeInfo;
use snuffler_web_core::state::AppState;

/// Deployment time, including its zone, supplied by the deployment workflow.
/// Local builds use the build script's UTC timestamp instead.
const BUILD_DATE: Option<&str> = option_env!("SNUFFLER_BUILD_DATE");
const GIT_COMMIT: &str = env!("SNUFFLER_BUILD_COMMIT");
/// The release tag, such as `v0.1.0`, which only a release build sets.
const RELEASE: Option<&str> = option_env!("SNUFFLER_RELEASE");

/// The release tag, if this is a release build.
///
/// The Docker build passes the variable empty for every other build, so empty counts as
/// unset.
fn release() -> Option<&'static str> {
    RELEASE.filter(|tag| !tag.is_empty())
}

/// The stamp for the control strip.
///
/// A release shows its tag alone.
/// Development builds show the package version and short commit.
pub fn build_stamp() -> String {
    if let Some(tag) = release() {
        return tag.to_owned();
    }
    version_identifier()
}

fn version_identifier() -> String {
    let commit = GIT_COMMIT.get(..10).unwrap_or(GIT_COMMIT);
    format!("{}+{commit}", env!("CARGO_PKG_VERSION"))
}

fn build_time() -> String {
    BUILD_DATE.map_or_else(
        || {
            option_env!("SNUFFLER_LOCAL_BUILD_EPOCH")
                .and_then(|epoch| epoch.parse::<f64>().ok())
                .map_or_else(
                    || "unknown".to_owned(),
                    |time| {
                        format!(
                            "{} UTC",
                            snuffler_web_core::timeaxis::format_timestamp(time)
                        )
                    },
                )
        },
        str::to_owned,
    )
}

fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// What this is made of: the projects that end up in the shipped bundle, and the toolchains
/// that produce it.
///
/// Ordered by role — languages, the wasm toolchain, the scientific stack, the UI, the
/// pickers, the plumbing, the typefaces — so the list reads as something more than an
/// alphabet.
///
/// pyrocko is deliberately absent: it is the project this one serves, and it has its own link
/// above.
/// Pure orchestration (mise, Task, micromux) is absent too — it never touches the artifact.
const ACKNOWLEDGED: &[(&str, &str)] = &[
    ("Rust", "https://www.rust-lang.org"),
    ("CPython", "https://www.python.org"),
    ("TypeScript", "https://www.typescriptlang.org"),
    ("Pyodide", "https://pyodide.org"),
    ("Emscripten", "https://emscripten.org"),
    ("LLVM", "https://llvm.org"),
    ("Binaryen", "https://github.com/WebAssembly/binaryen"),
    ("Trunk", "https://trunkrs.dev"),
    ("wasm-bindgen", "https://github.com/rustwasm/wasm-bindgen"),
    ("Bun", "https://bun.sh"),
    ("NumPy", "https://numpy.org"),
    ("SciPy", "https://scipy.org"),
    ("Matplotlib", "https://matplotlib.org"),
    ("egui", "https://github.com/emilk/egui"),
    ("AccessKit", "https://accesskit.dev"),
    ("resvg", "https://github.com/linebender/resvg"),
    ("SeisBench", "https://github.com/seisbench/seisbench"),
    ("ONNX Runtime", "https://onnxruntime.ai"),
    ("Protocol Buffers", "https://protobuf.dev"),
    ("prost", "https://github.com/tokio-rs/prost"),
    ("Rayon", "https://github.com/rayon-rs/rayon"),
    ("OpenStreetMap", "https://www.openstreetmap.org/copyright"),
    ("Geist", "https://github.com/vercel/geist-font"),
    ("JetBrains Mono", "https://www.jetbrains.com/lp/mono/"),
];

/// Draws the About window when the shared state asks for it.
pub fn show(ctx: &egui::Context, state: &mut AppState, runtime: Option<&RuntimeInfo>) {
    if !state.show_about {
        return;
    }
    let mut open = true;
    egui::Window::new("About")
        .open(&mut open)
        // Sized to fit the content at the default font; the scroll area below is
        // what makes a taller-than-expected layout (a bigger font, a longer status
        // of the Python runtime, a window dragged smaller) reachable rather than
        // cut off, so this only has to be close, not exact.
        .default_size([500.0, 555.0])
        .show(ctx, |ui| {
            // One scroll area over the whole window, so nothing inside it scrolls on its own.
            // `auto_shrink` off keeps the window at the size above instead of collapsing onto its
            // content.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| contents(ui, runtime));
        });
    state.show_about = open;
}

fn contents(ui: &mut egui::Ui, runtime: Option<&RuntimeInfo>) {
    ui.heading("snuffler-web");
    ui.label(weak("pyrocko's snuffler, running natively in the browser."));
    ui.add_space(8.0);
    ui.label(
        "pyrocko runs unmodified. What is compiled to WebAssembly (WASM) is CPython \
         and the native extensions: NumPy, SciPy and pyrocko's own C modules.",
    );
    ui.add_space(8.0);
    ui.label(
        "Only the frontend is new: a web frontend instead of the Qt interface. There \
         is no backend, and the files you open are never uploaded.",
    );

    section(ui, "Build");
    table(ui, "about_build", |ui| {
        fact(ui, "version", &version_identifier());
        if let Some(tag) = release() {
            fact(ui, "release", tag);
        }
        fact(ui, "built", &build_time());
        fact(ui, "commit", GIT_COMMIT);
        fact(ui, "profile", profile());
    });

    section(ui, "Runtime");
    match runtime {
        Some(info) => table(ui, "about_runtime", |ui| {
            fact(ui, "Python", &info.python);
            fact(ui, "NumPy", &info.numpy);
            fact(ui, "pyrocko", &info.pyrocko);
            fact(ui, "WASM", &wasm_summary(info));
        }),
        // The worker reports its versions once Python is up, so this is what the
        // window shows if it is opened during the boot.
        None => {
            ui.label(weak("Python is still starting…"));
        }
    }

    section(ui, "Links");
    link(ui, "pyrocko.org", "https://pyrocko.org");
    link(
        ui,
        "github.com/romnn/snuffler-web",
        "https://github.com/romnn/snuffler-web",
    );

    section(ui, "Acknowledgements");
    acknowledgement(ui);
    ui.add_space(8.0);
}

/// One line of thanks, then the projects themselves as a wrapping list of links.
fn acknowledgement(ui: &mut egui::Ui) {
    ui.label(weak(
        "Thanks to the contributors of the open source projects that power snuffler-web:",
    ));
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        // The separators are their own labels so only the name itself is a link.
        // Spacing goes to zero for the same reason: a comma belongs against the
        // word it follows, not a widget-gap away from it.
        ui.spacing_mut().item_spacing.x = 0.0;
        for (index, (name, url)) in ACKNOWLEDGED.iter().enumerate() {
            if index > 0 {
                ui.label(weak(", "));
            }
            link(ui, name, url);
        }
    });
}

/// A link that opens in a new tab.
///
/// Not `hyperlink_to`, which navigates the *current* tab: that would unload the
/// app, and with it every trace, marker and pick of the session, because someone
/// clicked a credit.
fn link(ui: &mut egui::Ui, label: &str, url: &str) {
    if ui.link(label).on_hover_text(url).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn wasm_summary(info: &RuntimeInfo) -> String {
    // A bare "single-threaded" is a real signal, not a cosmetic one: this build
    // only ships the py-mt runtime, so it means threading failed to come up.
    let mode = if info.threaded {
        "threads + SIMD"
    } else {
        "single-threaded"
    };
    if info.cores > 0 {
        format!("{mode} · {} cores", info.cores)
    } else {
        mode.to_owned()
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(4.0);
}

fn weak(text: &str) -> egui::RichText {
    egui::RichText::new(text).weak()
}

/// A plain two-column table.
fn table(ui: &mut egui::Ui, id: &str, rows: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, rows);
}

fn fact(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(weak(label));
    ui.label(egui::RichText::new(value).monospace());
    ui.end_row();
}
