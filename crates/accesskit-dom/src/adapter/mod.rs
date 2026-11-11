//! The DOM half of the projection: mounting, diffing and event wiring.
//!
//! Proxies nest to match the source hierarchy, so containment, reading order and grouping
//! survive the trip.
//! They are `opacity: 0` rather than `display: none` or `visibility: hidden`, either of which
//! would drop them from the accessibility tree.
//! They are `position: fixed`, so nesting them never affects where they sit.
//!
//! Activating a proxy sends an action request rather than a synthetic pointer event, so the
//! toolkit's own hit-testing never enters into it.

mod dom;
mod events;
mod proxy_element;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accesskit::TreeUpdate;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use wasm_bindgen::prelude::Closure;
use web_sys::{Document, HtmlCanvasElement, HtmlElement};

use self::events::CanvasResizeObserver;
use crate::metrics::Recorder;
use crate::plan::{Plan, Viewport, plan_tree};
use crate::{Error, Handler, Options};

/// A proxy element in the overlay, with the signatures of what was last written to it.
struct MountedProxy {
    element: HtmlElement,
    /// Recreated rather than mutated when the role changes the element type.
    tag: &'static str,
    /// [`Plan::description_signature`] as last written.
    description_signature: u64,
    /// [`Plan::geometry_signature`] as last written.
    geometry_signature: u64,
}

/// What [`Adapter::mount_proxy`] did to a node's proxy.
enum MountOutcome {
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
    canvas: HtmlCanvasElement,
    options: Options,
    /// Proxies by AccessKit node id.
    mounted: FxHashMap<u64, MountedProxy>,
    /// The last measurement, reused until the canvas changes.
    viewport: Option<Viewport>,
    /// Set by the `ResizeObserver`; makes the next pass re-measure the canvas.
    canvas_resized: Rc<Cell<bool>>,
    /// The toolkit focus last mirrored, so each focus change is mirrored only once.
    mirrored_focus: Option<u64>,
    #[expect(
        dead_code,
        reason = "the delegated DOM listeners must outlive the overlay they are bound to"
    )]
    listeners: Vec<Closure<dyn FnMut(web_sys::Event)>>,
    resize_observer: Option<CanvasResizeObserver>,
    recorder: Recorder,
    #[cfg(feature = "tree-json")]
    tree_json_publisher: Option<crate::tree_json::Publisher>,
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
        canvas: &HtmlCanvasElement,
        options: Options,
        handler: impl Handler + 'static,
    ) -> Result<Self, Error> {
        let document = canvas.owner_document().ok_or(Error::Detached)?;
        let overlay = dom::create_element(&document, "div")?;
        dom::set_attribute(&overlay, "id", &options.overlay_id);
        // Only the proxies hit-test, so a press landing between them still reaches the canvas.
        dom::set_style(
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

        let handler: Rc<RefCell<dyn Handler>> = Rc::new(RefCell::new(handler));
        let listeners = events::listen_on_overlay(&overlay, &options.overlay_id, &handler)?;

        let canvas_resized = Rc::new(Cell::new(true));
        #[cfg(feature = "tree-json")]
        let tree_json_publisher = options
            .tree_json_global
            .clone()
            .map(crate::tree_json::Publisher::new);

        Ok(Self {
            resize_observer: events::observe_canvas_resize(canvas, &canvas_resized),
            canvas: canvas.clone(),
            document,
            overlay,
            options,
            mounted: FxHashMap::default(),
            viewport: None,
            canvas_resized,
            mirrored_focus: None,
            listeners,
            recorder: Recorder::new(),
            #[cfg(feature = "tree-json")]
            tree_json_publisher,
        })
    }

    /// Mirrors `update` into the overlay, returning whether anything changed.
    ///
    /// `pixels_per_point` is the toolkit's device pixels per tree coordinate.
    pub fn update(&mut self, update: &TreeUpdate, pixels_per_point: f64) -> bool {
        let pass_start = self.recorder.start_pass();
        let viewport = self.measure_viewport(pixels_per_point);
        let canvas_area_css_px = viewport.width_css_px * viewport.height_css_px;
        let canvas_area_points =
            canvas_area_css_px / viewport.css_px_per_point.max(f64::EPSILON).powi(2);

        let plans = plan_tree(update, canvas_area_points, &self.options);
        let mut changed = false;
        let mut structure_changed = false;

        // Create or update a proxy per plan
        let mut planned_ids = FxHashSet::with_capacity_and_hasher(plans.len(), FxBuildHasher);
        for plan in &plans {
            planned_ids.insert(plan.node_id);
            match self.mount_proxy(plan, &viewport) {
                MountOutcome::Unchanged => {}
                MountOutcome::Updated => changed = true,
                MountOutcome::Created => {
                    changed = true;
                    structure_changed = true;
                }
            }
        }

        // Remove the proxies of nodes that are gone
        self.mounted.retain(|node_id, mounted| {
            if planned_ids.contains(node_id) {
                return true;
            }
            mounted.element.remove();
            changed = true;
            structure_changed = true;
            false
        });

        // Re-parenting reads every parent's live child list, so only passes that added or
        // removed a proxy pay for it.
        if structure_changed {
            self.arrange_proxies(&plans);
        }
        self.mirror_focus(update.focus.0);
        self.recorder.end_pass(pass_start);

        #[cfg(feature = "tree-json")]
        if changed && let Some(publisher) = self.tree_json_publisher.as_mut() {
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
    fn measure_viewport(&mut self, pixels_per_point: f64) -> Viewport {
        let canvas = &self.canvas;
        let backing_width_device_px = canvas.width();
        let backing_height_device_px = canvas.height();
        let resized = self.canvas_resized.replace(false);
        if !resized
            && let Some(viewport) = self.viewport
            && viewport.backing_width_device_px == backing_width_device_px
            && viewport.backing_height_device_px == backing_height_device_px
        {
            return viewport;
        }

        let rect = canvas.get_bounding_client_rect();
        // Derived from the canvas rather than the toolkit's zoom factor, because a toolkit may
        // keep sizing the backing store to the device pixel ratio it started with, and the
        // proxies must track what is on screen.
        let css_px_per_point = if backing_width_device_px > 0 && rect.width() > 0.0 {
            rect.width() * pixels_per_point / f64::from(backing_width_device_px)
        } else {
            1.0
        };
        let viewport = Viewport {
            left_css_px: rect.left(),
            top_css_px: rect.top(),
            width_css_px: rect.width(),
            height_css_px: rect.height(),
            backing_width_device_px,
            backing_height_device_px,
            css_px_per_point,
        };
        self.viewport = Some(viewport);
        viewport
    }

    /// Brings one node's proxy up to date, rewriting only what its signatures say changed.
    fn mount_proxy(&mut self, plan: &Plan<'_>, viewport: &Viewport) -> MountOutcome {
        let description_signature = plan.description_signature();
        let geometry_signature = plan.geometry_signature(viewport);
        let overlay_id = &self.options.overlay_id;

        // Same element type: rewrite in place
        if let Some(mounted) = self.mounted.get_mut(&plan.node_id)
            && mounted.tag == plan.proxy.tag
        {
            let describe = mounted.description_signature != description_signature;
            let place = mounted.geometry_signature != geometry_signature;
            if !describe && !place {
                return MountOutcome::Unchanged;
            }
            if describe {
                proxy_element::describe(&mounted.element, plan, overlay_id);
                self.recorder.record_description();
            }
            if place {
                proxy_element::place(&mounted.element, plan, viewport);
                self.recorder.record_placement();
            }
            mounted.description_signature = description_signature;
            mounted.geometry_signature = geometry_signature;
            return MountOutcome::Updated;
        }

        // New node, or a role change that needs another element type: recreate
        if let Some(stale) = self.mounted.remove(&plan.node_id) {
            stale.element.remove();
        }
        let Ok(element) = dom::create_element(&self.document, plan.proxy.tag) else {
            return MountOutcome::Unchanged;
        };
        proxy_element::describe(&element, plan, overlay_id);
        proxy_element::place(&element, plan, viewport);
        self.mounted.insert(
            plan.node_id,
            MountedProxy {
                element,
                tag: plan.proxy.tag,
                description_signature,
                geometry_signature,
            },
        );
        MountOutcome::Created
    }

    /// Puts every proxy under its parent, in the toolkit's own child order.
    ///
    /// The plans arrive depth-first, so a cursor per parent visits each parent's children in
    /// order, and only elements actually out of place are moved.
    fn arrange_proxies(&self, plans: &[Plan<'_>]) {
        let mut next_child_index: FxHashMap<Option<u64>, u32> = FxHashMap::default();
        for plan in plans {
            let Some(element) = self
                .mounted
                .get(&plan.node_id)
                .map(|mounted| &mounted.element)
            else {
                continue;
            };
            let parent = match plan.projected_parent {
                Some(parent_id) => match self.mounted.get(&parent_id) {
                    Some(mounted) => mounted.element.clone(),
                    None => continue,
                },
                None => self.overlay.clone(),
            };
            let child_index = next_child_index.entry(plan.projected_parent).or_insert(0);
            let current = parent.children().item(*child_index);
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
            *child_index += 1;
        }
    }

    /// Follows the toolkit's focus with the DOM, but only while the overlay already has it.
    ///
    /// Moving focus unasked would take it from the canvas whenever the toolkit focused
    /// anything, breaking ordinary mouse and keyboard use.
    fn mirror_focus(&mut self, focused_node_id: u64) {
        if self.mirrored_focus == Some(focused_node_id) {
            return;
        }
        self.mirrored_focus = Some(focused_node_id);
        if !events::overlay_has_focus(&self.document, &self.options.overlay_id) {
            return;
        }
        if let Some(mounted) = self.mounted.get(&focused_node_id)
            && mounted.element.focus().is_err()
        {
            log::warn!("accesskit-dom: could not move focus to node {focused_node_id}");
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if let Some(resize_observer) = &self.resize_observer {
            resize_observer.observer.disconnect();
        }
        // Removing the overlay takes every proxy and delegated listener with it.
        self.overlay.remove();
    }
}
