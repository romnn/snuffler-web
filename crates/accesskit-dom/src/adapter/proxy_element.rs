//! Writing a [`Plan`] onto its proxy element: the ARIA description and the placement.

use accesskit::{Live, Node, Orientation, Role, Toggled};
use web_sys::HtmlElement;

use super::dom::{remove_attribute, set_attribute};
use crate::plan::{Plan, Viewport};

/// Attribute carrying the AccessKit node id on each proxy, for event routing and for harnesses
/// that want to correlate the DOM with the published tree.
pub(super) const NODE_ID_ATTRIBUTE: &str = "data-ak";

/// Styles every proxy shares: invisible, but still in the accessibility tree.
const BASE_STYLE: &str = "position:fixed;margin:0;padding:0;border:0;background:transparent;\
                          color:transparent;font:inherit;overflow:hidden;white-space:nowrap;\
                          opacity:0;";

/// The proxy's DOM `id`, prefixed by the overlay's so two adapters on one page stay disjoint.
fn proxy_element_id(overlay_id: &str, node_id: u64) -> String {
    format!("{overlay_id}-{node_id}")
}

/// Writes the proxy's whole appearance and position as one `style` attribute.
///
/// One attribute write replaces sixteen `setProperty` calls, each a separate crossing into JS.
pub(super) fn place(element: &HtmlElement, plan: &Plan<'_>, viewport: &Viewport) {
    let pointer_events = if plan.proxy.is_hit_testable {
        "pointer-events:auto;"
    } else {
        "pointer-events:none;"
    };

    let Some(bounds) = plan.node.bounds() else {
        // Keep a boundless node in the tree but sized to nothing, not parked over the origin.
        set_attribute(
            element,
            "style",
            &format!("{BASE_STYLE}{pointer_events}width:0;height:0;"),
        );
        return;
    };

    let scale = viewport.css_px_per_point;
    let left_css_px = viewport.left_css_px + bounds.x0 * scale;
    let top_css_px = viewport.top_css_px + bounds.y0 * scale;
    let width_css_px = ((bounds.x1 - bounds.x0) * scale).max(0.0);
    let height_css_px = ((bounds.y1 - bounds.y0) * scale).max(0.0);
    // Smaller proxies stack above larger ones, because some toolkits send nodes in hash order
    // and paint order alone would decide overlap arbitrarily.
    let z_index = (10_000.0 - width_css_px * height_css_px / 1_000.0).clamp(1.0, 10_000.0);
    set_attribute(
        element,
        "style",
        &format!(
            "{BASE_STYLE}{pointer_events}left:{left_css_px:.1}px;top:{top_css_px:.1}px;\
             width:{width_css_px:.1}px;height:{height_css_px:.1}px;z-index:{z_index:.0};"
        ),
    );
}

/// Writes the proxy's identity, role, name and state.
pub(super) fn describe(element: &HtmlElement, plan: &Plan<'_>, overlay_id: &str) {
    let node = plan.node;
    let proxy = &plan.proxy;

    // Identity and role
    set_attribute(element, NODE_ID_ATTRIBUTE, &plan.node_id.to_string());
    // Needed so `aria-labelledby` has something to point at.
    set_attribute(element, "id", &proxy_element_id(overlay_id, plan.node_id));
    if let Some(aria_role) = proxy.aria_role {
        set_attribute(element, "role", aria_role);
    }
    if proxy.is_interactive {
        // Focus moving here is routed back to the toolkit as an AccessKit focus request.
        set_attribute(element, "tabindex", "0");
    }

    // Name
    if proxy.name_as_text {
        element.set_text_content(Some(proxy.name));
    } else if proxy.name.is_empty() {
        // A nameless container stays anonymous rather than repeating its children's text.
        remove_attribute(element, "aria-label");
    } else {
        set_attribute(element, "aria-label", proxy.name);
    }
    if let Some(label_id) = proxy.labelled_by {
        set_attribute(
            element,
            "aria-labelledby",
            &proxy_element_id(overlay_id, label_id),
        );
    } else {
        remove_attribute(element, "aria-labelledby");
    }

    describe_state(element, node);
    describe_value(element, node, proxy.aria_role);
    describe_grid_position(element, node);
}

fn describe_state(element: &HtmlElement, node: &Node) {
    match node.toggled() {
        Some(Toggled::True) => set_attribute(element, "aria-checked", "true"),
        Some(Toggled::False) => set_attribute(element, "aria-checked", "false"),
        Some(Toggled::Mixed) => set_attribute(element, "aria-checked", "mixed"),
        None => remove_attribute(element, "aria-checked"),
    }
    set_optional_bool_attribute(element, "aria-expanded", node.is_expanded());
    set_optional_bool_attribute(element, "aria-selected", node.is_selected());
    set_flag_attribute(element, "aria-readonly", node.is_read_only());
    set_flag_attribute(element, "aria-required", node.is_required());
    set_flag_attribute(element, "aria-busy", node.is_busy());

    if node.is_disabled() {
        set_attribute(element, "aria-disabled", "true");
        // A disabled `<button>` fires no click, which is what we want: automation should
        // refuse the interaction rather than silently do nothing.
        set_attribute(element, "disabled", "");
    } else {
        remove_attribute(element, "aria-disabled");
        remove_attribute(element, "disabled");
    }

    match node.live() {
        Some(Live::Polite) => set_live_region(element, "polite"),
        Some(Live::Assertive) => set_live_region(element, "assertive"),
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

fn set_live_region(element: &HtmlElement, politeness: &str) {
    set_attribute(element, "aria-live", politeness);
    // Read the whole message rather than only the words that changed.
    set_attribute(element, "aria-atomic", "true");
}

fn describe_value(element: &HtmlElement, node: &Node, aria_role: Option<&str>) {
    if let Some(value) = node.numeric_value() {
        set_attribute(element, "aria-valuenow", &value.to_string());
    } else {
        remove_attribute(element, "aria-valuenow");
    }
    // A range without bounds is nearly useless to a screen reader: "47" alone says nothing
    // about how far along the control sits.
    if let Some(min) = node.min_numeric_value() {
        set_attribute(element, "aria-valuemin", &min.to_string());
    }
    if let Some(max) = node.max_numeric_value() {
        set_attribute(element, "aria-valuemax", &max.to_string());
    }

    if node.role() == Role::PasswordInput {
        // Never publish a password: the projection is a parallel copy of the UI in the DOM,
        // readable by anything else running on the page.
        element.set_text_content(None);
        remove_attribute(element, "aria-valuetext");
    } else if let Some(text) = node.value() {
        if aria_role == Some("textbox") {
            // A textbox is named by its label and read from its contents.
            // Its only children in the source tree are text runs, which are not projected, so
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

fn describe_grid_position(element: &HtmlElement, node: &Node) {
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

/// Writes `"true"` or `"false"`, or removes the attribute when the state is unknown.
fn set_optional_bool_attribute(element: &HtmlElement, name: &str, value: Option<bool>) {
    match value {
        Some(value) => set_attribute(element, name, if value { "true" } else { "false" }),
        None => remove_attribute(element, name),
    }
}

/// Writes `"true"` when set and removes the attribute otherwise, its ARIA default.
fn set_flag_attribute(element: &HtmlElement, name: &str, is_set: bool) {
    if is_set {
        set_attribute(element, name, "true");
    } else {
        remove_attribute(element, name);
    }
}
