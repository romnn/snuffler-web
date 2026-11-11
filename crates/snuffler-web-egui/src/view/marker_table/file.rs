//! Writing the markers to a marker file.

use snuffler_web_core::bridge::{MarkerFile, MarkerFileEntry, Nslc};
use snuffler_web_core::markers::Marker;

use super::row::moment_tensor;

/// Converts markers to the bridge's marker-file message, in time order.
pub fn markers_to_file(markers: &[Marker]) -> MarkerFile {
    // Qt keeps its markers in a `Sorted(..., 'tmin')` container
    // (pile_viewer.py:833), so a saved file is always in time order.
    // Sorting here makes two logically identical files compare equal.
    let mut ordered: Vec<&Marker> = markers.iter().collect();
    ordered.sort_by(|a, b| a.tmin.total_cmp(&b.tmin));
    MarkerFile {
        markers: ordered
            .iter()
            .map(|marker| MarkerFileEntry {
                tmin: marker.tmin,
                tmax: marker.tmax,
                kind: u32::from(marker.kind),
                phase: marker.phase.clone(),
                nslc_ids: marker
                    .nslc_ids
                    .iter()
                    .map(|(net, sta, loc, cha)| Nslc {
                        net: net.clone(),
                        sta: sta.clone(),
                        loc: loc.clone(),
                        cha: cha.clone(),
                    })
                    .collect(),
                polarity: marker.polarity.map(i32::from),
                uncertainty: marker.uncertainty,
                is_event: marker.is_event,
                magnitude: marker.magnitude,
                depth: marker.depth_km.map(|depth_km| depth_km * 1000.0),
                lat: marker.lat,
                lon: marker.lon,
                event_hash: marker.event_hash.clone(),
                moment_tensor: marker.moment_tensor.map(moment_tensor),
                // A hand pick's automatic flag saves `False`, never `None`, on a
                // phase marker (Qt writes it unconditionally).
                automatic: if marker.is_event {
                    None
                } else {
                    marker
                        .automatic
                        .or(Some(false))
                        .filter(|_| marker.phase.is_some())
                },
                incidence_angle: marker.incidence_angle,
                takeoff_angle: marker.takeoff_angle,
                event_time: marker.event_time,
                event_name: marker.event_name.clone(),
                region: marker.region.clone(),
                catalog: marker.catalog.clone(),
            })
            .collect(),
    }
}
