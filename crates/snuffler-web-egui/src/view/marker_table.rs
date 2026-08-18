//! Marker-table serialization, sorting, and inline-cell widgets.

use egui::Color32;

use snuffler_web_core::bridge::{
    Bridge, EventSnapshot, MarkerFile, MarkerFileEntry, MomentTensor, Nslc,
};
use snuffler_web_core::markers::{MARKER_COLORS, Marker, MarkerColumn};
use snuffler_web_core::state::AppState;

/// Frontend-local marker-table sorting and inline editing state.
pub struct MarkerTable {
    sort: MarkerColumn,
    sort_ascending: bool,
    edit: Option<(MarkerColumn, usize, String)>,
}

impl Default for MarkerTable {
    fn default() -> Self {
        Self {
            sort: MarkerColumn::Time,
            sort_ascending: true,
            edit: None,
        }
    }
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

        bridge.ensure_station_distances(bridge.snuffling_gen());
        let mut open = true;
        let mut output = TableOutput::default();
        egui::Window::new("Markers")
            .open(&mut open)
            .default_size([560.0, 320.0])
            .show(ctx, |ui| {
                marker_toolbar(ui, state);
                ui.separator();
                self.table(ui, state, bridge, &mut output);
            });
        state.show_marker_editor = open;

        if let Some(row) = output.replace_selection {
            for (i, marker) in state.markers.iter_mut().enumerate() {
                marker.selected = i == row;
            }
        }
        if let Some(time) = output.center_time {
            let window = (state.tmax - state.tmin).max(1.0);
            state.tmin = time - window / 2.0;
            state.tmax = time + window / 2.0;
        }
        for event in output.event_updates {
            bridge.update_event(event);
        }
        output.beachball
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        bridge: &Bridge,
        output: &mut TableOutput,
    ) {
        // Sort through an index permutation so marker identity and insertion order remain stable.
        let order = self.row_order(state, bridge);
        let (mut sort, mut ascending) = (self.sort, self.sort_ascending);
        let mut edit = self.edit.take();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("marker_table")
                .striped(true)
                .num_columns(16)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    marker_headers(ui, &mut sort, &mut ascending);
                    for &row in &order {
                        let Some(marker) = state.markers.get_mut(row) else {
                            continue;
                        };
                        marker_row(ui, row, marker, &mut edit, output);
                    }
                });
        });
        self.sort = sort;
        self.sort_ascending = ascending;
        self.edit = edit;
    }

    /// A permutation of marker indices sorted by the current marker-table column
    /// (numeric, like Qt's sort role; a time tiebreak keeps it deterministic). The
    /// underlying `Vec` order is never reordered so per-row actions map correctly.
    fn row_order(&self, state: &AppState, bridge: &Bridge) -> Vec<usize> {
        let ms = &state.markers;
        let mut order: Vec<usize> = (0..ms.len()).collect();
        let (col, asc) = (self.sort, self.sort_ascending);
        let _ = bridge;
        // Qt's Dist [km]: this *event* to the active event (marker_editor.py:502),
        // precomputed in Python; rows without one sort last.
        let dist_of = |m: &Marker| -> f64 { m.dist_active_km.unwrap_or(f64::INFINITY) };
        order.sort_by(|&a, &b| {
            let (Some(ma), Some(mb)) = (ms.get(a), ms.get(b)) else {
                return std::cmp::Ordering::Equal;
            };
            let ord = match col {
                MarkerColumn::Type => ma.is_event.cmp(&mb.is_event),
                MarkerColumn::Time => ma.tmin.total_cmp(&mb.tmin),
                MarkerColumn::Span => (ma.tmax - ma.tmin).total_cmp(&(mb.tmax - mb.tmin)),
                MarkerColumn::Phase => {
                    let label = |marker: &Marker| {
                        if marker.is_event {
                            marker.event_label()
                        } else {
                            marker.phase.clone().unwrap_or_default()
                        }
                    };
                    label(ma).cmp(&label(mb))
                }
                MarkerColumn::Pol => ma
                    .polarity
                    .unwrap_or(i8::MIN)
                    .cmp(&mb.polarity.unwrap_or(i8::MIN)),
                MarkerColumn::Kind => (ma.kind & 7).cmp(&(mb.kind & 7)),
                MarkerColumn::Channel => nslc_key(ma).cmp(&nslc_key(mb)),
                MarkerColumn::Dist => dist_of(ma).total_cmp(&dist_of(mb)),
                MarkerColumn::Uncert => ma
                    .uncertainty
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.uncertainty.unwrap_or(f64::INFINITY)),
                MarkerColumn::Hash => ma
                    .event_hash
                    .as_deref()
                    .unwrap_or("")
                    .cmp(mb.event_hash.as_deref().unwrap_or("")),
                MarkerColumn::Kagan => ma
                    .kagan_deg
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.kagan_deg.unwrap_or(f64::INFINITY)),
                MarkerColumn::Mag => ma
                    .magnitude
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.magnitude.unwrap_or(f64::INFINITY)),
                MarkerColumn::Depth => ma
                    .depth_km
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.depth_km.unwrap_or(f64::INFINITY)),
                MarkerColumn::Lat => ma
                    .lat
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.lat.unwrap_or(f64::INFINITY)),
                MarkerColumn::Lon => ma
                    .lon
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&mb.lon.unwrap_or(f64::INFINITY)),
            };
            let ord = ord.then_with(|| ma.tmin.total_cmp(&mb.tmin));
            if asc { ord } else { ord.reverse() }
        });
        order
    }
}

#[derive(Default)]
struct TableOutput {
    center_time: Option<f64>,
    beachball: Option<[f64; 6]>,
    /// A plain row click replaces the whole selection with this row (Qt table
    /// semantics); applied after the draw pass because it touches every marker.
    replace_selection: Option<usize>,
    event_updates: Vec<EventSnapshot>,
}

fn marker_toolbar(ui: &mut egui::Ui, state: &mut AppState) {
    let selected = state
        .markers
        .iter()
        .filter(|marker| marker.selected)
        .count();
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
        if ui.button("Delete selected").clicked() {
            state.remove_selected_markers();
        }
        // Destroying every marker deserves a second look: the first click arms
        // the confirmation, the second (on the re-labelled button) performs it.
        let confirm_id = ui.make_persistent_id("clear-all-armed");
        let armed = ui.memory(|mem| mem.data.get_temp::<bool>(confirm_id).unwrap_or(false));
        let label = if armed {
            "Really clear all?"
        } else {
            "Clear all"
        };
        let resp = ui.button(label);
        if resp.clicked() {
            if armed {
                state.clear_markers();
            }
            ui.memory_mut(|mem| mem.data.insert_temp(confirm_id, !armed));
        } else if armed && ui.input(|i| i.pointer.any_click()) {
            // A click anywhere else disarms it.
            ui.memory_mut(|mem| mem.data.insert_temp(confirm_id, false));
        }
    });
}

fn marker_headers(ui: &mut egui::Ui, sort: &mut MarkerColumn, ascending: &mut bool) {
    const HEADERS: [(MarkerColumn, &str, Option<&str>); 15] = [
        (MarkerColumn::Type, "", None),
        (MarkerColumn::Time, "time (UTC)", None),
        (MarkerColumn::Span, "span", None),
        (
            MarkerColumn::Phase,
            "label",
            Some("phase name of a pick; composed event label (click to edit its name)"),
        ),
        (MarkerColumn::Pol, "pol", None),
        (
            MarkerColumn::Uncert,
            "unc [s]",
            Some("phase-pick time uncertainty (shift-drag a selected pick to set)"),
        ),
        (MarkerColumn::Kind, "kind", None),
        (MarkerColumn::Channel, "NSLCs", None),
        (
            MarkerColumn::Dist,
            "dist [km]",
            Some("distance from this event to the active event"),
        ),
        (
            MarkerColumn::Kagan,
            "Kagan [°]",
            Some("Kagan angle to the active event (0° = identical, 120° = maximally different)"),
        ),
        (MarkerColumn::Mag, "mag", None),
        (MarkerColumn::Depth, "depth [km]", None),
        (MarkerColumn::Lat, "lat", None),
        (MarkerColumn::Lon, "lon", None),
        (MarkerColumn::Hash, "event hash", None),
    ];

    for (column, label, hover) in HEADERS {
        let response = sort_header(ui, sort, ascending, column, label);
        if let Some(hover) = hover {
            response.on_hover_text(hover);
        }
    }
    ui.strong("");
    ui.end_row();
}

fn marker_row(
    ui: &mut egui::Ui,
    row: usize,
    marker: &mut Marker,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    output: &mut TableOutput,
) {
    let event_before = event_snapshot(marker);
    let color = MARKER_COLORS
        .get((marker.kind & 7) as usize)
        .copied()
        .unwrap_or(Color32::from_rgb(204, 0, 0));
    let symbol = egui::RichText::new(if marker.is_event { "▲" } else { "●" }).color(color);
    // Both cells select the row, and both say so: without this the only place
    // selection surfaces is the table's "N markers · M selected" summary, which
    // tells a screen-reader user how many rows are picked but never which.
    // A plain click *replaces* the selection, as clicking a Qt table row does;
    // Ctrl/Cmd/Shift adds or removes the row. A double click re-centres the
    // view on the marker, Qt's non-editable-cell double-click.
    let symbol_cell = ui.selectable_label(marker.selected, symbol);
    crate::a11y::publish_selected(ui.ctx(), &symbol_cell, marker.selected);
    let time_cell = ui.selectable_label(
        marker.selected,
        snuffler_web_core::timeaxis::format_full(marker.tmin),
    );
    crate::a11y::publish_selected(ui.ctx(), &time_cell, marker.selected);
    if symbol_cell.double_clicked() || time_cell.double_clicked() {
        output.center_time = Some(f64::midpoint(marker.tmin, marker.tmax));
    } else if symbol_cell.clicked() || time_cell.clicked() {
        let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command || i.modifiers.shift);
        if additive {
            marker.selected = !marker.selected;
        } else {
            output.replace_selection = Some(row);
        }
    }
    let span = marker.tmax - marker.tmin;
    ui.label(if span > 0.0 {
        format!("{span:.3} s")
    } else {
        "—".to_owned()
    });
    // Event display composes magnitude, region, and name like Qt.
    // Editing still targets only the raw name, which is Qt's rename path.
    if marker.is_event {
        let shown = marker.event_label();
        edit_text_cell_with_display(
            ui,
            edit,
            row,
            MarkerColumn::Phase,
            &mut marker.event_name,
            true,
            shown,
        );
    } else {
        edit_text_cell(ui, edit, row, MarkerColumn::Phase, &mut marker.phase, true);
    }
    marker_identity_cells(ui, row, marker, edit);
    marker_geometry_cells(ui, row, marker, edit);
    ui.label(marker.event_hash.as_deref().map_or_else(
        || "—".to_owned(),
        |h| {
            if h.chars().count() > 12 {
                let head: String = h.chars().take(12).collect();
                format!("{head}…")
            } else {
                h.to_owned()
            }
        },
    ));
    marker_actions(ui, marker, output);
    if let Some(event_after) = event_snapshot(marker)
        && event_before.as_ref() != Some(&event_after)
    {
        output.event_updates.push(event_after);
    }
    ui.end_row();
}

fn event_snapshot(marker: &Marker) -> Option<EventSnapshot> {
    if !marker.is_event {
        return None;
    }
    Some(EventSnapshot {
        event_hash: marker.event_hash.clone()?,
        time: marker.tmin,
        lat: marker.lat?,
        lon: marker.lon?,
        depth: marker.depth_km.unwrap_or(0.0) * 1000.0,
        magnitude: marker.magnitude,
        name: marker.event_name.clone(),
        region: marker.region.clone(),
        catalog: marker.catalog.clone(),
        moment_tensor: marker.moment_tensor.map(|tensor| MomentTensor {
            mnn: tensor[0],
            mee: tensor[1],
            mdd: tensor[2],
            mne: tensor[3],
            mnd: tensor[4],
            med: tensor[5],
        }),
    })
}

fn marker_identity_cells(
    ui: &mut egui::Ui,
    row: usize,
    marker: &mut Marker,
    edit: &mut Option<(MarkerColumn, usize, String)>,
) {
    let polarity = match marker.polarity {
        Some(1) => "+",
        Some(-1) => "-",
        Some(0) => "0",
        _ => "—",
    };
    if ui
        .small_button(polarity)
        .on_hover_text("cycle polarity (+up / -down / 0 / none)")
        .clicked()
    {
        marker.polarity = match marker.polarity {
            None => Some(1),
            Some(1) => Some(-1),
            Some(-1) => Some(0),
            _ => None,
        };
    }
    // Uncertainty [s], backed by the same field the shift-drag writes; only a
    // phase pick carries one, as in Qt.
    let editable = marker.phase.is_some();
    edit_num_cell(
        ui,
        edit,
        row,
        MarkerColumn::Uncert,
        &mut marker.uncertainty,
        3,
        editable,
    );
    if ui
        .small_button(format!("{}", marker.kind & 7))
        .on_hover_text("cycle marker kind / colour (0–7)")
        .clicked()
    {
        marker.kind = (marker.kind + 1) & 7;
    }
    ui.label(nslc_key(marker));
}

fn marker_geometry_cells(
    ui: &mut egui::Ui,
    row: usize,
    marker: &mut Marker,
    edit: &mut Option<(MarkerColumn, usize, String)>,
) {
    // Qt's Dist [km] (marker_editor.py:502): this *event* to the active event,
    // '%.5g' of kilometres, blank for anything that is not an event marker.
    ui.label(marker.dist_active_km.map_or_else(|| "—".to_owned(), fmt_g5));
    ui.label(
        marker
            .kagan_deg
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}")),
    );
    let editable = marker.is_event;
    for (column, value, precision) in [
        (MarkerColumn::Mag, &mut marker.magnitude, 1),
        (MarkerColumn::Depth, &mut marker.depth_km, 1),
        (MarkerColumn::Lat, &mut marker.lat, 2),
        (MarkerColumn::Lon, &mut marker.lon, 2),
    ] {
        edit_num_cell(ui, edit, row, column, value, precision, editable);
    }
}

fn marker_actions(ui: &mut egui::Ui, marker: &Marker, output: &mut TableOutput) {
    ui.horizontal(|ui| {
        if ui.small_button("⌖").on_hover_text("center view").clicked() {
            output.center_time = Some(f64::midpoint(marker.tmin, marker.tmax));
        }
        if let Some(moment_tensor) = marker.moment_tensor
            && ui
                .small_button("◍")
                .on_hover_text("show focal-mechanism beachball")
                .clicked()
        {
            output.beachball = Some(moment_tensor);
        }
    });
}
pub fn markers_to_file(markers: &[Marker]) -> MarkerFile {
    // Qt keeps its markers in a `Sorted(..., 'tmin')` container
    // (pile_viewer.py:833), so a saved file is always in time order — sort here
    // so two logically identical files compare equal.
    let mut ordered: Vec<&Marker> = markers.iter().collect();
    ordered.sort_by(|a, b| a.tmin.total_cmp(&b.tmin));
    MarkerFile {
        markers: ordered
            .iter()
            .map(|m| MarkerFileEntry {
                tmin: m.tmin,
                tmax: m.tmax,
                kind: u32::from(m.kind),
                phase: m.phase.clone(),
                nslc_ids: m
                    .nslc_ids
                    .iter()
                    .map(|(net, sta, loc, cha)| Nslc {
                        net: net.clone(),
                        sta: sta.clone(),
                        loc: loc.clone(),
                        cha: cha.clone(),
                    })
                    .collect(),
                polarity: m.polarity.map(i32::from),
                uncertainty: m.uncertainty,
                is_event: m.is_event,
                magnitude: m.magnitude,
                depth: m.depth_km.map(|d| d * 1000.0),
                lat: m.lat,
                lon: m.lon,
                event_hash: m.event_hash.clone(),
                moment_tensor: m.moment_tensor.map(|t| MomentTensor {
                    mnn: t[0],
                    mee: t[1],
                    mdd: t[2],
                    mne: t[3],
                    mnd: t[4],
                    med: t[5],
                }),
                // A hand pick's automatic flag saves `False`, never `None`, on a
                // phase marker (Qt writes it unconditionally).
                automatic: if m.is_event {
                    None
                } else {
                    m.automatic.or(Some(false)).filter(|_| m.phase.is_some())
                },
                incidence_angle: m.incidence_angle,
                takeoff_angle: m.takeoff_angle,
                event_time: m.event_time,
                event_name: m.event_name.clone(),
                region: m.region.clone(),
                catalog: m.catalog.clone(),
            })
            .collect(),
    }
}

/// `%.5g`, the format Qt's Dist [km] cell prints (`marker_editor.py:510`): five
/// significant digits, trailing zeros trimmed. Distances in km sit comfortably
/// inside fixed notation, so the scientific branch never fires in practice.
fn fmt_g5(v: f64) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "log10 of a finite non-zero distance is a small integer"
    )]
    let mag = v.abs().log10().floor() as i32;
    if !(-4..5).contains(&mag) {
        return format!("{v:.4e}");
    }
    let decimals = usize::try_from((4 - mag).max(0)).unwrap_or(0);
    let mut t = format!("{v:.decimals$}");
    if t.contains('.') {
        t = t.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    t
}

/// The channel column's text: every id dot-joined, ids pipe-joined — Qt's
/// NSLCs cell (`'|'.join('.'.join(id))`, `marker_editor.py:513`). Empty when the
/// marker applies to all tracks. Doubles as the column's sort key.
pub fn nslc_key(m: &Marker) -> String {
    m.nslc_ids
        .iter()
        .map(|(n, s, l, c)| format!("{n}.{s}.{l}.{c}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// A clickable marker-table column header: shows a ▲/▼ arrow on the active sort
/// column, toggles direction on re-click, and starts ascending on a new column.
pub fn sort_header(
    ui: &mut egui::Ui,
    cur: &mut MarkerColumn,
    asc: &mut bool,
    col: MarkerColumn,
    label: &str,
) -> egui::Response {
    // egui's bundled font lacks the ▲/▼ triangles (they render as tofu), so use
    // ASCII carets for the sort-direction indicator.
    let active = *cur == col;
    let text = if active {
        format!("{label} {}", if *asc { '^' } else { 'v' })
    } else {
        label.to_string()
    };
    let resp = ui.add(egui::Button::new(egui::RichText::new(text).strong()).frame(false));
    if resp.clicked() {
        if active {
            *asc = !*asc;
        } else {
            *cur = col;
            *asc = true;
        }
    }
    resp
}

/// A marker-table numeric cell that becomes an inline text editor on click and
/// commits a valid parse on Enter/blur; an invalid parse is discarded (Qt's
/// toFloat validity check). Non-editable cells render as a plain label.
pub fn edit_num_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    row: usize,
    col: MarkerColumn,
    value: &mut Option<f64>,
    prec: usize,
    editable: bool,
) {
    let shown = value.map_or_else(|| "—".to_string(), |v| format!("{v:.prec$}"));
    if !editable {
        ui.label(shown);
        return;
    }
    if matches!(edit, Some((c, r, _)) if *c == col && *r == row) {
        if let Some((_, _, buf)) = edit {
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(56.0));
            if resp.lost_focus() {
                if let Ok(v) = buf.trim().parse::<f64>() {
                    *value = Some(v);
                }
                *edit = None;
            } else {
                resp.request_focus();
            }
        }
    } else if ui
        .add(egui::Label::new(shown).sense(egui::Sense::click()))
        .on_hover_text("click to edit")
        .clicked()
    {
        *edit = Some((
            col,
            row,
            value.map_or(String::new(), |v| format!("{v:.prec$}")),
        ));
    }
}

/// A marker-table string cell (the phase name) with the same click-to-edit,
/// commit-on-blur behaviour; empty commits clear the value.
pub fn edit_text_cell(
    ui: &mut egui::Ui,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    row: usize,
    col: MarkerColumn,
    value: &mut Option<String>,
    editable: bool,
) {
    let shown = value.clone().unwrap_or_default();
    edit_text_cell_with_display(ui, edit, row, col, value, editable, shown);
}

fn edit_text_cell_with_display(
    ui: &mut egui::Ui,
    edit: &mut Option<(MarkerColumn, usize, String)>,
    row: usize,
    col: MarkerColumn,
    value: &mut Option<String>,
    editable: bool,
    shown: String,
) {
    if !editable {
        ui.label(shown);
        return;
    }
    if matches!(edit, Some((c, r, _)) if *c == col && *r == row) {
        if let Some((_, _, buf)) = edit {
            let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(110.0));
            if resp.lost_focus() {
                let trimmed = buf.trim();
                *value = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                };
                *edit = None;
            } else {
                resp.request_focus();
            }
        }
    } else if ui
        .add(egui::Label::new(shown).sense(egui::Sense::click()))
        .on_hover_text("click to edit")
        .clicked()
    {
        *edit = Some((col, row, value.clone().unwrap_or_default()));
    }
}
