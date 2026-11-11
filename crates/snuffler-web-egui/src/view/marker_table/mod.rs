//! The Markers window: a sortable table of every marker, with inline editing.
//!
//! [`row`] draws one marker's cells, [`cells`] holds the click-to-edit cells,
//! [`order`] sorts the rows, and [`file`] writes the markers to a marker file.

mod cells;
mod file;
mod order;
mod row;

use snuffler_web_core::bridge::{Bridge, EventSnapshot};
use snuffler_web_core::markers::{MarkerColumn, MarkerId};
use snuffler_web_core::state::AppState;

use self::cells::CellEdit;
pub use self::file::markers_to_file;
use self::order::{OrderKey, marker_headers};

/// Frontend-local marker-table sorting and inline editing state.
pub struct MarkerTable {
    sort_column: MarkerColumn,
    sort_ascending: bool,
    /// The cell being edited inline, if any.
    edit: Option<CellEdit>,
    /// Every marker, in the table's current sort order.
    order: Vec<MarkerId>,
    /// What [`Self::order`] was sorted for, so it is re-sorted only on change.
    order_key: Option<OrderKey>,
}

impl Default for MarkerTable {
    fn default() -> Self {
        Self {
            sort_column: MarkerColumn::Time,
            sort_ascending: true,
            edit: None,
            order: Vec::new(),
            order_key: None,
        }
    }
}

/// What the table's rows asked for during one draw pass, applied after it.
#[derive(Default)]
struct TableOutput {
    center_time: Option<f64>,
    beachball: Option<[f64; 6]>,
    /// A plain row click replaces the whole selection with this row (Qt table
    /// semantics).
    ///
    /// It is applied after the draw pass because it touches every marker.
    replace_selection: Option<MarkerId>,
    event_updates: Vec<EventSnapshot>,
}

impl MarkerTable {
    /// Draws the marker window and returns a focal mechanism requested by the user.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &mut AppState,
        bridge: &Bridge,
    ) -> Option<[f64; 6]> {
        if !state.show_marker_editor {
            return None;
        }

        // An edit whose marker was deleted meanwhile is dropped.
        if self
            .edit
            .as_ref()
            .is_some_and(|edit| state.markers.by_id(edit.marker).is_none())
        {
            self.edit = None;
        }
        let mut open = true;
        let mut output = TableOutput::default();
        let marker_update_active = bridge.marker_update_active();
        egui::Window::new("Markers")
            .open(&mut open)
            .default_size([560.0, 320.0])
            .show(ctx, |ui| {
                marker_toolbar(ui, state, marker_update_active);
                ui.separator();
                self.table(ui, state, &mut output);
            });
        state.show_marker_editor = open;

        apply_output(state, bridge, output)
    }

    fn table(&mut self, ui: &mut egui::Ui, state: &mut AppState, output: &mut TableOutput) {
        self.ensure_order(state);
        let mut sort_column = self.sort_column;
        let mut sort_ascending = self.sort_ascending;
        let mut edit = self.edit.take();
        let marker_palette = crate::view::palette_for(ui.ctx().theme(), state.wiggle_theme).markers;
        let row_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        // The header is row 0 of the scroll area, the markers follow.
        egui::ScrollArea::vertical().show_rows(
            ui,
            row_height,
            self.order.len().saturating_add(1),
            |ui, rows| {
                egui::Grid::new("marker_table")
                    .striped(true)
                    .num_columns(16)
                    .spacing([10.0, 3.0])
                    .show(ui, |ui| {
                        for visible_row in rows {
                            if visible_row == 0 {
                                marker_headers(ui, &mut sort_column, &mut sort_ascending);
                                continue;
                            }
                            let Some(&id) = self.order.get(visible_row - 1) else {
                                continue;
                            };
                            state.markers.edit_with_change_flag(id, |marker| {
                                row::marker_row(ui, id, marker, &marker_palette, &mut edit, output)
                            });
                        }
                    });
            },
        );
        if self.sort_column != sort_column || self.sort_ascending != sort_ascending {
            self.order_key = None;
        }
        self.sort_column = sort_column;
        self.sort_ascending = sort_ascending;
        self.edit = edit;
    }
}

/// Applies what the rows asked for, and returns the requested beachball.
fn apply_output(state: &mut AppState, bridge: &Bridge, output: TableOutput) -> Option<[f64; 6]> {
    if let Some(row) = output.replace_selection {
        let ids: Vec<MarkerId> = state.markers.iter_with_ids().map(|(id, _)| id).collect();
        for id in ids {
            let selected = id == row;
            // Only real changes go through `edit`, which bumps the store generation.
            if state
                .markers
                .by_id(id)
                .is_some_and(|marker| marker.selected != selected)
            {
                let _ = state.markers.edit(id, |marker| marker.selected = selected);
            }
        }
    }
    if let Some(time) = output.center_time {
        let duration = (state.tmax - state.tmin).max(1.0);
        state.tmin = time - duration / 2.0;
        state.tmax = time + duration / 2.0;
    }
    for event in output.event_updates {
        let _submission = bridge.update_event(event);
    }
    output.beachball
}

fn marker_toolbar(ui: &mut egui::Ui, state: &mut AppState, marker_update_active: bool) {
    // While an analysis that emits markers is queued or running, destructive edits
    // are disabled: its commit replaces the origin's markers, so a deletion issued
    // now would appear to be silently undone moments later.
    const MARKER_UPDATE_HOVER: &str =
        "disabled while an analysis is producing markers — its result would replace this edit";
    let selected = state.markers.summary().selected;
    ui.horizontal(|ui| {
        ui.label(format!(
            "{} markers · {selected} selected",
            state.markers.len()
        ));
        if ui.button("Select all").clicked() {
            for marker in &mut state.markers {
                marker.selected = true;
            }
        }
        if ui.button("Deselect all").clicked() {
            for marker in &mut state.markers {
                marker.selected = false;
            }
        }
        if ui
            .add_enabled(!marker_update_active, egui::Button::new("Delete selected"))
            .on_disabled_hover_text(MARKER_UPDATE_HOVER)
            .clicked()
        {
            state.remove_selected_markers();
        }
        // Destroying every marker deserves a second look: the first click arms the
        // confirmation, the second (on the re-labelled button) performs it.
        let armed_id = ui.make_persistent_id("clear-all-armed");
        let armed = ui.memory(|memory| memory.data.get_temp::<bool>(armed_id).unwrap_or(false));
        let label = if armed {
            "Really clear all?"
        } else {
            "Clear all"
        };
        let response = ui
            .add_enabled(!marker_update_active, egui::Button::new(label))
            .on_disabled_hover_text(MARKER_UPDATE_HOVER);
        if response.clicked() {
            if armed {
                state.clear_markers();
            }
            ui.memory_mut(|memory| memory.data.insert_temp(armed_id, !armed));
        } else if armed && ui.input(|input| input.pointer.any_click()) {
            // A click anywhere else disarms it.
            ui.memory_mut(|memory| memory.data.insert_temp(armed_id, false));
        }
    });
}
