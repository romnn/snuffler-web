//! Planning: deciding what each AccessKit node becomes, before any DOM is touched.
//!
//! Everything here is pure — it reads a [`TreeUpdate`] and produces [`Plan`]s — so the
//! projection's judgement calls (role mapping, name resolution, label borrowing,
//! region demotion) are unit-tested on the host, without a browser in the loop.
//! The wasm-only [`crate::dom`] module then diffs each plan's signatures against what
//! is mounted and writes only what changed.

use std::hash::Hasher as _;

use accesskit::{Action, Node, Role, TreeUpdate};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

use crate::Options;

/// Where the canvas sits and how many CSS pixels one tree coordinate covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewport {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    /// The canvas backing store's size in device pixels.
    ///
    /// A plain attribute read, so it is what the projection watches to notice a
    /// device-pixel-ratio change without forcing a layout.
    pub backing: (u32, u32),
    pub scale: f64,
}

/// One node's place in the projected tree.
pub(crate) struct Plan<'a> {
    pub id: u64,
    /// Nearest projected ancestor; `None` means the overlay itself.
    pub parent: Option<u64>,
    pub node: &'a Node,
    pub proxy: Proxy<'a>,
}

impl Plan<'_> {
    /// Role, name and state — everything `describe` writes.
    ///
    /// Toolkits resend the whole tree every pass, so this runs for every node on every
    /// frame. Hashing keeps it allocation-free; formatting a string per node here
    /// was on its own a third of the layer's cost.
    pub(crate) fn description_signature(&self) -> u64 {
        use std::hash::Hash as _;

        let mut hasher = FxHasher::default();
        let (node, proxy) = (self.node, &self.proxy);

        proxy.role.hash(&mut hasher);
        proxy.name.hash(&mut hasher);
        proxy.labelled_by.hash(&mut hasher);
        (proxy.interactive, proxy.text_content).hash(&mut hasher);

        node.value().hash(&mut hasher);
        node.description().hash(&mut hasher);
        node.placeholder().hash(&mut hasher);
        (
            node.is_disabled(),
            node.is_read_only(),
            node.is_required(),
            node.is_busy(),
            node.is_expanded(),
            node.is_selected(),
        )
            .hash(&mut hasher);
        (
            node.row_index(),
            node.column_index(),
            node.row_count(),
            node.column_count(),
        )
            .hash(&mut hasher);

        // These enums are not `Hash`, but they are plain discriminants.
        node.toggled().map(|v| v as u8).hash(&mut hasher);
        node.live().map(|v| v as u8).hash(&mut hasher);
        node.orientation().map(|v| v as u8).hash(&mut hasher);

        for value in [
            node.numeric_value(),
            node.min_numeric_value(),
            node.max_numeric_value(),
        ] {
            value.map(f64::to_bits).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Everything that ends up in the element's `style` attribute.
    pub(crate) fn geometry_signature(&self, viewport: &Viewport) -> u64 {
        use std::hash::Hash as _;

        let mut hasher = FxHasher::default();
        self.proxy.hit_testable.hash(&mut hasher);
        [viewport.left, viewport.top, viewport.scale]
            .map(f64::to_bits)
            .hash(&mut hasher);
        if let Some(bounds) = self.node.bounds() {
            [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                .map(f64::to_bits)
                .hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Walk the tree the toolkit sent, deciding what each node becomes.
pub(crate) fn plan<'a>(
    update: &'a TreeUpdate,
    canvas_area_points: f64,
    options: &Options,
) -> Vec<Plan<'a>> {
    let index: FxHashMap<u64, &Node> = update.nodes.iter().map(|(id, node)| (id.0, node)).collect();
    let Some(root) = update.tree.as_ref().map(|tree| tree.root.0) else {
        return Vec::new();
    };
    let Some(root_node) = index.get(&root).copied() else {
        return Vec::new();
    };

    let policy = Policy {
        region_area_points: canvas_area_points * options.region_area_fraction,
        pointer_proxies: options.pointer_proxies,
    };
    let mut out = Vec::with_capacity(update.nodes.len());
    let mut visited = FxHashSet::default();
    visited.insert(root);
    // The root stands for the overlay itself, so its children start at the top.
    walk_children(root_node, None, &index, &mut visited, &policy, &mut out);
    borrow_labels(&mut out, options.label_gap);
    out
}

/// What the projection does with a node, resolved once from [`Options`] so the
/// walk carries decisions rather than the settings they came from.
pub(crate) struct Policy {
    /// Area (in tree coordinates) at which a clickable node becomes a region.
    pub region_area_points: f64,
    /// Whether a control proxy is the browser's hit target; see
    /// [`Options::pointer_proxies`](crate::Options::pointer_proxies).
    pub pointer_proxies: bool,
}

fn walk_children<'a>(
    parent_node: &'a Node,
    projected_parent: Option<u64>,
    index: &FxHashMap<u64, &'a Node>,
    visited: &mut FxHashSet<u64>,
    policy: &Policy,
    out: &mut Vec<Plan<'a>>,
) {
    for child in parent_node.children() {
        let id = child.0;
        if !visited.insert(id) {
            continue;
        }
        let Some(node) = index.get(&id).copied() else {
            continue;
        };
        let proxy = Proxy::for_node(node, index, policy);
        let next_parent = match proxy {
            Some(proxy) => {
                out.push(Plan {
                    id,
                    parent: projected_parent,
                    node,
                    proxy,
                });
                Some(id)
            }
            // A node we do not project must not orphan its subtree: its children are
            // hoisted to the nearest ancestor that does have an element.
            None => projected_parent,
        };
        walk_children(node, next_parent, index, visited, policy, out);
    }
}

/// How one AccessKit node is rendered as DOM.
pub(crate) struct Proxy<'a> {
    pub tag: &'static str,
    pub role: Option<&'static str>,
    pub name: &'a str,
    /// Focusable and activatable, as opposed to a container or a run of text.
    pub interactive: bool,
    /// Whether the name belongs in the element's text rather than its label.
    pub text_content: bool,
    /// Whether the proxy takes pointer input; see [`Options::region_area_fraction`].
    pub hit_testable: bool,
    /// Node whose text names this one, filled in by [`borrow_labels`].
    pub labelled_by: Option<u64>,
}

impl<'a> Proxy<'a> {
    fn for_node(node: &'a Node, index: &FxHashMap<u64, &'a Node>, policy: &Policy) -> Option<Self> {
        let region_area_points = policy.region_area_points;
        if node.is_hidden() {
            return None;
        }
        let (mut tag, mut role, mut kind) = map_role(node.role())?;

        // A control's value is not its name: a text field holding "off" is named by
        // the "Lowpass [Hz]:" beside it, and the value belongs in the content. For a
        // combo box or a label, though, the value *is* what identifies it.
        let named_by_value = !matches!(
            node.role(),
            Role::TextInput
                | Role::PasswordInput
                | Role::MultilineTextInput
                | Role::Slider
                | Role::SpinButton
                | Role::ScrollBar
                | Role::ProgressIndicator
        );
        // Toolkits give their custom-painted and generic areas no role of their own,
        // so a clickable one is promoted to a control rather than left as scenery.
        // This has to happen before the name is resolved, or a promoted control would
        // miss the sibling fallback that is usually the only thing naming it.
        if matches!(kind, Kind::Region | Kind::Text)
            && role.is_none()
            && node.supports_action(Action::Click)
        {
            tag = "button";
            kind = Kind::Control;
        }

        // A container must never borrow a name from what it contains. Its children
        // are projected too, so any text taken from them would be announced once for
        // the container and again for the child that actually owns it — and with
        // deeply nested containers, the same words several times over.
        let name = match kind {
            Kind::Region => node.label().unwrap_or_default(),
            Kind::Text => text_of(node).unwrap_or_default(),
            Kind::Control => node
                .label()
                .or_else(|| named_by_value.then(|| node.value()).flatten())
                // A value-bearing control's only descendant is the run of text
                // holding its value, which names it no better than the value does.
                .or_else(|| {
                    named_by_value
                        .then(|| descendant_text(node, index, 3))
                        .flatten()
                })
                // Anything still nameless is left to `borrow_labels`, which can see
                // the whole tree rather than just this subtree.
                .unwrap_or_default(),
        };

        let area = node
            .bounds()
            .map_or(0.0, |b| (b.x1 - b.x0).max(0.0) * (b.y1 - b.y0).max(0.0));
        let region_sized = region_area_points > 0.0 && area > region_area_points;
        if matches!(kind, Kind::Control) && region_sized {
            // Too big to be a control: publish it for reading, but never let it take
            // presses away from the canvas underneath.
            tag = "div";
            role = role.or(Some("group"));
            kind = Kind::Region;
        }

        let interactive = matches!(kind, Kind::Control);
        Some(Self {
            tag,
            role,
            name,
            interactive,
            // Only a leaf carries text: `textContent` replaces an element's children,
            // so writing it on a container would delete the proxies nested inside it.
            // A label that senses clicks — e.g. one that resets its field when
            // double-clicked — is promoted to a button above, and still owns its text:
            // it is what a neighbouring control has to borrow to get a name at all.
            text_content: matches!(kind, Kind::Text) || node.role() == Role::Label,
            // A control only becomes the browser's hit target where something drives
            // the page by element rather than by coordinate; otherwise the press
            // belongs to the canvas, which is the only thing that can follow a drag.
            hit_testable: interactive && policy.pointer_proxies,
            labelled_by: None,
        })
    }
}

pub(crate) enum Kind {
    Control,
    Region,
    Text,
}

fn map_role(role: Role) -> Option<(&'static str, Option<&'static str>, Kind)> {
    Some(match role {
        // A text run is the content of the label above it; projecting both would say
        // everything twice.
        Role::TextRun => return None,

        Role::Button | Role::ColorWell => ("button", None, Kind::Control),
        Role::CheckBox => ("button", Some("checkbox"), Kind::Control),
        Role::RadioButton => ("button", Some("radio"), Kind::Control),
        Role::ComboBox => ("button", Some("combobox"), Kind::Control),
        Role::Link => ("button", Some("link"), Kind::Control),
        Role::MenuItem => ("button", Some("menuitem"), Kind::Control),
        Role::ListBoxOption => ("button", Some("option"), Kind::Control),
        Role::Tab => ("button", Some("tab"), Kind::Control),
        Role::Slider => ("div", Some("slider"), Kind::Control),
        Role::SpinButton => ("div", Some("spinbutton"), Kind::Control),
        Role::ScrollBar => ("div", Some("scrollbar"), Kind::Control),
        Role::TextInput | Role::PasswordInput | Role::MultilineTextInput => {
            ("div", Some("textbox"), Kind::Control)
        }

        Role::RadioGroup => ("div", Some("radiogroup"), Kind::Region),
        Role::Menu => ("div", Some("menu"), Kind::Region),
        Role::MenuBar => ("div", Some("menubar"), Kind::Region),
        Role::ListBox => ("div", Some("listbox"), Kind::Region),
        Role::Toolbar => ("div", Some("toolbar"), Kind::Region),
        Role::ProgressIndicator => ("div", Some("progressbar"), Kind::Region),
        Role::Image => ("div", Some("img"), Kind::Region),
        Role::Splitter => ("div", Some("separator"), Kind::Region),
        Role::Status => ("div", Some("status"), Kind::Region),
        Role::Table => ("div", Some("table"), Kind::Region),
        Role::Row => ("div", Some("row"), Kind::Region),
        Role::Cell => ("div", Some("cell"), Kind::Region),
        Role::GridCell => ("div", Some("gridcell"), Kind::Region),
        Role::ColumnHeader => ("div", Some("columnheader"), Kind::Region),
        Role::RowHeader => ("div", Some("rowheader"), Kind::Region),
        Role::Group | Role::Pane => ("div", Some("group"), Kind::Region),
        // Not the root, which stands for the overlay: this is a toolkit window.
        Role::Window => ("div", Some("dialog"), Kind::Region),

        Role::Label => ("span", None, Kind::Text),
        _ => ("div", None, Kind::Region),
    })
}

/// Toolkits report a label's text as the node's value rather than its label.
pub(crate) fn text_of(node: &Node) -> Option<&str> {
    node.label().or_else(|| node.value())
}

fn descendant_text<'a>(
    node: &'a Node,
    index: &FxHashMap<u64, &'a Node>,
    depth: u8,
) -> Option<&'a str> {
    if depth == 0 {
        return None;
    }
    node.children().iter().find_map(|child| {
        let child = index.get(&child.0)?;
        text_of(child).or_else(|| descendant_text(child, index, depth - 1))
    })
}

/// Point still-nameless controls at the label painted beside them.
///
/// Toolkits wrap nearly every widget in a container of its own, so the text naming a
/// control is almost never its sibling in the tree — a form row may put the label,
/// the text field and the slider in three separate single-child containers. What
/// does hold is what a sighted user relies on: the label is the text immediately to
/// the left, on the same row. Linking rather than copying keeps the two in step and
/// leaves the screen reader one copy of the words instead of two.
fn borrow_labels(plans: &mut [Plan<'_>], label_gap: f64) {
    let mut borrowed: Vec<(usize, u64)> = Vec::new();
    for (position, item) in plans.iter().enumerate() {
        if !item.proxy.interactive || !item.proxy.name.is_empty() {
            continue;
        }
        let (Some(bounds), Some(earlier)) = (item.node.bounds(), plans.get(..position)) else {
            continue;
        };
        let found = earlier.iter().rev().find_map(|candidate| {
            if !candidate.proxy.text_content || candidate.proxy.name.is_empty() {
                return None;
            }
            let label = candidate.node.bounds()?;
            let same_row = label.y0 < bounds.y1 && bounds.y0 < label.y1;
            (same_row && label.x1 <= bounds.x0 + label_gap).then_some(candidate.id)
        });
        if let Some(label) = found {
            borrowed.push((position, label));
        }
    }
    for (position, label) in borrowed {
        if let Some(item) = plans.get_mut(position) {
            item.proxy.labelled_by = Some(label);
        }
    }
}

#[cfg(test)]
mod tests {
    use accesskit::{NodeId, Rect, Tree, TreeId};
    use color_eyre::eyre::{self, eyre};

    use super::*;

    /// Large enough that no test fixture trips the region-size demotion by accident.
    const CANVAS_AREA: f64 = 1_000_000.0;

    fn update_of(nodes: Vec<(u64, Node)>, root: u64) -> TreeUpdate {
        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes: nodes
                .into_iter()
                .map(|(id, node)| (NodeId(id), node))
                .collect(),
            tree: Some(Tree::new(NodeId(root))),
            focus: NodeId(root),
        }
    }

    fn with_children(role: Role, children: &[u64]) -> Node {
        let mut node = Node::new(role);
        node.set_children(children.iter().copied().map(NodeId).collect::<Vec<_>>());
        node
    }

    fn label_at(text: &str, bounds: Rect) -> Node {
        let mut node = Node::new(Role::Label);
        node.set_label(text);
        node.set_bounds(bounds);
        node
    }

    fn find<'p, 'a>(plans: &'p [Plan<'a>], id: u64) -> Option<&'p Plan<'a>> {
        plans.iter().find(|plan| plan.id == id)
    }

    /// [`find`] for the tests that require the node to have been projected, so a
    /// missing plan surfaces as an error rather than as an unwrap.
    fn plan_for<'p, 'a>(plans: &'p [Plan<'a>], id: u64) -> eyre::Result<&'p Plan<'a>> {
        find(plans, id).ok_or_else(|| eyre!("no plan was projected for node {id}"))
    }

    /// A nameless control is linked to the text immediately to its left on the same
    /// row, the way a sighted user pairs them.
    /// The projection only competes with the canvas for presses where it was asked
    /// to: a control that hit-tests takes the drag the toolkit needs to see.
    #[test]
    fn a_control_hit_tests_only_where_pointer_proxies_are_asked_for() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1]);
        let mut slider = Node::new(Role::Slider);
        slider.set_label("Gain");
        slider.set_bounds(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 120.0,
            y1: 18.0,
        });
        let update = update_of(vec![(0, root), (1, slider)], 0);

        let plans = plan(&update, CANVAS_AREA, &Options::default());
        let proxy = &plan_for(&plans, 1)?.proxy;
        assert!(proxy.interactive, "it is still focusable and activatable");
        assert!(!proxy.hit_testable, "but the press belongs to the canvas");

        let plans = plan(
            &update,
            CANVAS_AREA,
            &Options {
                pointer_proxies: true,
                ..Options::default()
            },
        );
        assert!(plan_for(&plans, 1)?.proxy.hit_testable);
        Ok(())
    }

    #[test]
    fn borrows_the_label_beside_a_control() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1, 2]);
        let label = label_at(
            "Highpass [Hz]:",
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 60.0,
                y1: 20.0,
            },
        );
        let mut field = Node::new(Role::TextInput);
        field.set_bounds(Rect {
            x0: 70.0,
            y0: 0.0,
            x1: 140.0,
            y1: 20.0,
        });

        let update = update_of(vec![(0, root), (1, label), (2, field)], 0);
        let plans = plan(&update, CANVAS_AREA, &Options::default());

        assert_eq!(plan_for(&plans, 2)?.proxy.labelled_by, Some(1));
        Ok(())
    }

    /// Text on a different row never names a control, however close it sits.
    #[test]
    fn does_not_borrow_across_rows() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1, 2]);
        let label = label_at(
            "Highpass [Hz]:",
            Rect {
                x0: 0.0,
                y0: 30.0,
                x1: 60.0,
                y1: 50.0,
            },
        );
        let mut field = Node::new(Role::TextInput);
        field.set_bounds(Rect {
            x0: 70.0,
            y0: 0.0,
            x1: 140.0,
            y1: 20.0,
        });

        let update = update_of(vec![(0, root), (1, label), (2, field)], 0);
        let plans = plan(&update, CANVAS_AREA, &Options::default());

        assert_eq!(plan_for(&plans, 2)?.proxy.labelled_by, None);
        Ok(())
    }

    /// A container stays anonymous and carries no text of its own: its children are
    /// projected too, so any words it took from them would be announced twice.
    #[test]
    fn containers_stay_anonymous() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1]);
        let group = with_children(Role::Group, &[2]);
        let label = label_at(
            "Options",
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 40.0,
                y1: 20.0,
            },
        );

        let update = update_of(vec![(0, root), (1, group), (2, label)], 0);
        let plans = plan(&update, CANVAS_AREA, &Options::default());

        let group = plan_for(&plans, 1)?;
        assert_eq!(group.proxy.name, "");
        assert!(!group.proxy.text_content);
        // The label itself still owns its words, as element text.
        let label = plan_for(&plans, 2)?;
        assert_eq!(label.proxy.name, "Options");
        assert!(label.proxy.text_content);
        Ok(())
    }

    /// Children of a node that is not projected reattach to the nearest projected
    /// ancestor instead of vanishing with their parent.
    #[test]
    fn hoists_children_of_hidden_nodes() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1]);
        let mut pane = with_children(Role::Pane, &[2]);
        pane.set_hidden();
        let mut button = Node::new(Role::Button);
        button.set_label("Ok");

        let update = update_of(vec![(0, root), (1, pane), (2, button)], 0);
        let plans = plan(&update, CANVAS_AREA, &Options::default());

        assert!(find(&plans, 1).is_none());
        // The root stands for the overlay, so the hoisted button lands at top level.
        assert_eq!(plan_for(&plans, 2)?.parent, None);
        Ok(())
    }

    /// A clickable node covering more than the configured share of the canvas is
    /// published for reading but never takes pointer input.
    #[test]
    fn oversized_clickables_become_regions() -> eyre::Result<()> {
        let root = with_children(Role::Window, &[1]);
        let mut waveform = Node::new(Role::Button);
        waveform.set_label("waveform");
        waveform.add_action(Action::Click);
        waveform.set_bounds(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 1000.0,
            y1: 600.0,
        });

        let update = update_of(vec![(0, root), (1, waveform)], 0);
        let plans = plan(&update, CANVAS_AREA, &Options::default());

        let region = plan_for(&plans, 1)?;
        assert!(!region.proxy.hit_testable);
        assert!(!region.proxy.interactive);
        // Even where proxies are allowed to hit-test, a region never does.
        let opened = Options {
            pointer_proxies: true,
            ..Options::default()
        };
        let plans = plan(&update, CANVAS_AREA, &opened);
        assert!(!plan_for(&plans, 1)?.proxy.hit_testable);
        // Demotion also swaps the element type: a `<button>` would stay focusable
        // and clickable no matter which ARIA attributes are written on it.
        assert_eq!(region.proxy.role, Some("group"));
        assert_eq!(region.proxy.tag, "div");
        Ok(())
    }

    /// A value change re-describes a proxy without re-placing it, and a move does
    /// the reverse — the split that keeps a slider drag from rewriting geometry.
    #[test]
    fn signatures_separate_description_from_geometry() -> eyre::Result<()> {
        let viewport = Viewport {
            left: 0.0,
            top: 0.0,
            width: 800.0,
            height: 600.0,
            backing: (800, 600),
            scale: 1.0,
        };
        let slider = |value: f64, x0: f64| {
            let mut node = Node::new(Role::Slider);
            node.set_numeric_value(value);
            node.set_bounds(Rect {
                x0,
                y0: 0.0,
                x1: x0 + 100.0,
                y1: 20.0,
            });
            node
        };
        let signatures = |value: f64, x0: f64| {
            let root = with_children(Role::Window, &[1]);
            let update = update_of(vec![(0, root), (1, slider(value, x0))], 0);
            let plans = plan(&update, CANVAS_AREA, &Options::default());
            let plan = plan_for(&plans, 1)?;
            eyre::Ok((
                plan.description_signature(),
                plan.geometry_signature(&viewport),
            ))
        };

        let (description, geometry) = signatures(1.0, 0.0)?;
        let (moved_description, moved_geometry) = signatures(1.0, 50.0)?;
        let (valued_description, valued_geometry) = signatures(2.0, 0.0)?;

        assert_eq!(description, moved_description);
        assert_ne!(geometry, moved_geometry);
        assert_ne!(description, valued_description);
        assert_eq!(geometry, valued_geometry);
        Ok(())
    }
}
