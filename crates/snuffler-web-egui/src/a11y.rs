//! App policy for the web accessibility layer.
//!
//! The mechanism lives in this workspace's generic crates: `accesskit-dom` projects
//! egui's AccessKit tree into transparent DOM proxies over the canvas, and
//! [`egui_a11y`] bridges the two directions between that projection and egui. What
//! remains here is only what is snuffler-web's to decide: when the layer is on, what
//! it is called in the DOM, the names of the wasm exports a test harness drives, and
//! the single line of eframe wiring.
//!
//! That wiring line is why the workspace pins a patched eframe in
//! `[patch.crates-io]`: upstream's web backend drops the tree egui builds
//! (`accesskit_update: _, // not currently implemented`), while the patch hands it
//! to a handler instead.
//!
//! The layer is on by default and can be turned off with `?a11y=0`; see [`enabled`].
//! It never competes with the canvas for pointer input unless `?a11y=pointer` asks
//! it to, which is what lets browser automation drive the app; see
//! [`pointer_proxies`] and `.agents/skills/qa/SKILL.md`.

use std::cell::RefCell;

use egui::accesskit::{ActionRequest, NodeId, TreeId};
use egui_a11y::accesskit_dom;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::wasm_bindgen;

pub use egui_a11y::{Urgency, announce_changes, publish_selected};

/// Must match the canvas the app is mounted on in `main.rs` and `index.html`.
const CANVAS_ID: &str = "the_canvas_id";

thread_local! {
    /// The installed layer, if any; the wasm exports below need a slot the
    /// `#[wasm_bindgen]` free functions can reach.
    static A11Y: RefCell<Option<egui_a11y::A11y>> = const { RefCell::new(None) };
}

/// Whether to build and publish the accessibility layer.
///
/// On unless `?a11y=0` says otherwise. Accessibility that has to be asked for is
/// not much use to the people who need it, and the cost of leaving it on is one
/// AccessKit node per widget per pass plus a DOM diff that touches nothing when
/// nothing changed.
fn flag() -> Option<String> {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .and_then(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .find_map(|pair| pair.strip_prefix("a11y=").map(str::to_owned))
        })
}

fn enabled() -> bool {
    !matches!(flag().as_deref(), Some("0" | "false" | "off"))
}

/// Whether the proxies take pointer input, i.e. `?a11y=pointer`.
///
/// Off for everyone else, and that is the point: a proxy that hit-tests is handed
/// the press that was meant for the canvas, and since it can only report a `click`,
/// every drag over it dies there — sliders stop pulling, selections stop sweeping.
/// Screen readers do not need it (they go through focus, keys, and synthetic
/// clicks); tools that resolve an element and then check it is the topmost one at
/// its centre — `WebDriver`, Playwright, the agents built on them — do.
fn pointer_proxies() -> bool {
    matches!(flag().as_deref(), Some("pointer"))
}

/// Whether to also publish the tree as JSON on `window.__snufflerA11y`.
///
/// Off unless `?a11y=debug`: serializing the whole tree costs more than the DOM
/// projection it exists to explain, and it has to be rebuilt on every pass that
/// changes anything, which during a drag means every frame.
fn debug_json() -> bool {
    matches!(flag().as_deref(), Some("debug"))
}

fn canvas() -> Option<web_sys::HtmlCanvasElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(CANVAS_ID)?
        .dyn_into()
        .ok()
}

/// Turn on AccessKit and keep the DOM overlay in sync with each pass.
pub fn install(ctx: &egui::Context) {
    if !enabled() {
        log::info!("a11y layer disabled by ?a11y=0");
        return;
    }
    if matches!(flag().as_deref(), Some("tree")) {
        // Profiling aid: egui builds its tree, nothing is projected. The difference
        // against `?a11y=0` is what enabling AccessKit costs inside egui, and the
        // difference against `?a11y=1` is what the projection costs on top.
        ctx.enable_accesskit();
        log::info!("a11y: tree enabled, projection off");
        return;
    }

    let Some(canvas) = canvas() else {
        log::error!("a11y: canvas #{CANVAS_ID} not found, projection disabled");
        return;
    };
    let options = accesskit_dom::Options {
        overlay_id: "snuffler-a11y".to_owned(),
        // Below the boot overlay (`#loading`, z-index 10), which must keep covering
        // the app — proxies included — until Pyodide is up.
        z_index: 5,
        pointer_proxies: pointer_proxies(),
        tree_json_global: debug_json().then(|| "__snufflerA11y".to_owned()),
        ..accesskit_dom::Options::default()
    };
    match egui_a11y::install(ctx, &canvas, options) {
        Ok(a11y) => {
            // The one eframe-specific line in the layer: the patched eframe hands
            // each pass's AccessKit update to this handler, and everything else
            // flows through the generic crates.
            eframe::web::accesskit::set_update_handler({
                let a11y = a11y.clone();
                move |update| a11y.on_tree_update(update)
            });
            A11Y.with_borrow_mut(|slot| *slot = Some(a11y));
        }
        Err(err) => log::error!("a11y: could not build the DOM projection: {err}"),
    }
}

/// Hand this pass's queued accessibility input to egui.
///
/// Call from [`eframe::App::raw_input_hook`]. A no-op while the layer is off.
pub fn drain(raw_input: &mut egui::RawInput) {
    A11Y.with_borrow(|a11y| {
        if let Some(a11y) = a11y.as_ref() {
            a11y.drain(raw_input);
        }
    });
}

/// Queue an AccessKit action against a node id from the published tree.
///
/// The DOM proxies drive themselves; this is the escape hatch for a caller that
/// already has a node id — a test harness reading `window.__snufflerA11y`, or a
/// control the projection deliberately does not make clickable, such as the
/// waveform region.
///
/// `node_id` is the decimal string an entry's `id` field carries, and `action` one
/// of the names in its `actions` array. Returns `false` for an unparsable id, an
/// unknown action, or while the layer is off; a well-formed request naming a node
/// that no longer exists returns `true` and is dropped by egui on the next pass.
#[wasm_bindgen]
#[must_use]
pub fn snuffler_a11y_action(node_id: &str, action: &str) -> bool {
    let (Ok(node_id), Some(action)) = (
        node_id.parse::<u64>(),
        accesskit_dom::action_from_name(action),
    ) else {
        return false;
    };
    A11Y.with_borrow(|a11y| {
        let Some(a11y) = a11y.as_ref() else {
            return false;
        };
        a11y.request(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: NodeId(node_id),
            data: None,
        });
        true
    })
}

/// What the DOM projection has cost so far, as JSON.
///
/// The per-pass average is the honest measure of this layer; whole-page CPU
/// comparisons drift by more between page loads than the layer itself uses. The
/// write counts say whether a pass did real DOM work or only compared hashes.
/// All zeros while the layer is off.
#[wasm_bindgen]
#[must_use]
pub fn snuffler_a11y_stats() -> String {
    let stats = A11Y
        .with_borrow(|a11y| a11y.as_ref().map(egui_a11y::A11y::stats))
        .unwrap_or_default();
    format!(
        "{{\"passes\":{},\"totalMs\":{:.3},\"described\":{},\"placed\":{}}}",
        stats.passes, stats.total_ms, stats.described, stats.placed
    )
}
