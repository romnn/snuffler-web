use accesskit::{Action, Node, NodeId, Rect, Role, Tree, TreeId, TreeUpdate};
use color_eyre::eyre::{self, eyre};

use super::{Plan, Viewport, plan_tree};
use crate::Options;

/// Large enough that no test fixture trips the region-size demotion by accident.
const CANVAS_AREA_POINTS: f64 = 1_000_000.0;

fn update_of(nodes: Vec<(u64, Node)>, root_id: u64) -> TreeUpdate {
    TreeUpdate {
        tree_id: TreeId::ROOT,
        nodes: nodes
            .into_iter()
            .map(|(id, node)| (NodeId(id), node))
            .collect(),
        tree: Some(Tree::new(NodeId(root_id))),
        focus: NodeId(root_id),
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

fn find<'p, 'a>(plans: &'p [Plan<'a>], node_id: u64) -> Option<&'p Plan<'a>> {
    plans.iter().find(|plan| plan.node_id == node_id)
}

/// [`find`] for nodes that must have been projected.
fn plan_for<'p, 'a>(plans: &'p [Plan<'a>], node_id: u64) -> eyre::Result<&'p Plan<'a>> {
    find(plans, node_id).ok_or_else(|| eyre!("no plan was projected for node {node_id}"))
}

/// A control only takes presses from the canvas where pointer proxies were asked for.
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

    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());
    let proxy = &plan_for(&plans, 1)?.proxy;
    assert!(
        proxy.is_interactive,
        "it is still focusable and activatable"
    );
    assert!(
        !proxy.is_hit_testable,
        "but the press belongs to the canvas"
    );

    let plans = plan_tree(
        &update,
        CANVAS_AREA_POINTS,
        &Options {
            pointer_proxies: true,
            ..Options::default()
        },
    );
    assert!(plan_for(&plans, 1)?.proxy.is_hit_testable);
    Ok(())
}

/// The toolkit's own `LabelledBy` relation names the control even when the control carries a
/// label of its own.
/// A combo box's AccessKit label is its selected value, and "viridis" is not this control's
/// name.
#[test]
fn an_explicit_labelled_by_relation_outranks_the_controls_own_label() -> eyre::Result<()> {
    let root = with_children(Role::Window, &[1, 2]);
    let mut combo = Node::new(Role::ComboBox);
    combo.set_label("viridis");
    combo.set_bounds(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 90.0,
        y1: 20.0,
    });
    combo.push_labelled_by(NodeId(2));
    // The caption sits to the *right*, where egui's `from_label` puts it: outside the
    // geometric borrow's reach on purpose.
    let caption = label_at(
        "Colormap",
        Rect {
            x0: 100.0,
            y0: 0.0,
            x1: 160.0,
            y1: 20.0,
        },
    );

    let update = update_of(vec![(0, root), (1, combo), (2, caption)], 0);
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

    assert_eq!(plan_for(&plans, 1)?.proxy.labelled_by, Some(2));
    Ok(())
}

/// A nameless control is linked to the text immediately to its left on the same row.
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
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

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
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

    assert_eq!(plan_for(&plans, 2)?.proxy.labelled_by, None);
    Ok(())
}

/// A container stays anonymous and carries no text of its own.
/// Its children are projected too, so any words it took from them would be announced twice.
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
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

    let group = plan_for(&plans, 1)?;
    assert_eq!(group.proxy.name, "");
    assert!(!group.proxy.name_as_text);

    // The label itself still owns its words, as element text.
    let label = plan_for(&plans, 2)?;
    assert_eq!(label.proxy.name, "Options");
    assert!(label.proxy.name_as_text);
    Ok(())
}

/// Children of a node that is not projected reattach to the nearest projected ancestor instead
/// of vanishing with their parent.
#[test]
fn hoists_children_of_hidden_nodes() -> eyre::Result<()> {
    let root = with_children(Role::Window, &[1]);
    let mut pane = with_children(Role::Pane, &[2]);
    pane.set_hidden();
    let mut button = Node::new(Role::Button);
    button.set_label("Ok");

    let update = update_of(vec![(0, root), (1, pane), (2, button)], 0);
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

    assert!(find(&plans, 1).is_none());
    // The root stands for the overlay, so the hoisted button lands at top level.
    assert_eq!(plan_for(&plans, 2)?.projected_parent, None);
    Ok(())
}

/// A clickable node covering more than the configured share of the canvas is published for
/// reading but never takes pointer input.
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
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());

    let region = plan_for(&plans, 1)?;
    assert!(!region.proxy.is_hit_testable);
    assert!(!region.proxy.is_interactive);

    // Even where proxies are allowed to hit-test, a region never does.
    let with_pointer_proxies = Options {
        pointer_proxies: true,
        ..Options::default()
    };
    let plans = plan_tree(&update, CANVAS_AREA_POINTS, &with_pointer_proxies);
    assert!(!plan_for(&plans, 1)?.proxy.is_hit_testable);

    // Demotion also swaps the element type: a `<button>` would stay focusable and clickable
    // no matter which ARIA attributes are written on it.
    assert_eq!(region.proxy.aria_role, Some("group"));
    assert_eq!(region.proxy.tag, "div");
    Ok(())
}

/// A value change re-describes a proxy without re-placing it, and a move does the reverse.
/// That split keeps a slider drag from rewriting geometry.
#[test]
fn signatures_separate_description_from_geometry() -> eyre::Result<()> {
    let viewport = Viewport {
        left_css_px: 0.0,
        top_css_px: 0.0,
        width_css_px: 800.0,
        height_css_px: 600.0,
        backing_width_device_px: 800,
        backing_height_device_px: 600,
        css_px_per_point: 1.0,
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
        let plans = plan_tree(&update, CANVAS_AREA_POINTS, &Options::default());
        let plan = plan_for(&plans, 1)?;
        eyre::Ok((
            plan.description_signature(),
            plan.geometry_signature(&viewport),
        ))
    };

    let (description, geometry) = signatures(1.0, 0.0)?;
    let (moved_description, moved_geometry) = signatures(1.0, 50.0)?;
    let (revalued_description, revalued_geometry) = signatures(2.0, 0.0)?;

    // Moving changes only the geometry
    assert_eq!(description, moved_description);
    assert_ne!(geometry, moved_geometry);

    // Changing the value changes only the description
    assert_ne!(description, revalued_description);
    assert_eq!(geometry, revalued_geometry);
    Ok(())
}
