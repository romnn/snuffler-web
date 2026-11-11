//! Change detection: one hash per kind of DOM write, so an unchanged pass writes nothing.
//!
//! The two signatures are kept apart because they change independently.
//! A dragged slider changes its value and moves nothing, while a resize moves every proxy and
//! renames none.

use std::hash::{Hash as _, Hasher as _};

use rustc_hash::FxHasher;

use super::{Plan, Viewport};

impl Plan<'_> {
    /// Hash of role, name and state: everything `describe` writes.
    ///
    /// Toolkits resend the whole tree every pass, so this runs for every node on every frame
    /// and must not allocate.
    pub(crate) fn description_signature(&self) -> u64 {
        let mut hasher = FxHasher::default();
        let (node, proxy) = (self.node, &self.proxy);

        proxy.aria_role.hash(&mut hasher);
        proxy.name.hash(&mut hasher);
        proxy.labelled_by.hash(&mut hasher);
        (proxy.is_interactive, proxy.name_as_text).hash(&mut hasher);

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
        node.toggled()
            .map(|toggled| toggled as u8)
            .hash(&mut hasher);
        node.live().map(|live| live as u8).hash(&mut hasher);
        node.orientation()
            .map(|orientation| orientation as u8)
            .hash(&mut hasher);

        for value in [
            node.numeric_value(),
            node.min_numeric_value(),
            node.max_numeric_value(),
        ] {
            value.map(f64::to_bits).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Hash of everything that ends up in the element's `style` attribute: what `place` writes.
    pub(crate) fn geometry_signature(&self, viewport: &Viewport) -> u64 {
        let mut hasher = FxHasher::default();
        self.proxy.is_hit_testable.hash(&mut hasher);
        [
            viewport.left_css_px,
            viewport.top_css_px,
            viewport.css_px_per_point,
        ]
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
