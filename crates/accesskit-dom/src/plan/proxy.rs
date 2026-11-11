//! What one AccessKit node becomes in the DOM: element type, ARIA role, name and input policy.

use accesskit::{Action, Node, Role};

use super::{NodesById, Policy};

/// How many levels below a nameless control [`descendant_text`] looks for its value's text.
const DESCENDANT_TEXT_MAX_DEPTH: u8 = 3;

/// How one AccessKit node is rendered as DOM.
pub(crate) struct Proxy<'a> {
    /// The HTML element to create, such as `"button"` or `"div"`.
    pub tag: &'static str,
    /// The ARIA `role` attribute; `None` keeps the element's implicit role.
    pub aria_role: Option<&'static str>,
    pub name: &'a str,
    /// Focusable and activatable, as opposed to a container or a run of text.
    pub is_interactive: bool,
    /// Whether the name is written as the element's text rather than its `aria-label`.
    pub name_as_text: bool,
    /// Whether the proxy takes pointer input; see [`crate::Options::region_area_fraction`].
    pub is_hit_testable: bool,
    /// Node whose text names this one, filled in by the label passes in `labels`.
    pub labelled_by: Option<u64>,
}

/// What a proxy is for, which decides how it is named and whether it takes input.
enum ProxyKind {
    Control,
    Region,
    Text,
}

impl<'a> Proxy<'a> {
    /// Plans the proxy for `node`, or `None` when the node is not projected at all.
    pub(super) fn for_node(
        node: &'a Node,
        nodes_by_id: &NodesById<'a>,
        policy: &Policy,
    ) -> Option<Self> {
        if node.is_hidden() {
            return None;
        }
        let (mut tag, mut aria_role, mut kind) = element_for_role(node.role())?;

        // An input's value is not its name: a text field holding "off" is named by the
        // "Lowpass [Hz]:" beside it.
        // For a combo box or a label, though, the value is what identifies it.
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

        // A clickable custom-painted or generic area is promoted to a control.
        // This must happen before name resolution, which treats controls differently.
        if matches!(kind, ProxyKind::Region | ProxyKind::Text)
            && aria_role.is_none()
            && node.supports_action(Action::Click)
        {
            tag = "button";
            kind = ProxyKind::Control;
        }

        // A container never borrows a name from its children, which are projected too and
        // would announce the same words again.
        let name = match kind {
            ProxyKind::Region => node.label().unwrap_or_default(),
            ProxyKind::Text => node_text(node).unwrap_or_default(),
            ProxyKind::Control => node
                .label()
                .or_else(|| named_by_value.then(|| node.value()).flatten())
                // A value-bearing control's only descendant is the run of text holding its
                // value, which names it no better than the value does.
                .or_else(|| {
                    named_by_value
                        .then(|| descendant_text(node, nodes_by_id, DESCENDANT_TEXT_MAX_DEPTH))
                        .flatten()
                })
                // Anything still nameless is left to the label passes, which see the whole
                // tree rather than just this subtree.
                .unwrap_or_default(),
        };

        // Too big to be a control; see `Options::region_area_fraction`.
        let area_points = node.bounds().map_or(0.0, |bounds| {
            (bounds.x1 - bounds.x0).max(0.0) * (bounds.y1 - bounds.y0).max(0.0)
        });
        let is_region_sized =
            policy.region_area_points > 0.0 && area_points > policy.region_area_points;
        if matches!(kind, ProxyKind::Control) && is_region_sized {
            tag = "div";
            aria_role = aria_role.or(Some("group"));
            kind = ProxyKind::Region;
        }

        let is_interactive = matches!(kind, ProxyKind::Control);
        Some(Self {
            tag,
            aria_role,
            name,
            is_interactive,
            // Only a leaf carries text, because `textContent` replaces an element's children.
            // A clickable label promoted to a button keeps its text, so neighbouring
            // controls can still borrow it as their name.
            name_as_text: matches!(kind, ProxyKind::Text) || node.role() == Role::Label,
            is_hit_testable: is_interactive && policy.pointer_proxies,
            labelled_by: None,
        })
    }
}

/// The element tag, ARIA role and kind for an AccessKit role; `None` skips the node.
fn element_for_role(role: Role) -> Option<(&'static str, Option<&'static str>, ProxyKind)> {
    Some(match role {
        // A text run is the content of the label above it; projecting both would say
        // everything twice.
        Role::TextRun => return None,

        // Controls
        Role::Button | Role::ColorWell => ("button", None, ProxyKind::Control),
        Role::CheckBox => ("button", Some("checkbox"), ProxyKind::Control),
        Role::RadioButton => ("button", Some("radio"), ProxyKind::Control),
        Role::ComboBox => ("button", Some("combobox"), ProxyKind::Control),
        Role::Link => ("button", Some("link"), ProxyKind::Control),
        Role::MenuItem => ("button", Some("menuitem"), ProxyKind::Control),
        Role::ListBoxOption => ("button", Some("option"), ProxyKind::Control),
        Role::Tab => ("button", Some("tab"), ProxyKind::Control),
        Role::Slider => ("div", Some("slider"), ProxyKind::Control),
        Role::SpinButton => ("div", Some("spinbutton"), ProxyKind::Control),
        Role::ScrollBar => ("div", Some("scrollbar"), ProxyKind::Control),
        Role::TextInput | Role::PasswordInput | Role::MultilineTextInput => {
            ("div", Some("textbox"), ProxyKind::Control)
        }

        // Regions
        Role::RadioGroup => ("div", Some("radiogroup"), ProxyKind::Region),
        Role::Menu => ("div", Some("menu"), ProxyKind::Region),
        Role::MenuBar => ("div", Some("menubar"), ProxyKind::Region),
        Role::ListBox => ("div", Some("listbox"), ProxyKind::Region),
        Role::Toolbar => ("div", Some("toolbar"), ProxyKind::Region),
        Role::ProgressIndicator => ("div", Some("progressbar"), ProxyKind::Region),
        Role::Image => ("div", Some("img"), ProxyKind::Region),
        Role::Splitter => ("div", Some("separator"), ProxyKind::Region),
        Role::Status => ("div", Some("status"), ProxyKind::Region),
        Role::Table => ("div", Some("table"), ProxyKind::Region),
        Role::Row => ("div", Some("row"), ProxyKind::Region),
        Role::Cell => ("div", Some("cell"), ProxyKind::Region),
        Role::GridCell => ("div", Some("gridcell"), ProxyKind::Region),
        Role::ColumnHeader => ("div", Some("columnheader"), ProxyKind::Region),
        Role::RowHeader => ("div", Some("rowheader"), ProxyKind::Region),
        Role::Group | Role::Pane => ("div", Some("group"), ProxyKind::Region),
        // Not the root, which stands for the overlay: this is a toolkit window.
        Role::Window => ("div", Some("dialog"), ProxyKind::Region),

        // Text
        Role::Label => ("span", None, ProxyKind::Text),
        _ => ("div", None, ProxyKind::Region),
    })
}

/// The text a node displays.
///
/// Toolkits report a label's text as the node's value rather than its label.
fn node_text(node: &Node) -> Option<&str> {
    node.label().or_else(|| node.value())
}

/// The first text found among `node`'s descendants, at most `max_depth` levels down.
fn descendant_text<'a>(
    node: &'a Node,
    nodes_by_id: &NodesById<'a>,
    max_depth: u8,
) -> Option<&'a str> {
    if max_depth == 0 {
        return None;
    }
    node.children().iter().find_map(|child_id| {
        let child = nodes_by_id.get(&child_id.0)?;
        node_text(child).or_else(|| descendant_text(child, nodes_by_id, max_depth - 1))
    })
}
