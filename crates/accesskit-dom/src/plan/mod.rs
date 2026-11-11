//! Planning: deciding what each AccessKit node becomes, before any DOM is touched.
//!
//! Everything here is pure, so role mapping, name resolution, label borrowing and region
//! demotion are unit-tested on the host.
//! The wasm-only `adapter` module then diffs each plan's signatures against what is mounted.

mod labels;
mod proxy;
mod signature;
#[cfg(test)]
mod tests;

use accesskit::{Node, TreeUpdate};
use rustc_hash::{FxHashMap, FxHashSet};

pub(crate) use self::proxy::Proxy;
use crate::Options;

/// AccessKit nodes keyed by their raw id, for walking the tree an update describes.
type NodesById<'a> = FxHashMap<u64, &'a Node>;

/// Where the canvas sits on the page and how many CSS pixels one tree coordinate covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewport {
    pub left_css_px: f64,
    pub top_css_px: f64,
    pub width_css_px: f64,
    pub height_css_px: f64,
    /// The canvas backing store's width, which tracks the device pixel ratio.
    pub backing_width_device_px: u32,
    pub backing_height_device_px: u32,
    /// CSS pixels per tree coordinate (an egui "point").
    pub css_px_per_point: f64,
}

/// One node's place in the projected tree.
pub(crate) struct Plan<'a> {
    pub node_id: u64,
    /// Nearest projected ancestor; `None` means the overlay itself.
    pub projected_parent: Option<u64>,
    pub node: &'a Node,
    pub proxy: Proxy<'a>,
}

/// The [`Options`] the walk needs, resolved once per pass.
pub(crate) struct Policy {
    /// Area (in tree coordinates) at which a clickable node becomes a region.
    pub region_area_points: f64,
    /// Whether a control proxy is the browser's hit target; see [`Options::pointer_proxies`].
    pub pointer_proxies: bool,
}

/// Walks the tree the toolkit sent, deciding what each node becomes.
///
/// The plans come out depth-first, in the toolkit's own child order.
pub(crate) fn plan_tree<'a>(
    update: &'a TreeUpdate,
    canvas_area_points: f64,
    options: &Options,
) -> Vec<Plan<'a>> {
    let nodes_by_id: NodesById<'a> = update.nodes.iter().map(|(id, node)| (id.0, node)).collect();
    let Some(root_id) = update.tree.as_ref().map(|tree| tree.root.0) else {
        return Vec::new();
    };
    let Some(root_node) = nodes_by_id.get(&root_id).copied() else {
        return Vec::new();
    };

    let policy = Policy {
        region_area_points: canvas_area_points * options.region_area_fraction,
        pointer_proxies: options.pointer_proxies,
    };
    let mut plans = Vec::with_capacity(update.nodes.len());
    let mut visited = FxHashSet::default();
    visited.insert(root_id);

    // The root stands for the overlay itself, so its children start at the top.
    walk_children(
        root_node,
        None,
        &nodes_by_id,
        &mut visited,
        &policy,
        &mut plans,
    );

    labels::link_explicit_labels(&mut plans);
    labels::borrow_adjacent_labels(&mut plans, options.label_gap_points);
    plans
}

/// Plans every not-yet-visited child of `parent_node`, depth-first.
fn walk_children<'a>(
    parent_node: &'a Node,
    projected_parent: Option<u64>,
    nodes_by_id: &NodesById<'a>,
    visited: &mut FxHashSet<u64>,
    policy: &Policy,
    plans: &mut Vec<Plan<'a>>,
) {
    for child_id in parent_node.children() {
        let node_id = child_id.0;
        if !visited.insert(node_id) {
            continue;
        }
        let Some(node) = nodes_by_id.get(&node_id).copied() else {
            continue;
        };
        let projected_parent_of_children = match Proxy::for_node(node, nodes_by_id, policy) {
            Some(proxy) => {
                plans.push(Plan {
                    node_id,
                    projected_parent,
                    node,
                    proxy,
                });
                Some(node_id)
            }
            // An unprojected node's children are hoisted to the nearest projected ancestor.
            None => projected_parent,
        };
        walk_children(
            node,
            projected_parent_of_children,
            nodes_by_id,
            visited,
            policy,
            plans,
        );
    }
}
