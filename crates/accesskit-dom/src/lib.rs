//! Projection of an [AccessKit](https://accesskit.dev) tree into live DOM elements.
//!
//! AccessKit has adapters for the native accessibility APIs but none for the web, where the
//! browser's accessibility tree *is* the DOM.
//! [`Adapter`] mirrors every node of a toolkit's [`TreeUpdate`]s into a transparent proxy
//! element over the canvas, carrying the node's ARIA role, name, state and geometry, and diffs
//! internally so an unchanged pass writes nothing.
//! Input on the proxies comes back through the caller's [`Handler`] as AccessKit
//! [`ActionRequest`]s and [`KeyEvent`]s.
//!
//! The crate is toolkit-agnostic: a binding such as `egui-a11y` implements [`Handler`] and
//! wires the adapter into its frame loop.
//!
//! [`TreeUpdate`]: accesskit::TreeUpdate
//! [`ActionRequest`]: accesskit::ActionRequest
//!
//! # Features
//!
//! - `metrics`: per-pass timing and DOM write counters, exposed via `Adapter::stats`.
//! - `tree-json`: publish each changed tree as JSON on a window global, for test harnesses
//!   that want the raw node data.

use accesskit::Action;

// The planning half is pure and unit-tested on the host, but outside of tests its only
// consumer is the wasm-only DOM projection.
#[cfg(any(target_arch = "wasm32", test))]
mod plan;

#[cfg(target_arch = "wasm32")]
mod adapter;
#[cfg(target_arch = "wasm32")]
mod metrics;
#[cfg(all(target_arch = "wasm32", feature = "tree-json"))]
mod tree_json;

#[cfg(target_arch = "wasm32")]
pub use adapter::Adapter;
#[cfg(all(target_arch = "wasm32", feature = "metrics"))]
pub use metrics::Stats;

/// Placement policy for the projection.
///
/// Semantics (roles, names, states, hierarchy) come only from the tree; a producer that wants
/// richer output enriches its nodes.
/// These options cover what a tree cannot express.
#[derive(Debug, Clone)]
pub struct Options {
    /// The `id` of the overlay element holding every proxy.
    ///
    /// Also prefixes each proxy's own `id`, so two adapters on one page stay disjoint.
    /// Must be usable inside a CSS id selector.
    pub overlay_id: String,

    /// `z-index` of the overlay, which decides what page chrome stays above every proxy.
    pub z_index: i32,

    /// Above this share of the canvas area, a clickable node is treated as a region.
    ///
    /// Toolkits report drag-sensing areas such as a plot as clickable, and a proxy that large
    /// would swallow every press over it, so such nodes are published for reading but never
    /// take pointer input.
    pub region_area_fraction: f64,

    /// Whether control proxies are the browser's hit target for pointer input.
    ///
    /// A hit-testing proxy turns a `click` into [`Action::Click`] but swallows drags and
    /// hovers meant for the canvas, so this is off by default.
    /// Turn it on for automation: `WebDriver` and Playwright check that the resolved element
    /// is topmost at its centre before dispatching.
    /// Screen readers need it off either way, since they activate through focus, keys and
    /// synthetic clicks.
    pub pointer_proxies: bool,

    /// How far the text naming a nameless control may overlap the control's left edge, in
    /// tree coordinates.
    ///
    /// Text further right never names it, so a control does not take its right-hand
    /// neighbour's caption.
    pub label_gap_points: f64,

    /// Window global to publish the tree JSON on, e.g. `"__myAppA11y"`; `None` publishes
    /// nothing.
    ///
    /// A second global named `<global>Rev` counts republishes, so a reader can tell a stale
    /// snapshot from a live one.
    #[cfg(feature = "tree-json")]
    pub tree_json_global: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            overlay_id: "accesskit-dom".to_owned(),
            z_index: 5,
            region_area_fraction: 0.25,
            pointer_proxies: false,
            label_gap_points: 16.0,
            #[cfg(feature = "tree-json")]
            tree_json_global: None,
        }
    }
}

/// A keyboard event delivered to a focused proxy.
///
/// Mirrors the DOM `KeyboardEvent`, decoupled from `web-sys` so a [`Handler`] stays testable
/// off the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// The DOM `KeyboardEvent.key` value: the produced character for printable keys, a name
    /// like `"Enter"` or `"ArrowUp"` otherwise.
    pub key: String,
    /// Whether the event is an auto-repeat of a held key.
    pub repeat: bool,
    /// Whether Alt (Option on mac) was held.
    pub alt: bool,
    /// Whether Control was held.
    pub ctrl: bool,
    /// Whether Shift was held.
    pub shift: bool,
    /// Whether the Meta key (Command on mac) was held.
    pub meta: bool,
}

/// What the projection reports back to the toolkit driving it.
///
/// Every method is called from a DOM event listener, never re-entrantly from
/// `Adapter::update`.
pub trait Handler {
    /// An action a proxy requested, targeting [`accesskit::TreeId::ROOT`].
    fn action_request(&mut self, request: accesskit::ActionRequest);

    /// A key went down (`pressed`) or up on a focused proxy; returns whether the toolkit
    /// wants it.
    ///
    /// `true` suppresses the browser's default handling, so a control activated through the
    /// toolkit is not activated again by a synthetic DOM click.
    /// Keys reach the proxies rather than the canvas because the canvas has no DOM focus
    /// while a proxy does.
    fn key_event(&mut self, event: &KeyEvent, pressed: bool) -> bool;

    /// DOM focus entered (`true`) or left (`false`) the overlay.
    ///
    /// While a proxy holds focus the canvas does not, which a toolkit deriving "window
    /// focused" from the canvas needs to know.
    fn focus_changed(&mut self, focus_within: bool);
}

/// Why an `Adapter` could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The canvas is not attached to a document.
    #[error("canvas is not attached to a document")]
    Detached,
    /// The canvas's document has no `<body>` to append the overlay to.
    #[error("document has no body to hold the overlay")]
    NoBody,
    /// A DOM call failed; the payload is the browser's error, formatted.
    #[error("DOM call failed: {0}")]
    Dom(String),
}

/// Every action name the crate speaks, so the `tree-json` dump and [`action_from_name`] agree.
const ACTION_NAMES: &[(Action, &str)] = &[
    (Action::Click, "click"),
    (Action::Focus, "focus"),
    (Action::Blur, "blur"),
    (Action::Increment, "increment"),
    (Action::Decrement, "decrement"),
    (Action::Expand, "expand"),
    (Action::Collapse, "collapse"),
    (Action::SetValue, "setValue"),
    (Action::ScrollIntoView, "scrollIntoView"),
    (Action::ScrollUp, "scrollUp"),
    (Action::ScrollDown, "scrollDown"),
    (Action::ScrollLeft, "scrollLeft"),
    (Action::ScrollRight, "scrollRight"),
    (Action::ShowContextMenu, "showContextMenu"),
];

/// Parses an action name from the `tree-json` vocabulary back into an [`Action`].
///
/// Returns `None` for unknown names and for `"setValue"`, whose payload a bare name cannot
/// carry.
#[must_use]
pub fn action_from_name(name: &str) -> Option<Action> {
    if name == "setValue" {
        return None;
    }
    ACTION_NAMES
        .iter()
        .find_map(|(action, known)| (*known == name).then_some(*action))
}

/// The `tree-json` name of `action`, if it is one this crate publishes.
#[must_use]
pub fn action_name(action: Action) -> Option<&'static str> {
    ACTION_NAMES
        .iter()
        .find_map(|(known, name)| (*known == action).then_some(*name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every published name parses back to its action, except `setValue`, which requires a
    /// payload.
    #[test]
    fn action_names_round_trip() {
        for (action, name) in ACTION_NAMES {
            if *name == "setValue" {
                assert_eq!(action_from_name(name), None);
            } else {
                assert_eq!(action_from_name(name), Some(*action));
            }
            assert_eq!(action_name(*action), Some(*name));
        }
    }
}
