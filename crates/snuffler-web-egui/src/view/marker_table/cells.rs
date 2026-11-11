//! Click-to-edit table cells.

use snuffler_web_core::markers::{MarkerColumn, MarkerId};

/// The one cell being edited inline: which column of which marker, and the text
/// typed so far.
pub(super) struct CellEdit {
    pub(super) column: MarkerColumn,
    pub(super) marker: MarkerId,
    pub(super) text: String,
}

impl CellEdit {
    fn is_at(&self, column: MarkerColumn, marker: MarkerId) -> bool {
        self.column == column && self.marker == marker
    }
}

/// A numeric cell that becomes an inline text editor on click.
///
/// It commits a valid parse on Enter or blur; an invalid parse is discarded (Qt's
/// `toFloat` validity check).
/// Non-editable cells render as a plain label.
/// Returns whether a commit wrote the value this frame.
pub(super) fn number_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<CellEdit>,
    marker: MarkerId,
    column: MarkerColumn,
    value: &mut Option<f64>,
    decimals: usize,
    editable: bool,
) -> bool {
    let shown = value.map_or_else(|| "—".to_owned(), |number| format!("{number:.decimals$}"));
    if !editable {
        ui.label(shown);
        return false;
    }
    match edit {
        Some(cell) if cell.is_at(column, marker) => {
            let response = ui.add(egui::TextEdit::singleline(&mut cell.text).desired_width(56.0));
            if response.lost_focus() {
                let committed = cell.text.trim().parse::<f64>();
                if let Ok(number) = committed {
                    *value = Some(number);
                }
                *edit = None;
                return committed.is_ok();
            }
            response.request_focus();
        }
        _ => {
            if click_to_edit_label(ui, shown) {
                *edit = Some(CellEdit {
                    column,
                    marker,
                    text: value.map_or(String::new(), |number| format!("{number:.decimals$}")),
                });
            }
        }
    }
    false
}

/// A text cell showing `shown`, with the same click-to-edit, commit-on-blur
/// behaviour as [`number_cell`]; an empty commit clears the value.
///
/// Returns whether a commit wrote the value this frame.
pub(super) fn text_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<CellEdit>,
    marker: MarkerId,
    column: MarkerColumn,
    value: &mut Option<String>,
    shown: String,
) -> bool {
    match edit {
        Some(cell) if cell.is_at(column, marker) => {
            let response = ui.add(egui::TextEdit::singleline(&mut cell.text).desired_width(110.0));
            if response.lost_focus() {
                let trimmed = cell.text.trim();
                *value = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                *edit = None;
                return true;
            }
            response.request_focus();
        }
        _ => {
            if click_to_edit_label(ui, shown) {
                *edit = Some(CellEdit {
                    column,
                    marker,
                    text: value.clone().unwrap_or_default(),
                });
            }
        }
    }
    false
}

/// A label that starts an edit when clicked; returns whether it was.
fn click_to_edit_label(ui: &mut egui::Ui, shown: String) -> bool {
    ui.add(egui::Label::new(shown).sense(egui::Sense::click()))
        .on_hover_text("click to edit")
        .clicked()
}
