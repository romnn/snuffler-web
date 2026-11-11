//! The wasm-only wiring: adapter construction, the input queue, and the drain.
//!
//! Proxy input is queued and reaches the next pass through [`A11y::drain`], not
//! [`egui::Context::input_mut`]: egui rebuilds `InputState` from the frame's `RawInput` in
//! `begin_pass`, discarding anything written between passes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use egui::accesskit::{ActionRequest, TreeUpdate};

use crate::keys;

/// Input queued by the DOM listeners (via [`ProxyInputHandler`]) and
/// [`A11y::request_action`] for [`A11y::drain`].
struct InputQueue {
    /// Kept so queued input can wake an idle app, since egui only repaints on demand.
    ctx: egui::Context,
    events: RefCell<Vec<egui::Event>>,
    /// Whether DOM focus currently sits on one of the proxies.
    proxy_has_focus: Cell<bool>,
}

impl InputQueue {
    fn push(&self, event: egui::Event) {
        self.events.borrow_mut().push(event);
        self.ctx.request_repaint();
    }
}

/// Feeds proxy input from the adapter into the [`InputQueue`].
struct ProxyInputHandler {
    queue: Rc<InputQueue>,
}

impl accesskit_dom::Handler for ProxyInputHandler {
    fn action_request(&mut self, request: ActionRequest) {
        self.queue
            .push(egui::Event::AccessKitActionRequest(request));
    }

    fn key_event(&mut self, event: &accesskit_dom::KeyEvent, pressed: bool) -> bool {
        let wanted = keys::translate(event, pressed, &mut self.queue.events.borrow_mut());
        if wanted {
            self.queue.ctx.request_repaint();
        }
        wanted
    }

    fn focus_changed(&mut self, focus_within: bool) {
        if self.queue.proxy_has_focus.replace(focus_within) != focus_within {
            self.queue.ctx.request_repaint();
        }
    }
}

/// Handle to an installed accessibility layer.
///
/// Cheap to clone; all clones share one adapter and one event queue.
/// The overlay lives as long as any clone does.
#[derive(Clone)]
pub struct A11y {
    queue: Rc<InputQueue>,
    adapter: Rc<RefCell<accesskit_dom::Adapter>>,
}

/// Turns on AccessKit for `ctx` and builds the DOM projection over `canvas`.
///
/// The app still feeds each pass's tree to [`A11y::on_tree_update`] and calls
/// [`A11y::drain`] from its runner's raw-input hook.
///
/// # Errors
///
/// Returns [`accesskit_dom::Error`] when the overlay cannot be built; see
/// [`accesskit_dom::Adapter::new`].
pub fn install(
    ctx: &egui::Context,
    canvas: &web_sys::HtmlCanvasElement,
    options: accesskit_dom::Options,
) -> Result<A11y, accesskit_dom::Error> {
    ctx.enable_accesskit();
    let queue = Rc::new(InputQueue {
        ctx: ctx.clone(),
        events: RefCell::new(Vec::new()),
        proxy_has_focus: Cell::new(false),
    });
    let handler = ProxyInputHandler {
        queue: Rc::clone(&queue),
    };
    let adapter = accesskit_dom::Adapter::new(canvas, options, handler)?;
    Ok(A11y {
        queue,
        adapter: Rc::new(RefCell::new(adapter)),
    })
}

impl A11y {
    /// Mirrors one pass's tree into the DOM overlay; an unchanged tree costs no DOM writes.
    pub fn on_tree_update(&self, update: &TreeUpdate) {
        let pixels_per_point = f64::from(self.queue.ctx.pixels_per_point());
        self.adapter.borrow_mut().update(update, pixels_per_point);
    }

    /// Hands the queued accessibility input to this pass.
    ///
    /// Call from the web runner's raw-input hook (eframe:
    /// [`raw_input_hook`](https://docs.rs/eframe/latest/eframe/trait.App.html#method.raw_input_hook)).
    pub fn drain(&self, raw_input: &mut egui::RawInput) {
        if self.queue.proxy_has_focus.get() {
            // The runner derives `focused` from the canvas, which has no DOM focus while a
            // proxy does.
            // Uncorrected, egui would hide the text cursor while the user types.
            raw_input.focused = true;
        }
        raw_input.events.append(&mut self.queue.events.borrow_mut());
    }

    /// Queues an AccessKit action request as if a proxy had raised it.
    ///
    /// The escape hatch for a caller that already has a node id, such as a harness reading
    /// the tree JSON.
    /// egui drops a request naming a node that no longer exists.
    pub fn request_action(&self, request: ActionRequest) {
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
