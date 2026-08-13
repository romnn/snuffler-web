//! The wasm-only wiring: adapter construction, the input queue, and the drain.
//!
//! Everything the DOM proxies do to egui arrives here as a queued [`egui::Event`]
//! and reaches the next pass through [`A11y::drain`]. Events cannot be pushed onto
//! [`egui::Context::input_mut`] instead: egui rebuilds `InputState` from the
//! frame's `RawInput` in `begin_pass`, so anything written between passes is
//! discarded before a widget ever sees it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use egui::accesskit::{ActionRequest, TreeUpdate};

use crate::keys;

/// The queue half of the layer, shared between the DOM listeners (via [`Bridge`]),
/// [`A11y::drain`] and any escape hatch the app exposes.
struct Queue {
    /// Kept so injected input can wake an idle app: egui only repaints on demand,
    /// and an event nobody drains is an event that never happens.
    ctx: egui::Context,
    pending: RefCell<Vec<egui::Event>>,
    /// Whether DOM focus currently sits on one of the proxies.
    holds_focus: Cell<bool>,
}

impl Queue {
    fn push(&self, event: egui::Event) {
        self.pending.borrow_mut().push(event);
        self.ctx.request_repaint();
    }
}

/// Feeds proxy events from the adapter into the egui event queue.
struct Bridge {
    queue: Rc<Queue>,
}

impl accesskit_dom::Handler for Bridge {
    fn action_request(&mut self, request: ActionRequest) {
        self.queue
            .push(egui::Event::AccessKitActionRequest(request));
    }

    fn key_event(&mut self, event: &accesskit_dom::KeyEvent, pressed: bool) -> bool {
        let wanted = keys::translate(event, pressed, &mut self.queue.pending.borrow_mut());
        if wanted {
            self.queue.ctx.request_repaint();
        }
        wanted
    }

    fn focus_changed(&mut self, focus_within: bool) {
        if self.queue.holds_focus.replace(focus_within) != focus_within {
            self.queue.ctx.request_repaint();
        }
    }
}

/// Handle to an installed accessibility layer.
///
/// Cheap to clone; all clones share one adapter and one event queue. The layer
/// lives as long as any clone does — apps install it once for their lifetime.
#[derive(Clone)]
pub struct A11y {
    queue: Rc<Queue>,
    adapter: Rc<RefCell<accesskit_dom::Adapter>>,
}

/// Turn on AccessKit for `ctx` and build the DOM projection over `canvas`.
///
/// The returned handle still needs two one-line connections only the app can make,
/// because they belong to the app's web runner rather than to egui: feed each
/// pass's tree to [`A11y::on_tree_update`], and call [`A11y::drain`] from the
/// runner's raw-input hook.
///
/// # Errors
///
/// Returns [`accesskit_dom::Error`] when the overlay cannot be built — see
/// [`accesskit_dom::Adapter::new`].
pub fn install(
    ctx: &egui::Context,
    canvas: &web_sys::HtmlCanvasElement,
    options: accesskit_dom::Options,
) -> Result<A11y, accesskit_dom::Error> {
    ctx.enable_accesskit();
    let queue = Rc::new(Queue {
        ctx: ctx.clone(),
        pending: RefCell::new(Vec::new()),
        holds_focus: Cell::new(false),
    });
    let bridge = Bridge {
        queue: Rc::clone(&queue),
    };
    let adapter = accesskit_dom::Adapter::new(canvas, options, bridge)?;
    Ok(A11y {
        queue,
        adapter: Rc::new(RefCell::new(adapter)),
    })
}

impl A11y {
    /// Mirror one pass's tree into the DOM overlay.
    ///
    /// Call with every update the web runner hands out; the adapter diffs
    /// internally, so an unchanged tree costs no DOM writes.
    pub fn on_tree_update(&self, update: &TreeUpdate) {
        let pixels_per_point = f64::from(self.queue.ctx.pixels_per_point());
        self.adapter.borrow_mut().update(update, pixels_per_point);
    }

    /// Hand this pass's queued accessibility input to egui.
    ///
    /// Call from the web runner's raw-input hook (eframe:
    /// [`raw_input_hook`](https://docs.rs/eframe/latest/eframe/trait.App.html#method.raw_input_hook)).
    pub fn drain(&self, raw_input: &mut egui::RawInput) {
        if self.queue.holds_focus.get() {
            // The runner derives `focused` from the canvas holding DOM focus, and a
            // focused proxy necessarily means it does not. Left uncorrected, egui
            // would behave as if the window were in the background — no text cursor,
            // no blink — while the user is actively typing into a widget.
            raw_input.focused = true;
        }
        raw_input
            .events
            .append(&mut self.queue.pending.borrow_mut());
    }

    /// Queue an AccessKit action request as if a proxy had raised it.
    ///
    /// The DOM proxies drive themselves; this is the escape hatch for a caller that
    /// already has a node id — a test harness reading the published tree JSON, or a
    /// control the projection deliberately does not make clickable, such as a large
    /// drag-sensing region. A well-formed request naming a node that no longer
    /// exists is dropped by egui on the next pass.
    pub fn request(&self, request: ActionRequest) {
        self.queue
            .push(egui::Event::AccessKitActionRequest(request));
    }

    /// What the DOM projection has cost so far.
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn stats(&self) -> accesskit_dom::Stats {
        self.adapter.borrow().stats()
    }
}
