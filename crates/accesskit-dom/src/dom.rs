//! The DOM half of the projection: mounting, diffing and event wiring.
//!
//! Proxies nest to match the source hierarchy, so containment, reading order and grouping
//! survive the trip.
//! They are `opacity: 0` rather than `display: none` or `visibility: hidden`, either of which
//! would drop them from the accessibility tree, and `position: fixed`, so nesting them never
//! affects where they sit.
//!
//! Activating a proxy sends an action request rather than a synthetic pointer event, so the
//! toolkit's own hit-testing never enters into it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accesskit::{
    Action, ActionRequest, Live, Node, NodeId, Orientation, Role, Toggled, TreeId, TreeUpdate,
};
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, Element, HtmlElement};

use crate::metrics::Recorder;
use crate::tree::{Plan, Viewport, plan};
use crate::{Error, Handler, KeyEvent, Options};

/// Attribute carrying the AccessKit node id on each proxy, for event routing and
/// for harnesses that want to correlate the DOM with the published tree.
const NODE_ATTR: &str = "data-ak";

struct Mounted {
    element: HtmlElement,
    /// Recreated rather than mutated when the role changes the element type.
    tag: &'static str,
    /// Signature of role, name and state, everything `describe` writes.
    ///
    /// Kept apart from `geometry` because the two change independently: a dragged slider
    /// changes its value and moves nothing, while a resize moves every proxy and renames none.
    description: u64,
    /// Signature of position and size, everything `place` writes.
    geometry: u64,
}

/// A `ResizeObserver` and the callback it holds, which must outlive it.
type CanvasObserver = (web_sys::ResizeObserver, Closure<dyn FnMut(js_sys::Array)>);

/// What [`Adapter::mount`] did to a node's proxy.
enum Applied {
    Unchanged,
    Updated,
    Created,
}

/// Projects [`TreeUpdate`]s into a DOM overlay above one canvas.
///
/// Build one per canvas with [`Adapter::new`] and feed it every update the toolkit produces
/// via [`Adapter::update`].
/// Input events on the proxies reach the [`Handler`] given at construction.
/// Dropping the adapter removes the overlay and everything in it.
pub struct Adapter {
    document: Document,
    overlay: HtmlElement,
    canvas: web_sys::HtmlCanvasElement,
    options: Options,
    mounted: FxHashMap<u64, Mounted>,
    viewport: Option<Viewport>,
    /// Set by the `ResizeObserver`; makes the next pass re-measure the canvas.
    resized: Rc<Cell<bool>>,
    /// The toolkit focus last mirrored, so each focus change is mirrored only once.
    mirrored_focus: Option<u64>,
    #[expect(
        dead_code,
        reason = "the delegated DOM listeners must outlive the overlay they are bound to"
    )]
    listeners: Vec<Closure<dyn FnMut(web_sys::Event)>>,
    resize: Option<CanvasObserver>,
    recorder: Recorder,
    #[cfg(feature = "tree-json")]
    publisher: Option<crate::tree_json::Publisher>,
}

impl Adapter {
    /// Builds the overlay next to `canvas` and wires its event listeners.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Detached`] when the canvas has no owning document,
    /// [`Error::NoBody`] when that document has no `<body>` to hold the overlay, and
    /// [`Error::Dom`] when creating the overlay or attaching a listener fails.
    pub fn new(
        canvas: &web_sys::HtmlCanvasElement,
        options: Options,
        handler: impl Handler + 'static,
    ) -> Result<Self, Error> {
        let document = canvas.owner_document().ok_or(Error::Detached)?;
        let overlay = create_element(&document, "div")?;
        set_attribute(&overlay, "id", &options.overlay_id);
        // Only the proxies hit-test, so a press landing between them still reaches the canvas.
        set_style(
            &overlay,
            &[
                ("position", "fixed"),
                ("left", "0"),
                ("top", "0"),
                ("pointer-events", "none"),
                ("z-index", &options.z_index.to_string()),
            ],
        );
        document
            .body()
            .ok_or(Error::NoBody)?
            .append_child(&overlay)
            .map_err(|err| Error::Dom(format!("{err:?}")))?;

        // One delegated set of listeners serves every proxy.
        // Each holds its own handle to the handler, so none reaches back into the adapter
        // while `update` borrows it mutably.
        let handler: Rc<RefCell<dyn Handler>> = Rc::new(RefCell::new(handler));
        let selector = format!("#{}", options.overlay_id);
        let listeners = vec![
            listen(&overlay, "click", {
                let handler = Rc::clone(&handler);
                move |event| on_click(&handler, &event)
            })?,
            listen(&overlay, "focusin", {
                let handler = Rc::clone(&handler);
                move |event| on_focus_in(&handler, &event)
            })?,
            listen(&overlay, "focusout", {
                let handler = Rc::clone(&handler);
                let selector = selector.clone();
                move |event| on_focus_out(&handler, &selector, event)
            })?,
            listen(&overlay, "keydown", {
                let handler = Rc::clone(&handler);
                move |event| on_key(&handler, event, true)
            })?,
            listen(&overlay, "keyup", {
                let handler = Rc::clone(&handler);
                move |event| on_key(&handler, event, false)
            })?,
        ];

        let resized = Rc::new(Cell::new(true));
        #[cfg(feature = "tree-json")]
        let publisher = options
            .tree_json_global
            .clone()
            .map(crate::tree_json::Publisher::new);

        Ok(Self {
            resize: observe_canvas(canvas, &resized),
            canvas: canvas.clone(),
            document,
            overlay,
            options,
            mounted: FxHashMap::default(),
            viewport: None,
            resized,
            mirrored_focus: None,
            listeners,
            recorder: Recorder::new(),
            #[cfg(feature = "tree-json")]
            publisher,
        })
    }

    /// Mirrors `update` into the overlay, returning whether anything changed.
    ///
    /// `pixels_per_point` is the toolkit's device pixels per tree coordinate.
    pub fn update(&mut self, update: &TreeUpdate, pixels_per_point: f64) -> bool {
        let started = self.recorder.begin();
        let viewport = self.measure(pixels_per_point);
        let canvas_area = viewport.width * viewport.height;

        let plans = plan(
            update,
            canvas_area / viewport.scale.max(f64::EPSILON).powi(2),
            &self.options,
        );
        let mut changed = false;
        let mut structure_changed = false;

        let mut wanted = FxHashSet::with_capacity_and_hasher(plans.len(), FxBuildHasher);
        for item in &plans {
            wanted.insert(item.id);
            match self.mount(item, &viewport) {
                Applied::Unchanged => {}
                Applied::Updated => changed = true,
                Applied::Created => {
                    changed = true;
                    structure_changed = true;
                }
            }
        }

        self.mounted.retain(|id, live| {
            if wanted.contains(id) {
                return true;
            }
            live.element.remove();
            changed = true;
            structure_changed = true;
            false
        });

        // Re-parenting reads every parent's live child list, so only passes that added or
        // removed a proxy pay for it.
        if structure_changed {
            self.arrange(&plans);
        }
        self.mirror_focus(update.focus.0);
        self.recorder.end_pass(started);

        #[cfg(feature = "tree-json")]
        if changed && let Some(publisher) = self.publisher.as_mut() {
            publisher.publish(update);
        }
        changed
    }

    /// What the projection has cost so far.
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn stats(&self) -> crate::Stats {
        self.recorder.stats()
    }

    /// Re-measures the canvas only when it was resized or its backing store changed.
    ///
    /// `getBoundingClientRect` forces a layout flush, so it must not run every pass.
    /// The backing store's size is a plain attribute read, and it catches device pixel ratio
    /// changes that a `ResizeObserver` alone would miss.
    fn measure(&mut self, pixels_per_point: f64) -> Viewport {
        let canvas = &self.canvas;
        let backing = (canvas.width(), canvas.height());
        let resized = self.resized.replace(false);
        if !resized
            && let Some(viewport) = self.viewport
            && viewport.backing == backing
        {
            return viewport;
        }

        let rect = canvas.get_bounding_client_rect();
        // Derived from the canvas rather than the toolkit's zoom factor, because a toolkit may
        // keep sizing the backing store to the device pixel ratio it started with, and the
        // proxies must track what is on screen.
        let scale = if backing.0 > 0 && rect.width() > 0.0 {
            rect.width() * pixels_per_point / f64::from(backing.0)
        } else {
            1.0
        };
        let viewport = Viewport {
            left: rect.left(),
            top: rect.top(),
            width: rect.width(),
            height: rect.height(),
            backing,
            scale,
        };
        self.viewport = Some(viewport);
        viewport
    }

    fn mount(&mut self, item: &Plan<'_>, viewport: &Viewport) -> Applied {
        let description = item.description_signature();
        let geometry = item.geometry_signature(viewport);
        if let Some(live) = self.mounted.get(&item.id)
            && live.tag == item.proxy.tag
        {
            if live.description == description && live.geometry == geometry {
                return Applied::Unchanged;
            }
            let element = live.element.clone();
            if live.description != description {
                describe(&element, item, &self.options.overlay_id);
                self.recorder.described();
            }
            if live.geometry != geometry {
                place(&element, item, viewport);
                self.recorder.placed();
            }
            if let Some(live) = self.mounted.get_mut(&item.id) {
                live.description = description;
                live.geometry = geometry;
            }
            return Applied::Updated;
        }

        if let Some(stale) = self.mounted.remove(&item.id) {
            stale.element.remove();
        }
        let Ok(element) = create_element(&self.document, item.proxy.tag) else {
            return Applied::Unchanged;
        };
        describe(&element, item, &self.options.overlay_id);
        place(&element, item, viewport);
        self.mounted.insert(
            item.id,
            Mounted {
                element,
                tag: item.proxy.tag,
                description,
                geometry,
            },
        );
        Applied::Created
    }

    /// Puts every proxy under its parent, in the toolkit's own child order.
    ///
    /// The plans arrive depth-first, so a cursor per parent visits each parent's children in
    /// order, and only elements actually out of place are moved.
    fn arrange(&self, plans: &[Plan<'_>]) {
        let mut cursor: FxHashMap<Option<u64>, u32> = FxHashMap::default();
        for item in plans {
            let Some(element) = self.mounted.get(&item.id).map(|live| &live.element) else {
                continue;
            };
            let parent = match item.parent {
                Some(parent) => match self.mounted.get(&parent) {
                    Some(live) => live.element.clone(),
                    None => continue,
                },
                None => self.overlay.clone(),
            };
            let index = cursor.entry(item.parent).or_insert(0);
            let current = parent.children().item(*index);
            let in_place = current
                .as_ref()
                .is_some_and(|node| element.is_same_node(Some(node)));
            if !in_place
                && parent
                    .insert_before(element, current.as_ref().map(AsRef::as_ref))
                    .is_err()
            {
                continue;
            }
            *index += 1;
        }
    }

    /// Follows the toolkit's focus with the DOM, but only while the overlay already has it.
    ///
    /// Moving focus unasked would take it from the canvas whenever the toolkit focused
    /// anything, breaking ordinary mouse and keyboard use.
    fn mirror_focus(&mut self, focus: u64) {
        if self.mirrored_focus == Some(focus) {
            return;
        }
        self.mirrored_focus = Some(focus);
        if !overlay_has_focus(&self.document, &self.options.overlay_id) {
            return;
        }
        if let Some(live) = self.mounted.get(&focus)
            && live.element.focus().is_err()
        {
            log::warn!("accesskit-dom: could not move focus to node {focus}");
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if let Some((observer, _)) = &self.resize {
            observer.disconnect();
        }
        // Removing the overlay takes every proxy and delegated listener with it.
        self.overlay.remove();
    }
}

fn observe_canvas(
    canvas: &web_sys::HtmlCanvasElement,
    resized: &Rc<Cell<bool>>,
) -> Option<CanvasObserver> {
    let resized = Rc::clone(resized);
    let closure = Closure::<dyn FnMut(js_sys::Array)>::new(move |_: js_sys::Array| {
        resized.set(true);
    });
    let observer = web_sys::ResizeObserver::new(closure.as_ref().unchecked_ref()).ok()?;
    observer.observe(canvas);
    Some((observer, closure))
}

fn overlay_has_focus(document: &Document, overlay_id: &str) -> bool {
    document
        .active_element()
        .and_then(|element| element.closest(&format!("#{overlay_id}")).ok().flatten())
        .is_some()
}

/// The AccessKit node id carried by the proxy an event targeted.
fn event_node_id(event: &web_sys::Event) -> Option<u64> {
    let id = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|target| target.get_attribute(NODE_ATTR))?;
    id.parse().ok()
}

fn request(handler: &Rc<RefCell<dyn Handler>>, action: Action, node: u64) {
    handler.borrow_mut().action_request(ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: NodeId(node),
        data: None,
    });
}

fn on_click(handler: &Rc<RefCell<dyn Handler>>, event: &web_sys::Event) {
    let Some(node) = event_node_id(event) else {
        log::warn!("accesskit-dom: dropped a click on an unidentified proxy");
        return;
    };
    request(handler, Action::Click, node);
}

fn on_focus_in(handler: &Rc<RefCell<dyn Handler>>, event: &web_sys::Event) {
    handler.borrow_mut().focus_changed(true);
    if let Some(node) = event_node_id(event) {
        request(handler, Action::Focus, node);
    }
}

fn on_focus_out(handler: &Rc<RefCell<dyn Handler>>, overlay_selector: &str, event: web_sys::Event) {
    // `relatedTarget` is where focus is going, which `document.activeElement` does
    // not yet report while the event is being dispatched.
    let staying = event
        .dyn_into::<web_sys::FocusEvent>()
        .ok()
        .and_then(|event| event.related_target())
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|element| element.closest(overlay_selector).ok().flatten())
        .is_some();
    handler.borrow_mut().focus_changed(staying);
}

fn on_key(handler: &Rc<RefCell<dyn Handler>>, event: web_sys::Event, pressed: bool) {
    let Ok(event) = event.dyn_into::<web_sys::KeyboardEvent>() else {
        return;
    };
    // Tab keeps moving DOM focus between proxies.
    // Every other key the toolkit wants is consumed, so Enter and Space do not fire a second
    // activation as a synthetic click.
    if event.key() == "Tab" {
        return;
    }
    let key = KeyEvent {
        key: event.key(),
        repeat: event.repeat(),
        alt: event.alt_key(),
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        meta: event.meta_key(),
    };
    if handler.borrow_mut().key_event(&key, pressed) {
        event.prevent_default();
    }
}

fn element_id(overlay_id: &str, node: u64) -> String {
    format!("{overlay_id}-{node}")
}

/// Writes the proxy's whole appearance and position as one `style` attribute.
///
/// One attribute write replaces sixteen `setProperty` calls, each a separate crossing into JS.
fn place(element: &HtmlElement, item: &Plan<'_>, viewport: &Viewport) {
    const BASE: &str = "position:fixed;margin:0;padding:0;border:0;background:transparent;\
                        color:transparent;font:inherit;overflow:hidden;white-space:nowrap;opacity:0;";

    let pointer = if item.proxy.hit_testable {
        "pointer-events:auto;"
    } else {
        "pointer-events:none;"
    };

    let Some(bounds) = item.node.bounds() else {
        // Keep a boundless node in the tree but sized to nothing, not parked over the origin.
        set_attribute(
            element,
            "style",
            &format!("{BASE}{pointer}width:0;height:0;"),
        );
        return;
    };
    let scale = viewport.scale;
    let width = ((bounds.x1 - bounds.x0) * scale).max(0.0);
    let height = ((bounds.y1 - bounds.y0) * scale).max(0.0);
    // Smaller proxies stack above larger ones, because some toolkits send nodes in hash
    // order and paint order alone would decide overlap arbitrarily.
    let depth = (10_000.0 - width * height / 1_000.0).clamp(1.0, 10_000.0);
    let left = viewport.left + bounds.x0 * scale;
    let top = viewport.top + bounds.y0 * scale;
    set_attribute(
        element,
        "style",
        &format!(
            "{BASE}{pointer}left:{left:.1}px;top:{top:.1}px;\
             width:{width:.1}px;height:{height:.1}px;z-index:{depth:.0};"
        ),
    );
}

fn describe(element: &HtmlElement, item: &Plan<'_>, overlay_id: &str) {
    let node = item.node;
    set_attribute(element, NODE_ATTR, &item.id.to_string());
    // Needed so `aria-labelledby` has something to point at.
    set_attribute(element, "id", &element_id(overlay_id, item.id));
    if let Some(role) = item.proxy.role {
        set_attribute(element, "role", role);
    }

    if item.proxy.interactive {
        // Focus moving here is routed back to the toolkit as an AccessKit focus request.
        set_attribute(element, "tabindex", "0");
    }
    if item.proxy.text_content {
        element.set_text_content(Some(item.proxy.name));
    } else if item.proxy.name.is_empty() {
        // A nameless container stays anonymous rather than repeating its children's text.
        remove_attribute(element, "aria-label");
    } else {
        set_attribute(element, "aria-label", item.proxy.name);
    }

    if let Some(label) = item.proxy.labelled_by {
        set_attribute(element, "aria-labelledby", &element_id(overlay_id, label));
    } else {
        remove_attribute(element, "aria-labelledby");
    }

    describe_state(element, node);
    describe_value(element, node, item.proxy.role);
    describe_grid(element, node);
}

fn describe_state(element: &HtmlElement, node: &Node) {
    match node.toggled() {
        Some(Toggled::True) => set_attribute(element, "aria-checked", "true"),
        Some(Toggled::False) => set_attribute(element, "aria-checked", "false"),
        Some(Toggled::Mixed) => set_attribute(element, "aria-checked", "mixed"),
        None => remove_attribute(element, "aria-checked"),
    }
    toggle_attribute(element, "aria-expanded", node.is_expanded());
    toggle_attribute(element, "aria-selected", node.is_selected());
    flag_attribute(element, "aria-readonly", node.is_read_only());
    flag_attribute(element, "aria-required", node.is_required());
    flag_attribute(element, "aria-busy", node.is_busy());

    if node.is_disabled() {
        set_attribute(element, "aria-disabled", "true");
        // A disabled `<button>` fires no click, which is what we want: automation
        // should refuse the interaction rather than silently do nothing.
        set_attribute(element, "disabled", "");
    } else {
        remove_attribute(element, "aria-disabled");
        remove_attribute(element, "disabled");
    }

    match node.live() {
        Some(Live::Polite) => announce(element, "polite"),
        Some(Live::Assertive) => announce(element, "assertive"),
        Some(Live::Off) | None => {
            remove_attribute(element, "aria-live");
            remove_attribute(element, "aria-atomic");
        }
    }

    if let Some(description) = node.description() {
        set_attribute(element, "aria-description", description);
    }
    if let Some(placeholder) = node.placeholder() {
        set_attribute(element, "aria-placeholder", placeholder);
    }
    match node.orientation() {
        Some(Orientation::Horizontal) => set_attribute(element, "aria-orientation", "horizontal"),
        Some(Orientation::Vertical) => set_attribute(element, "aria-orientation", "vertical"),
        _ => remove_attribute(element, "aria-orientation"),
    }
}

fn announce(element: &HtmlElement, politeness: &str) {
    set_attribute(element, "aria-live", politeness);
    // Read the whole message rather than only the words that changed.
    set_attribute(element, "aria-atomic", "true");
}

fn describe_value(element: &HtmlElement, node: &Node, role: Option<&str>) {
    if let Some(value) = node.numeric_value() {
        set_attribute(element, "aria-valuenow", &value.to_string());
    } else {
        remove_attribute(element, "aria-valuenow");
    }
    // A range without bounds is nearly useless to a screen reader: "47" alone says
    // nothing about how far along the control sits.
    if let Some(min) = node.min_numeric_value() {
        set_attribute(element, "aria-valuemin", &min.to_string());
    }
    if let Some(max) = node.max_numeric_value() {
        set_attribute(element, "aria-valuemax", &max.to_string());
    }
    if node.role() == Role::PasswordInput {
        // Never publish a password: the projection is a parallel copy of the UI in
        // the DOM, readable by anything else running on the page.
        element.set_text_content(None);
        remove_attribute(element, "aria-valuetext");
    } else if let Some(text) = node.value() {
        if role == Some("textbox") {
            // A textbox is named by its label and read from its contents. Its only
            // children in the source tree are text runs, which are not projected, so
            // writing text here deletes nothing.
            element.set_text_content(Some(text));
        } else {
            set_attribute(element, "aria-valuetext", text);
        }
    }
    if node.role() == Role::MultilineTextInput {
        set_attribute(element, "aria-multiline", "true");
    }
}

fn describe_grid(element: &HtmlElement, node: &Node) {
    // ARIA counts rows and columns from one; AccessKit indexes them from zero.
    if let Some(row) = node.row_index() {
        set_attribute(element, "aria-rowindex", &row.saturating_add(1).to_string());
    }
    if let Some(column) = node.column_index() {
        set_attribute(
            element,
            "aria-colindex",
            &column.saturating_add(1).to_string(),
        );
    }
    if let Some(rows) = node.row_count() {
        set_attribute(element, "aria-rowcount", &rows.to_string());
    }
    if let Some(columns) = node.column_count() {
        set_attribute(element, "aria-colcount", &columns.to_string());
    }
}

fn toggle_attribute(element: &HtmlElement, name: &str, value: Option<bool>) {
    match value {
        Some(value) => set_attribute(element, name, if value { "true" } else { "false" }),
        None => remove_attribute(element, name),
    }
}

fn flag_attribute(element: &HtmlElement, name: &str, value: bool) {
    if value {
        set_attribute(element, name, "true");
    } else {
        remove_attribute(element, name);
    }
}

fn create_element(document: &Document, tag: &str) -> Result<HtmlElement, Error> {
    document
        .create_element(tag)
        .map_err(|err| Error::Dom(format!("{err:?}")))?
        .dyn_into::<HtmlElement>()
        .map_err(|element| Error::Dom(format!("not an HTML element: {element:?}")))
}

fn set_style(element: &HtmlElement, properties: &[(&str, &str)]) {
    let style = element.style();
    for (name, value) in properties {
        if let Err(err) = style.set_property(name, value) {
            log::warn!("accesskit-dom: could not set style {name}: {err:?}");
        }
    }
}

fn set_attribute(element: &HtmlElement, name: &str, value: &str) {
    if let Err(err) = element.set_attribute(name, value) {
        log::warn!("accesskit-dom: could not set attribute {name}: {err:?}");
    }
}

fn remove_attribute(element: &HtmlElement, name: &str) {
    if let Err(err) = element.remove_attribute(name) {
        log::warn!("accesskit-dom: could not remove attribute {name}: {err:?}");
    }
}

fn listen(
    target: &HtmlElement,
    event: &str,
    handler: impl FnMut(web_sys::Event) + 'static,
) -> Result<Closure<dyn FnMut(web_sys::Event)>, Error> {
    let closure = Closure::<dyn FnMut(web_sys::Event)>::new(handler);
    target
        .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
        .map_err(|err| Error::Dom(format!("{err:?}")))?;
    Ok(closure)
}
