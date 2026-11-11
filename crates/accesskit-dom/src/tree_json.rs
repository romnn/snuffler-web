//! Debug serialization of the AccessKit tree, compiled in by the `tree-json` feature.
//!
//! The JSON lands on the window global named by [`crate::Options::tree_json_global`], for
//! harnesses that want the raw node data rather than the DOM's lossier rendering of it.

// Writing into a `String` is infallible; the `Result` exists only to satisfy the
// trait, so every `write!` below discards it.
use std::fmt::Write as _;

use accesskit::{Action, Node, NodeId, TreeUpdate};
use wasm_bindgen::JsValue;

use crate::action_name;

/// Publishes tree snapshots on the window global it was built with.
pub(crate) struct Publisher {
    tree_global: String,
    /// `<tree_global>Rev`, counting publishes so a reader can tell a stale snapshot.
    revision_global: String,
    revision: u32,
}

impl Publisher {
    pub fn new(tree_global: String) -> Self {
        Self {
            revision_global: format!("{tree_global}Rev"),
            tree_global,
            revision: 0,
        }
    }

    /// Publishes `update` on the configured global and bumps the revision counter.
    pub fn publish(&mut self, update: &TreeUpdate) {
        let Some(window) = web_sys::window() else {
            return;
        };
        self.revision = self.revision.saturating_add(1);
        let entries = [
            (
                self.tree_global.as_str(),
                JsValue::from_str(&tree_to_json(update)),
            ),
            (
                self.revision_global.as_str(),
                JsValue::from_f64(f64::from(self.revision)),
            ),
        ];
        for (key, value) in entries {
            if let Err(err) = js_sys::Reflect::set(window.as_ref(), &JsValue::from_str(key), &value)
            {
                log::error!("accesskit-dom: failed to publish {key}: {err:?}");
            }
        }
    }
}

// Node ids cross into JS as strings: toolkits such as egui use the full `u64` range, and
// `JSON.parse` would round anything past 2^53 to an id that matches no node.
fn push_node_id(id: NodeId, out: &mut String) {
    out.push('"');
    out.push_str(&id.0.to_string());
    out.push('"');
}

fn tree_to_json(update: &TreeUpdate) -> String {
    let mut out = String::new();
    out.push_str("{\"focus\":");
    push_node_id(update.focus, &mut out);
    if let Some(tree) = update.tree.as_ref() {
        out.push_str(",\"root\":");
        push_node_id(tree.root, &mut out);
    }
    out.push_str(",\"nodes\":[");
    for (index, (id, node)) in update.nodes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_node(*id, node, &mut out);
    }
    out.push_str("]}");
    out
}

fn push_node(id: NodeId, node: &Node, out: &mut String) {
    out.push_str("{\"id\":");
    push_node_id(id, out);
    out.push_str(",\"role\":\"");
    // `Role` has no `Display`, and its `Debug` name is exactly the variant we want.
    let _ = write!(out, "{:?}", node.role());
    out.push('"');

    if let Some(label) = node.label() {
        out.push_str(",\"label\":");
        push_json_string(label, out);
    }
    if let Some(value) = node.value() {
        out.push_str(",\"value\":");
        push_json_string(value, out);
    }
    if let Some(numeric) = node.numeric_value() {
        out.push_str(",\"numericValue\":");
        out.push_str(&numeric.to_string());
    }
    if let Some(toggled) = node.toggled() {
        out.push_str(",\"toggled\":\"");
        let _ = write!(out, "{toggled:?}");
        out.push('"');
    }
    if let Some(bounds) = node.bounds() {
        out.push_str(",\"bounds\":[");
        for (index, coordinate) in [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&coordinate.to_string());
        }
        out.push(']');
    }
    if node.is_disabled() {
        out.push_str(",\"disabled\":true");
    }
    if node.is_hidden() {
        out.push_str(",\"hidden\":true");
    }

    // Supported actions, named from the crate-wide table that `action_from_name` reads.
    let actions: Vec<&str> = [
        Action::Click,
        Action::Focus,
        Action::Increment,
        Action::Decrement,
        Action::SetValue,
        Action::ScrollIntoView,
    ]
    .into_iter()
    .filter(|action| node.supports_action(*action))
    .filter_map(action_name)
    .collect();
    if !actions.is_empty() {
        out.push_str(",\"actions\":[");
        for (index, name) in actions.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_string(name, out);
        }
        out.push(']');
    }

    let children = node.children();
    if !children.is_empty() {
        out.push_str(",\"children\":[");
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_node_id(*child, out);
        }
        out.push(']');
    }
    out.push('}');
}

fn push_json_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
