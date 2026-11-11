//! DOM events in: delegated listeners on the overlay, and the canvas resize observer.
//!
//! Every listener reports to the caller's [`Handler`] in AccessKit terms.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accesskit::{Action, ActionRequest, NodeId, TreeId};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, HtmlCanvasElement, HtmlElement};

use super::proxy_element::NODE_ID_ATTRIBUTE;
use crate::{Error, Handler, KeyEvent};

type SharedHandler = Rc<RefCell<dyn Handler>>;
type EventListener = Closure<dyn FnMut(web_sys::Event)>;

/// A `ResizeObserver` and the callback it holds, which must outlive it.
pub(super) struct CanvasResizeObserver {
    pub observer: web_sys::ResizeObserver,
    #[expect(
        dead_code,
        reason = "the observer calls back into this closure for as long as it is connected"
    )]
    callback: Closure<dyn FnMut(js_sys::Array)>,
}

/// Wires one delegated set of listeners on the overlay, serving every proxy.
///
/// Each listener holds its own handle to the handler, so none reaches back into the adapter
/// while `Adapter::update` borrows it mutably.
pub(super) fn listen_on_overlay(
    overlay: &HtmlElement,
    overlay_id: &str,
    handler: &SharedHandler,
) -> Result<Vec<EventListener>, Error> {
    let overlay_selector = format!("#{overlay_id}");
    Ok(vec![
        listen(overlay, "click", {
            let handler = Rc::clone(handler);
            move |event| on_click(&handler, &event)
        })?,
        listen(overlay, "focusin", {
            let handler = Rc::clone(handler);
            move |event| on_focus_in(&handler, &event)
        })?,
        listen(overlay, "focusout", {
            let handler = Rc::clone(handler);
            move |event| on_focus_out(&handler, &overlay_selector, event)
        })?,
        listen(overlay, "keydown", {
            let handler = Rc::clone(handler);
            move |event| on_key(&handler, event, true)
        })?,
        listen(overlay, "keyup", {
            let handler = Rc::clone(handler);
            move |event| on_key(&handler, event, false)
        })?,
    ])
}

/// Sets `resized` whenever the canvas changes size; `None` where `ResizeObserver` is missing.
pub(super) fn observe_canvas_resize(
    canvas: &HtmlCanvasElement,
    resized: &Rc<Cell<bool>>,
) -> Option<CanvasResizeObserver> {
    let resized = Rc::clone(resized);
    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |_: js_sys::Array| {
        resized.set(true);
    });
    let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok()?;
    observer.observe(canvas);
    Some(CanvasResizeObserver { observer, callback })
}

/// Whether DOM focus sits inside the overlay.
pub(super) fn overlay_has_focus(document: &Document, overlay_id: &str) -> bool {
    document
        .active_element()
        .and_then(|element| element.closest(&format!("#{overlay_id}")).ok().flatten())
        .is_some()
}

fn listen(
    target: &HtmlElement,
    event_type: &str,
    on_event: impl FnMut(web_sys::Event) + 'static,
) -> Result<EventListener, Error> {
    let listener = EventListener::new(on_event);
    target
        .add_event_listener_with_callback(event_type, listener.as_ref().unchecked_ref())
        .map_err(|err| Error::Dom(format!("{err:?}")))?;
    Ok(listener)
}

/// The AccessKit node id carried by the proxy an event targeted.
fn event_node_id(event: &web_sys::Event) -> Option<u64> {
    let node_id = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|target| target.get_attribute(NODE_ID_ATTRIBUTE))?;
    node_id.parse().ok()
}

fn request_action(handler: &SharedHandler, action: Action, node_id: u64) {
    handler.borrow_mut().action_request(ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: NodeId(node_id),
        data: None,
    });
}

fn on_click(handler: &SharedHandler, event: &web_sys::Event) {
    let Some(node_id) = event_node_id(event) else {
        log::warn!("accesskit-dom: dropped a click on an unidentified proxy");
        return;
    };
    request_action(handler, Action::Click, node_id);
}

fn on_focus_in(handler: &SharedHandler, event: &web_sys::Event) {
    handler.borrow_mut().focus_changed(true);
    if let Some(node_id) = event_node_id(event) {
        request_action(handler, Action::Focus, node_id);
    }
}

fn on_focus_out(handler: &SharedHandler, overlay_selector: &str, event: web_sys::Event) {
    // `relatedTarget` is where focus is going, which `document.activeElement` does not yet
    // report while the event is being dispatched.
    let focus_stays_in_overlay = event
        .dyn_into::<web_sys::FocusEvent>()
        .ok()
        .and_then(|event| event.related_target())
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|element| element.closest(overlay_selector).ok().flatten())
        .is_some();
    handler.borrow_mut().focus_changed(focus_stays_in_overlay);
}

fn on_key(handler: &SharedHandler, event: web_sys::Event, pressed: bool) {
    let Ok(event) = event.dyn_into::<web_sys::KeyboardEvent>() else {
        return;
    };
    // Tab keeps moving DOM focus between proxies.
    // Every other key the toolkit wants is consumed, so Enter and Space do not fire a second
    // activation as a synthetic click.
    if event.key() == "Tab" {
        return;
    }
    let key_event = KeyEvent {
        key: event.key(),
        repeat: event.repeat(),
        alt: event.alt_key(),
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        meta: event.meta_key(),
    };
    if handler.borrow_mut().key_event(&key_event, pressed) {
        event.prevent_default();
    }
}
