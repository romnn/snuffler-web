//! How Python's marker results and event registry land in the session's markers.

use std::collections::HashMap;

use crate::bridge::{ActiveEventResult, Nslc, SnufMarker};
use crate::markers::Marker;
#[cfg(target_arch = "wasm32")]
use crate::markers::{MarkerBudget, MarkerCommitOutcome, StagedMarkers};

use super::Session;

/// Converts a wire marker, resolving its channel indexes through the result's NSLC table.
fn marker_from_snuffling(marker: &SnufMarker, nslc_table: &[Nslc], origin: &str) -> Marker {
    let mut mapped = Marker::new(
        marker.tmin,
        marker.tmax.unwrap_or(marker.tmin),
        u8::try_from(marker.kind).unwrap_or(0),
    );
    mapped.phase.clone_from(&marker.phase);
    mapped.is_event = marker.is_event;
    mapped.nslc_ids = marker
        .nslc_indexes
        .iter()
        .filter_map(|index| {
            let nslc = nslc_table.get(usize::try_from(*index).ok()?)?;
            Some((
                nslc.net.clone(),
                nslc.sta.clone(),
                nslc.loc.clone(),
                nslc.cha.clone(),
            ))
        })
        .collect();
    mapped.polarity = marker.polarity.and_then(|value| i8::try_from(value).ok());
    mapped.moment_tensor = marker.moment_tensor.as_ref().map(|tensor| {
        [
            tensor.mnn, tensor.mee, tensor.mdd, tensor.mne, tensor.mnd, tensor.med,
        ]
    });
    mapped.kagan_deg = marker.kagan_deg;
    mapped.uncertainty = marker.uncertainty;
    mapped.event_hash.clone_from(&marker.event_hash);
    mapped.magnitude = marker.magnitude;
    // The wire depth is in metres, pyrocko's unit.
    mapped.depth_km = marker.depth.map(|depth| depth / 1000.0);
    mapped.lat = marker.lat;
    mapped.lon = marker.lon;
    mapped.automatic = marker.automatic;
    mapped.incidence_angle = marker.incidence_angle;
    mapped.takeoff_angle = marker.takeoff_angle;
    mapped.event_name.clone_from(&marker.event_name);
    mapped.region.clone_from(&marker.region);
    mapped.catalog.clone_from(&marker.catalog);
    mapped.event_time = marker.event_time;
    mapped.dist_active_km = marker.dist_active_km;
    mapped.active = marker.active;
    if !origin.is_empty() {
        mapped.origin = Some(origin.to_owned());
    }
    mapped
}

impl Session {
    /// Commits the latest snuffling result: its markers, its text, and a jump to its
    /// events when they are all off screen.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn apply_snuffling_result(&mut self) {
        let generation = self.bridge.snuffling_generation();
        if generation == self.last_snuffling_generation {
            return;
        }
        self.last_snuffling_generation = generation;
        let Some(result) = self.bridge.take_snuffling() else {
            return;
        };

        // Convert the markers and find the events' time extent
        let mut event_span: Option<(f64, f64)> = None;
        let mut staged_markers = Vec::with_capacity(result.markers.len());
        let mut activates_event = false;
        for marker in &result.markers {
            // Python-side auto-activation (largest catalog magnitude) arrives on
            // the marker so the canvas and table *show* the active event rather
            // than silently measuring distances from an invisible one.
            if marker.active {
                activates_event = true;
            }
            if marker.is_event {
                event_span = Some(event_span.map_or((marker.tmin, marker.tmin), |(min, max)| {
                    (min.min(marker.tmin), max.max(marker.tmin))
                }));
            }
            staged_markers.push(marker_from_snuffling(
                marker,
                &result.nslc_table,
                &result.origin,
            ));
        }

        // Commit them
        if activates_event {
            for existing in &mut self.state.markers {
                existing.active = false;
            }
        }
        let outcome = self.state.markers.commit(StagedMarkers {
            markers: staged_markers,
            origin: (!result.origin.is_empty()).then(|| result.origin.clone()),
            dropped: result.markers_dropped,
            budget: MarkerBudget::from_wire(result.marker_budget),
        });

        // Show the text, with the truncation notice the commit forces
        let truncation = match outcome {
            MarkerCommitOutcome::Committed { .. } => None,
            MarkerCommitOutcome::Truncated {
                added,
                dropped,
                budget,
            } => Some(format!(
                "Showing the {added} strongest markers; {dropped} detections were omitted at the {} marker budget.",
                budget.get()
            )),
        };
        self.state.snuffling_text = result.text;
        if let Some(truncation) = truncation {
            if !self.state.snuffling_text.is_empty() {
                self.state.snuffling_text.push_str("\n\n");
            }
            self.state.snuffling_text.push_str(&truncation);
        }
        self.state.show_snuffling_text = (result.show_window || result.markers_dropped > 0)
            && !self.state.snuffling_text.is_empty();

        // Jump to the events when none of them is on screen
        if let Some((event_min, event_max)) = event_span
            && (event_max < self.state.tmin || event_min > self.state.tmax)
        {
            let padding = ((event_max - event_min) * 0.1).max(120.0);
            self.state.tmin = event_min - padding;
            self.state.tmax = event_max + padding;
        }
    }

    /// Mirrors Python's canonical event registry onto the markers: event fields,
    /// the active flag, and the Kagan angle and distance to the active event.
    pub(super) fn apply_event_state(markers: &mut [Marker], state: &ActiveEventResult) {
        let events: HashMap<&str, _> = state
            .events
            .iter()
            .map(|event| (event.event_hash.as_str(), event))
            .collect();
        let metrics: HashMap<&str, _> = state
            .kagan
            .iter()
            .map(|metric| (metric.event_hash.as_str(), metric))
            .collect();
        let active_hash = state
            .has_event
            .then_some(state.active_hash.as_deref())
            .flatten();

        for marker in markers {
            let Some(event_hash) = marker.event_hash.as_deref() else {
                if marker.is_event {
                    marker.active = false;
                }
                continue;
            };
            if let Some(event) = events.get(event_hash) {
                // A phase pick keeps its event's time as the association fallback.
                marker.event_time = (!marker.is_event).then_some(event.time);
                if marker.is_event {
                    marker.tmin = event.time;
                    marker.tmax = event.time;
                    marker.lat = Some(event.lat);
                    marker.lon = Some(event.lon);
                    marker.depth_km = Some(event.depth / 1000.0);
                    marker.magnitude = event.magnitude;
                    marker.event_name.clone_from(&event.name);
                    marker.region.clone_from(&event.region);
                    marker.catalog.clone_from(&event.catalog);
                    marker.moment_tensor = event.moment_tensor.as_ref().map(|tensor| {
                        [
                            tensor.mnn, tensor.mee, tensor.mdd, tensor.mne, tensor.mnd, tensor.med,
                        ]
                    });
                }
            }
            marker.active = marker.is_event && active_hash == Some(event_hash);
            if marker.is_event {
                marker.kagan_deg = metrics.get(event_hash).and_then(|metric| metric.kagan_deg);
                marker.dist_active_km = metrics.get(event_hash).and_then(|metric| metric.dist_km);
            } else {
                marker.kagan_deg = None;
                marker.dist_active_km = None;
            }
        }
    }
}
