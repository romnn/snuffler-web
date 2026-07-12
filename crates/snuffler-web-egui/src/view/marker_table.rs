//! Marker-table serialization, sorting, and inline-cell widgets.

use egui::Color32;

use snuffler_web_core::bridge::{Bridge, MarkerFile, MarkerFileEntry, MomentTensor, Nslc};
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

        if let Some(time) = output.center_time {
            let window = (state.tmax - state.tmin).max(1.0);
            state.tmin = time - window / 2.0;
            state.tmax = time + window / 2.0;
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
                .num_columns(14)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    marker_headers(ui, &mut sort, &mut ascending);
                    for &row in &order {
                        let Some(marker) = state.markers.get_mut(row) else {
                            continue;
                        };
                        marker_row(ui, row, marker, &mut edit, bridge, output);
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
        let dist_of = |m: &Marker| -> f64 {
            m.nslc
                .as_ref()
                .and_then(|(n, s, _, _)| bridge.station_distance(n, s))
                .unwrap_or(f64::INFINITY)
        };
        order.sort_by(|&a, &b| {
            let (Some(ma), Some(mb)) = (ms.get(a), ms.get(b)) else {
                return std::cmp::Ordering::Equal;
            };
            let ord = match col {
                MarkerColumn::Type => ma.is_event.cmp(&mb.is_event),
                MarkerColumn::Time => ma.tmin.total_cmp(&mb.tmin),
                MarkerColumn::Span => (ma.tmax - ma.tmin).total_cmp(&(mb.tmax - mb.tmin)),
                MarkerColumn::Phase => ma
                    .phase
                    .as_deref()
                    .unwrap_or("")
                    .cmp(mb.phase.as_deref().unwrap_or("")),
                MarkerColumn::Pol => ma
                    .polarity
                    .unwrap_or(i8::MIN)
                    .cmp(&mb.polarity.unwrap_or(i8::MIN)),
                MarkerColumn::Kind => (ma.kind & 7).cmp(&(mb.kind & 7)),
                MarkerColumn::Channel => nslc_key(ma).cmp(&nslc_key(mb)),
                MarkerColumn::Dist => dist_of(ma).total_cmp(&dist_of(mb)),
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
            state.markers.retain(|marker| !marker.selected);
        }
        if ui.button("Clear all").clicked() {
            state.markers.clear();
        }
    });
}

fn marker_headers(ui: &mut egui::Ui, sort: &mut MarkerColumn, ascending: &mut bool) {
    const HEADERS: [(MarkerColumn, &str, Option<&str>); 13] = [
        (MarkerColumn::Type, "", None),
        (MarkerColumn::Time, "time (UTC)", None),
        (MarkerColumn::Span, "span", None),
        (MarkerColumn::Phase, "phase", None),
        (MarkerColumn::Pol, "pol", None),
        (MarkerColumn::Kind, "kind", None),
        (MarkerColumn::Channel, "channel", None),
        (
            MarkerColumn::Dist,
            "dist",
            Some("great-circle distance from the active event [°]"),
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
    bridge: &Bridge,
    output: &mut TableOutput,
) {
    let color = MARKER_COLORS
        .get((marker.kind & 7) as usize)
        .copied()
        .unwrap_or(Color32::from_rgb(204, 0, 0));
    let symbol = egui::RichText::new(if marker.is_event { "▲" } else { "●" }).color(color);
    if ui.selectable_label(marker.selected, symbol).clicked() {
        marker.selected = !marker.selected;
    }
    if ui
        .selectable_label(
            marker.selected,
            snuffler_web_core::timeaxis::format_full(marker.tmin),
        )
        .clicked()
    {
        marker.selected = !marker.selected;
    }
    let span = marker.tmax - marker.tmin;
    ui.label(if span > 0.0 {
        format!("{span:.3} s")
    } else {
        "—".to_owned()
    });
    edit_text_cell(
        ui,
        edit,
        row,
        MarkerColumn::Phase,
        &mut marker.phase,
        !marker.is_event,
    );
    marker_identity_cells(ui, marker);
    marker_geometry_cells(ui, row, marker, edit, bridge);
    marker_actions(ui, marker, output);
    ui.end_row();
}

fn marker_identity_cells(ui: &mut egui::Ui, marker: &mut Marker) {
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
    bridge: &Bridge,
) {
    let distance = marker
        .nslc
        .as_ref()
        .and_then(|(network, station, _, _)| bridge.station_distance(network, station));
    ui.label(distance.map_or_else(|| "—".to_owned(), |value| format!("{value:.2}°")));
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
    MarkerFile {
        markers: markers
            .iter()
            .map(|m| MarkerFileEntry {
                tmin: m.tmin,
                tmax: m.tmax,
                kind: u32::from(m.kind),
                phase: m.phase.clone(),
                nslc: m.nslc.as_ref().map(|(net, sta, loc, cha)| Nslc {
                    net: net.clone(),
                    sta: sta.clone(),
                    loc: loc.clone(),
                    cha: cha.clone(),
                }),
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
            })
            .collect(),
    }
}

/// The `n.s.l.c` string a marker row displays in the channel column (empty when
/// the marker applies to all tracks), used as its sort key.
pub fn nslc_key(m: &Marker) -> String {
    m.nslc
        .as_ref()
        .map(|(n, s, l, c)| format!("{n}.{s}.{l}.{c}"))
        .unwrap_or_default()
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
