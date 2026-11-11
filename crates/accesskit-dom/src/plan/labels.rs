//! Label passes: pointing nameless controls at the text that names them.
//!
//! Both passes link with `aria-labelledby` rather than copying the words, which leaves the
//! screen reader one copy of them.

use rustc_hash::FxHashSet;

use super::Plan;

/// Points controls at the label the toolkit named with a `LabelledBy` relation.
///
/// The explicit relation outranks the geometric guess in [`borrow_adjacent_labels`].
/// It matters most for combo boxes, whose own label is the selected value.
pub(super) fn link_explicit_labels(plans: &mut [Plan<'_>]) {
    let planned_ids: FxHashSet<u64> = plans.iter().map(|plan| plan.node_id).collect();
    for plan in plans.iter_mut() {
        if !plan.proxy.is_interactive {
            continue;
        }
        // An `aria-labelledby` naming an unprojected element names nothing.
        let label_id = plan
            .node
            .labelled_by()
            .iter()
            .map(|id| id.0)
            .find(|id| planned_ids.contains(id));
        if let Some(label_id) = label_id {
            plan.proxy.labelled_by = Some(label_id);
        }
    }
}

/// Points still-nameless controls at the text immediately to their left on the same row.
///
/// Toolkits wrap nearly every widget in its own container, so the naming text is rarely a
/// tree sibling; geometry is what a sighted user relies on instead.
/// The text must end left of the control's left edge, give or take `label_gap_points` of
/// overlap.
pub(super) fn borrow_adjacent_labels(plans: &mut [Plan<'_>], label_gap_points: f64) {
    // Collect first, then write: the search reads earlier plans while a write needs `&mut`.
    let mut borrowed: Vec<BorrowedLabel> = Vec::new();
    for (position, plan) in plans.iter().enumerate() {
        if !plan.proxy.is_interactive
            || !plan.proxy.name.is_empty()
            || plan.proxy.labelled_by.is_some()
        {
            continue;
        }
        let (Some(control), Some(earlier)) = (plan.node.bounds(), plans.get(..position)) else {
            continue;
        };
        let label_id = earlier.iter().rev().find_map(|candidate| {
            if !candidate.proxy.name_as_text || candidate.proxy.name.is_empty() {
                return None;
            }
            let label = candidate.node.bounds()?;
            let same_row = label.y0 < control.y1 && control.y0 < label.y1;
            (same_row && label.x1 <= control.x0 + label_gap_points).then_some(candidate.node_id)
        });
        if let Some(label_id) = label_id {
            borrowed.push(BorrowedLabel {
                control_position: position,
                label_id,
            });
        }
    }

    for BorrowedLabel {
        control_position,
        label_id,
    } in borrowed
    {
        if let Some(plan) = plans.get_mut(control_position) {
            plan.proxy.labelled_by = Some(label_id);
        }
    }
}

/// A control, by its position in the plans, and the node whose text names it.
struct BorrowedLabel {
    control_position: usize,
    label_id: u64,
}
