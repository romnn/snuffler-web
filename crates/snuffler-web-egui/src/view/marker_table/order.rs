//! The table's column headers and the sort order they select.

use std::cmp::Ordering;
use std::collections::HashMap;

use snuffler_web_core::markers::{Marker, MarkerColumn, MarkerId};
use snuffler_web_core::state::AppState;

use super::MarkerTable;
use super::row::nslc_text;

/// What a sorted order was computed for: the marker store's generation and the
/// sort selection.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct OrderKey {
    generation: u64,
    column: MarkerColumn,
    ascending: bool,
}

/// One column header.
struct Header {
    column: MarkerColumn,
    label: &'static str,
    hover: Option<&'static str>,
}

/// The sortable columns, in table order; the trailing action column has no header.
const HEADERS: [Header; 15] = [
    Header {
        column: MarkerColumn::Type,
        label: "",
        hover: None,
    },
    Header {
        column: MarkerColumn::Time,
        label: "time (UTC)",
        hover: None,
    },
    Header {
        column: MarkerColumn::Span,
        label: "span",
        hover: None,
    },
    Header {
        column: MarkerColumn::Phase,
        label: "label",
        hover: Some("phase name of a pick; composed event label (click to edit its name)"),
    },
    Header {
        column: MarkerColumn::Polarity,
        label: "pol",
        hover: None,
    },
    Header {
        column: MarkerColumn::Uncertainty,
        label: "unc [s]",
        hover: Some("phase-pick time uncertainty (shift-drag a selected pick to set)"),
    },
    Header {
        column: MarkerColumn::Kind,
        label: "kind",
        hover: None,
    },
    Header {
        column: MarkerColumn::Channel,
        label: "NSLCs",
        hover: None,
    },
    Header {
        column: MarkerColumn::Distance,
        label: "dist [km]",
        hover: Some("distance from this event to the active event"),
    },
    Header {
        column: MarkerColumn::Kagan,
        label: "Kagan [°]",
        hover: Some("Kagan angle to the active event (0° = identical, 120° = maximally different)"),
    },
    Header {
        column: MarkerColumn::Magnitude,
        label: "mag",
        hover: None,
    },
    Header {
        column: MarkerColumn::Depth,
        label: "depth [km]",
        hover: None,
    },
    Header {
        column: MarkerColumn::Latitude,
        label: "lat",
        hover: None,
    },
    Header {
        column: MarkerColumn::Longitude,
        label: "lon",
        hover: None,
    },
    Header {
        column: MarkerColumn::Hash,
        label: "event hash",
        hover: None,
    },
];

/// The header row: one sort button per column.
pub(super) fn marker_headers(
    ui: &mut egui::Ui,
    sort_column: &mut MarkerColumn,
    ascending: &mut bool,
) {
    for header in &HEADERS {
        let response = sort_header(ui, sort_column, ascending, header);
        if let Some(hover) = header.hover {
            response.on_hover_text(hover);
        }
    }
    ui.strong("");
    ui.end_row();
}

/// A clickable column header.
///
/// It shows a direction arrow on the active sort column, toggles the direction on a
/// re-click, and starts ascending on a new column.
fn sort_header(
    ui: &mut egui::Ui,
    sort_column: &mut MarkerColumn,
    ascending: &mut bool,
    header: &Header,
) -> egui::Response {
    // egui's bundled font lacks the ▲/▼ triangles (they render as tofu), so use
    // ASCII carets for the sort-direction indicator.
    let active = *sort_column == header.column;
    let text = if active {
        format!("{} {}", header.label, if *ascending { '^' } else { 'v' })
    } else {
        header.label.to_owned()
    };
    let response = ui.add(egui::Button::new(egui::RichText::new(text).strong()).frame(false));
    if response.clicked() {
        if active {
            *ascending = !*ascending;
        } else {
            *sort_column = header.column;
            *ascending = true;
        }
    }
    response
}

impl MarkerTable {
    /// Refreshes the stable marker identities sorted by the current table column.
    ///
    /// The insertion order is never reordered, and the permutation remains valid
    /// until the marker store or sort selection changes.
    pub(super) fn ensure_order(&mut self, state: &AppState) {
        let key = OrderKey {
            generation: state.markers.generation(),
            column: self.sort_column,
            ascending: self.sort_ascending,
        };
        if self.order_key == Some(key) {
            return;
        }
        let markers = &state.markers;
        let mut order: Vec<MarkerId> = markers.iter_with_ids().map(|(id, _)| id).collect();
        // The text columns compose their keys once, not once per comparison.
        let text_keys: HashMap<MarkerId, String> = match key.column {
            MarkerColumn::Phase => markers
                .iter_with_ids()
                .map(|(id, marker)| {
                    let text = if marker.is_event {
                        marker.event_label()
                    } else {
                        marker.phase.clone().unwrap_or_default()
                    };
                    (id, text)
                })
                .collect(),
            MarkerColumn::Channel => markers
                .iter_with_ids()
                .map(|(id, marker)| (id, nslc_text(marker)))
                .collect(),
            _ => HashMap::new(),
        };
        order.sort_by(|&a, &b| {
            let (Some(marker_a), Some(marker_b)) = (markers.by_id(a), markers.by_id(b)) else {
                return Ordering::Equal;
            };
            let ordering = compare(key.column, marker_a, marker_b, || {
                text_keys.get(&a).cmp(&text_keys.get(&b))
            })
            .then_with(|| marker_a.tmin.total_cmp(&marker_b.tmin));
            if key.ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
        self.order = order;
        self.order_key = Some(key);
    }
}

/// Compares two markers by one column; `compare_text` compares the text columns.
///
/// Missing numbers sort last.
fn compare(
    column: MarkerColumn,
    a: &Marker,
    b: &Marker,
    compare_text: impl FnOnce() -> Ordering,
) -> Ordering {
    let number = |value: Option<f64>| value.unwrap_or(f64::INFINITY);
    match column {
        MarkerColumn::Type => a.is_event.cmp(&b.is_event),
        MarkerColumn::Time => a.tmin.total_cmp(&b.tmin),
        MarkerColumn::Span => (a.tmax - a.tmin).total_cmp(&(b.tmax - b.tmin)),
        MarkerColumn::Phase | MarkerColumn::Channel => compare_text(),
        MarkerColumn::Polarity => a
            .polarity
            .unwrap_or(i8::MIN)
            .cmp(&b.polarity.unwrap_or(i8::MIN)),
        MarkerColumn::Kind => (a.kind & 7).cmp(&(b.kind & 7)),
        // Qt's Dist [km]: this *event* to the active event (marker_editor.py:502),
        // precomputed in Python.
        MarkerColumn::Distance => number(a.dist_active_km).total_cmp(&number(b.dist_active_km)),
        MarkerColumn::Uncertainty => number(a.uncertainty).total_cmp(&number(b.uncertainty)),
        MarkerColumn::Hash => a
            .event_hash
            .as_deref()
            .unwrap_or("")
            .cmp(b.event_hash.as_deref().unwrap_or("")),
        MarkerColumn::Kagan => number(a.kagan_deg).total_cmp(&number(b.kagan_deg)),
        MarkerColumn::Magnitude => number(a.magnitude).total_cmp(&number(b.magnitude)),
        MarkerColumn::Depth => number(a.depth_km).total_cmp(&number(b.depth_km)),
        MarkerColumn::Latitude => number(a.lat).total_cmp(&number(b.lat)),
        MarkerColumn::Longitude => number(a.lon).total_cmp(&number(b.lon)),
    }
}
