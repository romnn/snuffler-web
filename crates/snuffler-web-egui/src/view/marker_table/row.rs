//! One marker's row of cells.

use snuffler_web_core::bridge::{EventSnapshot, MomentTensor};
use snuffler_web_core::markers::{Marker, MarkerColumn, MarkerId, MarkerPalette};

use super::TableOutput;
use super::cells::{CellEdit, number_cell, text_cell};

/// Draws one marker row and reports whether it mutated the marker, so the
/// store bumps its generation only for real edits instead of paying a
/// clone-and-compare per visible row per frame.
pub(super) fn marker_row(
    ui: &mut egui::Ui,
    row: MarkerId,
    marker: &mut Marker,
    selected: bool,
    marker_palette: &MarkerPalette,
    edit: &mut Option<CellEdit>,
    output: &mut TableOutput,
) -> bool {
    let mut changed = false;
    let event_before = event_snapshot(marker);
    let color = marker_palette.kind(marker.kind, true);
    let symbol = egui::RichText::new(if marker.is_event { "▲" } else { "●" }).color(color);
    // Both cells select the row, and both say so: without this the only place selection surfaces
    // is the table's "N markers · M selected" summary, which tells a screen-reader user how many
    // rows are picked but never which.
    // A plain click *replaces* the selection, as clicking a Qt table row does; Ctrl/Cmd/Shift
    // adds or removes the row.
    // A double click re-centres the view on the marker, Qt's non-editable-cell double-click.
    let symbol_cell = ui.selectable_label(selected, symbol);
    crate::a11y::publish_selected(ui.ctx(), &symbol_cell, selected);
    let time_cell = ui.selectable_label(
        selected,
        snuffler_web_core::timeaxis::format_timestamp(marker.tmin),
    );
    crate::a11y::publish_selected(ui.ctx(), &time_cell, selected);
    if symbol_cell.double_clicked() || time_cell.double_clicked() {
        output.center_time = Some(f64::midpoint(marker.tmin, marker.tmax));
    } else if symbol_cell.clicked() || time_cell.clicked() {
        let additive = ui.input(|input| {
            input.modifiers.ctrl || input.modifiers.command || input.modifiers.shift
        });
        if additive {
            output.toggle_selection = Some(row);
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
        changed |= text_cell(
            ui,
            edit,
            row,
            MarkerColumn::Phase,
            &mut marker.event_name,
            shown,
        );
    } else {
        let shown = marker.phase.clone().unwrap_or_default();
        changed |= text_cell(ui, edit, row, MarkerColumn::Phase, &mut marker.phase, shown);
    }
    changed |= marker_identity_cells(ui, row, marker, edit);
    changed |= marker_geometry_cells(ui, row, marker, edit);
    ui.label(marker.event_hash.as_deref().map_or_else(
        || "—".to_owned(),
        |hash| {
            if hash.chars().count() > 12 {
                let head: String = hash.chars().take(12).collect();
                format!("{head}…")
            } else {
                hash.to_owned()
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
    changed
}

/// The event a marker describes, for the bridge's event update; `None` for a pick
/// and for an event without an identity or a location.
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
        moment_tensor: marker.moment_tensor.map(moment_tensor),
    })
}

/// The wire form of a marker's `[mnn, mee, mdd, mne, mnd, med]` moment tensor.
pub(super) fn moment_tensor([mnn, mee, mdd, mne, mnd, med]: [f64; 6]) -> MomentTensor {
    MomentTensor {
        mnn,
        mee,
        mdd,
        mne,
        mnd,
        med,
    }
}

/// The polarity, uncertainty, kind and channel cells.
fn marker_identity_cells(
    ui: &mut egui::Ui,
    row: MarkerId,
    marker: &mut Marker,
    edit: &mut Option<CellEdit>,
) -> bool {
    let mut changed = false;
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
        changed = true;
    }
    // Uncertainty [s], backed by the same field the shift-drag writes; only a
    // phase pick carries one, as in Qt.
    let editable = marker.phase.is_some();
    changed |= number_cell(
        ui,
        edit,
        row,
        MarkerColumn::Uncertainty,
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
        changed = true;
    }
    ui.label(nslc_text(marker));
    changed
}

/// The distance, Kagan angle, magnitude, depth and location cells.
fn marker_geometry_cells(
    ui: &mut egui::Ui,
    row: MarkerId,
    marker: &mut Marker,
    edit: &mut Option<CellEdit>,
) -> bool {
    // Qt's Dist [km] (marker_editor.py:502): this *event* to the active event,
    // '%.5g' of kilometres, blank for anything that is not an event marker.
    ui.label(
        marker
            .dist_active_km
            .map_or_else(|| "—".to_owned(), format_g5),
    );
    ui.label(
        marker
            .kagan_deg
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}")),
    );
    let editable = marker.is_event;
    let mut changed = false;
    for (column, value, decimals) in [
        (MarkerColumn::Magnitude, &mut marker.magnitude, 1),
        (MarkerColumn::Depth, &mut marker.depth_km, 1),
        (MarkerColumn::Latitude, &mut marker.lat, 2),
        (MarkerColumn::Longitude, &mut marker.lon, 2),
    ] {
        changed |= number_cell(ui, edit, row, column, value, decimals, editable);
    }
    changed
}

/// The row's buttons: centre the view, and show the beachball.
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

/// `%.5g`, the format Qt's Dist [km] cell prints (`marker_editor.py:510`): five
/// significant digits, trailing zeros trimmed.
///
/// Distances in km sit comfortably inside fixed notation, so the scientific branch
/// never fires in practice.
fn format_g5(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "log10 of a finite non-zero distance is a small integer"
    )]
    let exponent = value.abs().log10().floor() as i32;
    if !(-4..5).contains(&exponent) {
        return format!("{value:.4e}");
    }
    let decimals = usize::try_from((4 - exponent).max(0)).unwrap_or(0);
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    text
}

/// The channel column's text: every id dot-joined, ids pipe-joined.
///
/// This is Qt's NSLCs cell (`'|'.join('.'.join(id))`, `marker_editor.py:513`).
/// It is empty when the marker applies to all tracks, and doubles as the column's
/// sort key.
pub(super) fn nslc_text(marker: &Marker) -> String {
    marker
        .nslc_ids
        .iter()
        .map(|(net, sta, loc, cha)| format!("{net}.{sta}.{loc}.{cha}"))
        .collect::<Vec<_>>()
        .join("|")
}
