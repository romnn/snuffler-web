//! The About window: what this is, what it was built from, and what it runs on.

use snuffler_web_core::bridge::RuntimeInfo;
use snuffler_web_core::state::AppState;

/// When the published build was built (UTC, to the minute), and the commit it
/// came from. Both are baked in from the environment by the deploy workflow.
///
/// Read with `option_env!` rather than produced by a build script on purpose: a
/// script that stamped the current time would have to re-run — and so relink the
/// wasm — on every local edit, and a local build's timestamp identifies nothing
/// anyway. Unset means "built from a working tree", which is the honest answer.
const BUILD_DATE: Option<&str> = option_env!("SNUFFLER_BUILD_DATE");
const GIT_COMMIT: Option<&str> = option_env!("SNUFFLER_GIT_COMMIT");

/// The stamp for the top bar: the build date, or `dev` for an unpublished build.
pub fn build_stamp() -> &'static str {
    BUILD_DATE.unwrap_or("dev")
}

/// Hover text for the stamp — what does not fit in the top bar.
pub fn build_tooltip() -> String {
    let head = match BUILD_DATE {
        Some(date) => format!("Built {date} UTC"),
        None => "Development build (built from a working tree)".to_owned(),
    };
    let commit = GIT_COMMIT.map_or_else(String::new, |sha| format!(" · commit {sha}"));
    format!(
        "{head}{commit} · {}\nClick for the About window.",
        profile()
    )
}

fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// What this is made of: the projects that end up in the shipped bundle, and the
/// toolchains that produce it. Ordered by role — languages, the wasm toolchain,
/// the scientific stack, the UI, the pickers, the plumbing, the typefaces — so
/// the list reads as something more than an alphabet.
///
/// pyrocko is deliberately absent: it is the project this one serves, and it has
/// its own link above. Pure orchestration (mise, Task, micromux) is absent too —
/// it never touches the artifact.
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
    ("JetBrains Mono", "https://www.jetbrains.com/lp/mono/"),
    // Not bare "Ubuntu": in a list of projects that reads as the distribution.
    // This is the Dalton Maag typeface the bundled Ubuntu-Regular.ttf comes from.
    ("Ubuntu Typeface", "https://design.ubuntu.com/font"),
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
            // One scroll area over the whole window, so nothing inside it scrolls
            // on its own. `auto_shrink` off keeps the window at the size above
            // instead of collapsing onto its content.
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
        fact(
            ui,
            "built",
            &BUILD_DATE.map_or_else(
                || "development build".to_owned(),
                |date| format!("{date} UTC"),
            ),
        );
        if let Some(sha) = GIT_COMMIT {
            fact(ui, "commit", sha);
        }
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
