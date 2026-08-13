//! Projection of an [AccessKit](https://accesskit.dev) tree into live DOM elements.
//!
//! AccessKit ships adapters for the native accessibility APIs (UI Automation,
//! `NSAccessibility`, AT-SPI), but none for the web, where there is no separate API to
//! adapt to: the browser's accessibility tree *is* the DOM.
//! A canvas-rendered UI is one opaque rectangle to a screen reader and to browser
//! automation alike, no matter how rich a tree its toolkit builds.
//!
//! [`Adapter`] closes that gap.
//! Feed it the toolkit's [`TreeUpdate`]s and it mirrors every node into a transparent
//! proxy element over the canvas — carrying the ARIA role, name, state and geometry of
//! the widget it stands for, nested to match the source hierarchy — and it diffs
//! internally, so a pass in which nothing changed writes nothing to the DOM.
//! Input runs the other way through the [`Handler`] the caller supplies: activating or
//! focusing a proxy comes back as an AccessKit [`ActionRequest`], and keys pressed
//! while a proxy has focus come back as [`KeyEvent`]s, because the canvas the toolkit
//! listens on does not have DOM focus at that moment.
//!
//! The crate is toolkit-agnostic on purpose: nothing here depends on the UI library
//! producing the updates.
//! A toolkit binding (such as the `egui-a11y` sibling crate) implements [`Handler`],
//! translates the requests into its own event types, and owns how the adapter is wired
//! into the frame loop.
//! Customization of *semantics* belongs in the tree itself — enrich the nodes at the
//! source and every consumer benefits, native adapters included — while [`Options`]
//! only tunes what genuinely cannot live there: DOM placement policy.
//!
//! # Features
//!
//! - `metrics`: per-pass timing and DOM write counters, exposed via [`Adapter::stats`].
//! - `tree-json`: publish each changed tree as JSON on a window global, for test
//!   harnesses that want raw node data rather than the DOM's rendering of it.

use accesskit::Action;

// The planning half is pure and unit-tested on the host, but outside of tests its
// only consumer is the wasm-only DOM projection.
#[cfg(any(target_arch = "wasm32", test))]
mod tree;

#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(target_arch = "wasm32")]
mod metrics;
#[cfg(all(target_arch = "wasm32", feature = "tree-json"))]
mod tree_json;

#[cfg(target_arch = "wasm32")]
pub use dom::Adapter;
#[cfg(all(target_arch = "wasm32", feature = "metrics"))]
pub use metrics::Stats;

/// Placement policy for the projection.
///
/// Everything semantic — roles, names, states, hierarchy — comes from the tree and is
/// deliberately not configurable here: the tree is the single source of truth, and a
/// producer that wants richer output should enrich its nodes instead, which also
/// improves what native adapters announce.
/// These options only cover what a tree cannot express: where the proxies live in the
/// page and when a node is too large to safely take pointer input.
#[derive(Debug, Clone)]
pub struct Options {
    /// The `id` of the container element holding every proxy.
    ///
    /// Also used as the prefix of each proxy's own `id`, so two adapters on one page
    /// stay disjoint.
    /// Must be usable inside a CSS id selector.
    pub overlay_id: String,

    /// `z-index` of the overlay container.
    ///
    /// The container creates the stacking context all proxies live in, so this is the
    /// single knob deciding what page chrome (loading screens, dialogs) stays above
    /// the projection.
    pub z_index: i32,

    /// Above this share of the canvas area, a clickable node is treated as a region.
    ///
    /// Toolkits report drag-sensing areas — a plot, a panel background — as clickable,
    /// and a proxy that large would swallow every press over it.
    /// Direct manipulation is exactly what a click proxy cannot stand in for, so such
    /// nodes are published for reading but never take pointer input.
    pub region_area_fraction: f64,

    /// How far left of a control its label may sit, in tree coordinates.
    ///
    /// Used when linking a nameless control to the text beside it; see the projection
    /// docs on label borrowing.
    pub label_gap: f64,

    /// Window global to publish the tree JSON on, e.g. `"__myAppA11y"`.
    ///
    /// `None` publishes nothing.
    /// A second global named `<global>Rev` counts republishes, so a reader can tell a
    /// stale snapshot from a live one.
    #[cfg(feature = "tree-json")]
    pub tree_json_global: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            overlay_id: "accesskit-dom".to_owned(),
            z_index: 5,
            region_area_fraction: 0.25,
            label_gap: 16.0,
            #[cfg(feature = "tree-json")]
            tree_json_global: None,
        }
    }
}

/// A keyboard event delivered to a focused proxy.
///
/// Mirrors the fields of the DOM `KeyboardEvent` the projection received, decoupled
/// from `web-sys` so a [`Handler`] implementation stays testable off the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// The DOM `KeyboardEvent.key` value: the produced character for printable keys,
    /// a name like `"Enter"` or `"ArrowUp"` otherwise.
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
/// Implemented by the toolkit binding; every method is called from a DOM event
/// listener, never re-entrantly from [`Adapter::update`].
pub trait Handler {
    /// An action a proxy requested on the toolkit's behalf: a click, a focus move.
    ///
    /// Requests target [`accesskit::TreeId::ROOT`], matching toolkits that publish a
    /// single tree.
    fn action_request(&mut self, request: accesskit::ActionRequest);

    /// A key went down (`pressed`) or up on a focused proxy.
    ///
    /// Returns whether the toolkit wants the key.
    /// `true` suppresses the browser's own default handling, so a control activated
    /// through the toolkit is not activated a second time by a synthetic DOM click.
    fn key_event(&mut self, event: &KeyEvent, pressed: bool) -> bool;

    /// DOM focus entered (`true`) or left (`false`) the overlay.
    ///
    /// While a proxy holds focus, the canvas necessarily does not, and a toolkit that
    /// derives "window focused" from the canvas needs this to know better.
    fn focus_changed(&mut self, focus_within: bool);
}

/// Why an [`Adapter`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The canvas is not attached to a document.
    Detached,
    /// The canvas's document has no `<body>` to append the overlay to.
    NoBody,
    /// A DOM call failed; the payload is the browser's error, formatted.
    Dom(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Detached => write!(f, "canvas is not attached to a document"),
            Self::NoBody => write!(f, "document has no body to hold the overlay"),
            Self::Dom(err) => write!(f, "DOM call failed: {err}"),
        }
    }
}

impl std::error::Error for Error {}

/// Every action name the crate speaks, paired with the action it stands for.
///
/// This single table keeps the two string boundaries aligned: the `tree-json` dump
/// advertises a node's supported actions under these names, and
/// [`action_from_name`] turns the same names back into actions.
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
/// The intended caller is a stringly boundary — a wasm export driven by a test
/// harness — that received the name from the published tree.
/// Returns `None` for unknown names and for `"setValue"`: `SetValue` carries a payload
/// no bare name can express, so a request built from a string alone could never apply.
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

    /// Every published name parses back to its action, except `setValue`, which is
    /// deliberately unreachable from a string because it requires a payload.
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
