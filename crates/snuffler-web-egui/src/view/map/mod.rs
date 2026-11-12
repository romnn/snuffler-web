//! A slippy-map view: OSM / Carto / satellite raster tiles with station and event
//! overlays.
//!
//! The browser fetches and decodes the tiles (CORS-clean providers), and pyrocko
//! supplies the station and event coordinates.
//! Rust only projects and draws: the same kind of rendering geometry as the time
//! axis, not seismological computation.
//! The camera itself lives in [`snuffler_web_render::map`], where it can be tested
//! on the host.

mod basemap;
mod overlay;
mod tiles;

use egui::{Color32, Rect, Sense};
use snuffler_web_core::bridge::{Bridge, MapData, MapRequest};
use snuffler_web_core::state::AppState;
use snuffler_web_render::map::{Camera, GeoBounds};
use strum::IntoEnumIterator as _;

use self::basemap::Basemap;
use self::tiles::TileCache;

/// How often the station and event coordinates are refetched while the window is
/// open, so the map keeps up with Download and Catalog fetches unprompted.
const POLL_INTERVAL_S: f64 = 2.0;

/// Screen points of wheel travel worth one zoom level.
///
/// egui has already normalized line- and page-mode wheels into points by the time
/// this reads them, so one detent of a typical mouse wheel comes out as one level
/// either way.
const POINTS_PER_ZOOM_LEVEL: f32 = 120.0;

/// Flat colour behind the tiles, and the curtain the first view assembles behind.
const BACKDROP: Color32 = Color32::from_gray(40);

/// Zoom a double-click settles on, unless the reader is already closer in.
///
/// Chosen by looking, not computed: at 8 a station in Berlin shows Berlin and the
/// country around it, enough context to place it, not so close that orienting is
/// hard.
/// Nothing about the data can pick this number, so it is a judgement.
const FOCUS_ZOOM: f64 = 8.0;

/// Zoom levels per second past which tiles are not requested.
///
/// Mid-flick the camera is crossing tile levels it will have left before their
/// tiles could arrive.
/// The ancestors already on the GPU cover those frames, and waiting until the glide
/// is nearly done spends the connection on the level the user is actually stopping
/// at.
const MAX_REQUEST_ZOOM_SPEED: f64 = 2.0;

/// The map window's own state, kept across frames.
pub struct MapState {
    camera: Camera,
    /// The zoom the last frame drew, for measuring how fast the camera is moving.
    last_zoom: f64,
    basemap: Basemap,
    /// When the coordinates were last requested, in egui seconds.
    last_poll_s: f64,
    /// Fingerprint of the coordinates the view last auto-fitted to
    /// ([`fit_fingerprint`]), so a fit happens when the data changes and not when
    /// the user has merely panned away.
    ///
    /// `None` forces the next fit.
    fitted_key: Option<u64>,
    show_plates: bool,
    /// Qt's "Only active event" switch (map/__init__.py:242).
    only_active_event: bool,
    /// Event-marker projection used for the last coordinate request.
    last_request: Option<MapRequest>,
    tiles: TileCache,
    /// True until the window has drawn a real frame.
    ///
    /// The first fit cuts instead of easing: there is nothing on screen yet to
    /// animate away from, and easing would parade the default world view on the way
    /// to the one asked for.
    opening: bool,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            last_zoom: Camera::default().zoom(),
            basemap: Basemap::OpenStreetMap,
            last_poll_s: -1.0e9,
            fitted_key: None,
            show_plates: true,
            tiles: TileCache::default(),
            opening: true,
            only_active_event: false,
            last_request: None,
        }
    }
}

/// Draws the map window requested by shared application state.
pub fn window(ctx: &egui::Context, map: &mut MapState, state: &mut AppState, bridge: &Bridge) {
    if !state.show_map {
        // Re-opening starts over, so the map never flashes whatever it happened to
        // be showing when it was last closed.
        map.opening = true;
        return;
    }
    let mut open = true;
    egui::Window::new("Map — stations & events")
        .open(&mut open)
        .default_size([640.0, 480.0])
        .show(ctx, |ui| show(ui, map, bridge, ctx, state));
    state.show_map = open;
}

/// A fingerprint of what the auto-fit would frame: every station and event
/// coordinate, hashed, with the station count separating the two lists.
///
/// Counts alone are not enough: switching between two selected events, or swapping
/// datasets with equal counts, moves the coordinates without changing either count.
/// `None` while there is nothing to frame, so an empty poll never latches as fitted.
fn fit_fingerprint(data: &MapData) -> Option<u64> {
    use std::hash::Hasher;
    if data.stations.is_empty() && data.events.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_usize(data.stations.len());
    for station in &data.stations {
        hasher.write_u64(station.lat.to_bits());
        hasher.write_u64(station.lon.to_bits());
    }
    for event in &data.events {
        hasher.write_u64(event.lat.to_bits());
        hasher.write_u64(event.lon.to_bits());
    }
    Some(hasher.finish())
}

/// The window's content: the toolbar, then the map.
fn show(
    ui: &mut egui::Ui,
    map: &mut MapState,
    bridge: &Bridge,
    ctx: &egui::Context,
    state: &AppState,
) {
    let data = toolbar(ui, map, bridge, ctx, state);

    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;
    painter.rect_filled(rect, 0.0, BACKDROP);

    let Some(data) = &data else {
        // Nothing to point at yet.
        // Holding a plain backdrop until the coordinates land is what keeps opening
        // still: otherwise the map shows its default world view, pulls a screenful
        // of tiles for ground nobody asked to see, and then jumps elsewhere.
        // `toolbar` already scheduled the repaint that brings us back when the poll
        // answers.
        overlay::notice(&painter, rect, "Loading map…");
        return;
    };

    auto_fit(map, data, rect);
    move_camera(ui, ctx, map, &response, rect);
    draw_tiles(ctx, map, bridge, &painter, rect);

    // Overlays
    let project = overlay::projector(map.camera, rect);
    // One hit test feeds the hover styling, the pointer shape, and the
    // double-click, so what lights up under the pointer is exactly what a
    // double-click acts on.
    let hit = response
        .hover_pos()
        .and_then(|pointer| overlay::hit_test(data, pointer, &project));
    if hit.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.double_clicked()
        && let Some(target) = hit.and_then(|hit| hit.position(data))
    {
        map.camera.focus_on(target, FOCUS_ZOOM, rect);
    }
    if map.show_plates {
        overlay::plate_boundaries(&painter, data, &project);
    }
    overlay::stations(
        &painter,
        data,
        rect,
        map.basemap.label_style(),
        hit,
        &project,
    );
    overlay::events(&painter, data, rect, hit, &project);
    overlay::footer(&painter, rect, map.basemap.attribution());

    let levels = overlay::zoom_buttons(ui, rect);
    if levels != 0.0 {
        // No focus: the buttons zoom the middle of the view, since there is no
        // gesture position to zoom towards.
        map.camera.nudge_zoom(levels, None, rect);
    }
}

/// Frames the stations and events whenever they change.
fn auto_fit(map: &mut MapState, data: &MapData, rect: Rect) {
    let key = fit_fingerprint(data);
    if key.is_some() && key != map.fitted_key {
        let points = data
            .stations
            .iter()
            .map(|station| (station.lat, station.lon))
            .chain(data.events.iter().map(|event| (event.lat, event.lon)));
        if let Some(bounds) = GeoBounds::around(points) {
            if map.opening {
                map.camera.cut_to_fit(bounds, rect);
            } else {
                map.camera.fit(bounds, rect);
            }
        }
        map.fitted_key = key;
    }
    // Whether or not there was anything to frame, the window has now seen its data;
    // a later fit is a deliberate move and should ease.
    map.opening = false;
}

/// Applies this frame's drag and wheel, then steps the camera's easing.
fn move_camera(
    ui: &egui::Ui,
    ctx: &egui::Context,
    map: &mut MapState,
    response: &egui::Response,
    rect: Rect,
) {
    // Drag and wheel are applied before the step so this frame already shows them.
    if response.dragged() {
        map.camera.pan(response.drag_delta(), rect);
    }
    apply_wheel(ui, map, rect);
    let frame_seconds = ctx.input(|input| f64::from(input.stable_dt));
    if map.camera.advance(frame_seconds, rect) {
        ctx.request_repaint();
    }
}

/// Paints the tiles on hand, and requests the missing ones unless the zoom is
/// racing past them.
fn draw_tiles(
    ctx: &egui::Context,
    map: &mut MapState,
    bridge: &Bridge,
    painter: &egui::Painter,
    rect: Rect,
) {
    let (now, frame_seconds) = ctx.input(|input| (input.time, f64::from(input.stable_dt)));
    let zoom_speed = ((map.camera.zoom() - map.last_zoom) / frame_seconds.max(1e-3)).abs();
    map.last_zoom = map.camera.zoom();

    let grid = map.camera.tile_grid(rect);
    map.tiles.absorb_ready(ctx, bridge, now);
    if map.tiles.paint(painter, &grid, now) {
        ctx.request_repaint();
    }
    if zoom_speed < MAX_REQUEST_ZOOM_SPEED {
        let basemap = map.basemap;
        bridge.request_tiles(
            map.tiles
                .wanted(&grid, |zoom, x, y| basemap.tile_url(zoom, x, y)),
        );
    }
    map.tiles.evict_stale(bridge);
}

/// The controls above the map, returning the coordinates this frame will draw.
fn toolbar(
    ui: &mut egui::Ui,
    map: &mut MapState,
    bridge: &Bridge,
    ctx: &egui::Context,
    state: &AppState,
) -> Option<MapData> {
    let previous_basemap = map.basemap;
    let mut refresh = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Basemap")
            .selected_text(map.basemap.name())
            .show_ui(ui, |ui| {
                for basemap in Basemap::iter() {
                    ui.selectable_value(&mut map.basemap, basemap, basemap.name());
                }
            });
        if ui.button("Refresh data").clicked() {
            refresh = true;
        }
        if ui.button("Fit").clicked() {
            map.fitted_key = None;
        }
        ui.checkbox(&mut map.show_plates, "Plate boundaries");
        refresh |= ui
            .checkbox(&mut map.only_active_event, "Only active event")
            .changed();
        ui.label(format!("zoom {:.1}", map.camera.zoom()));
    });

    // Both tile caches are keyed by (z, x, y) with no provider, so a new basemap
    // only reaches keys that were never fetched.
    // Drop both to refetch every visible tile from the provider now selected.
    if map.basemap != previous_basemap {
        map.tiles.clear();
        bridge.clear_tiles();
    }

    let request = map_request(state, map.only_active_event);
    if map.last_request.as_ref() != Some(&request) {
        refresh = true;
        map.last_request = Some(request.clone());
    }
    refresh |= !bridge.map_request_is_current(&request);
    let data = bridge.map_data();
    let now = ctx.input(|input| input.time);
    if refresh || now - map.last_poll_s > POLL_INTERVAL_S {
        let _submission = bridge.fetch_map_data(request);
        map.last_poll_s = now;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(POLL_INTERVAL_S));
    data
}

/// Selects the event-marker projection Qt hands to its map snuffling.
fn map_request(state: &AppState, only_active_event: bool) -> MapRequest {
    let any_selected = state.markers.summary().selected > 0;
    let active_event = state
        .markers
        .iter()
        .find(|marker| marker.is_event && marker.active);
    let mut event_hashes = Vec::new();
    for (id, marker) in state.markers.iter_with_ids() {
        let included = marker.is_event
            && if only_active_event {
                marker.active
            } else if any_selected {
                state.markers.is_selected(id)
            } else {
                marker.tmin >= state.tmin && marker.tmax <= state.tmax
            };
        if included
            && let Some(event_hash) = &marker.event_hash
            && !event_hashes.contains(event_hash)
        {
            event_hashes.push(event_hash.clone());
        }
    }
    MapRequest {
        event_hashes,
        only_active_event,
        active_event_hash: active_event.and_then(|marker| marker.event_hash.clone()),
        active_event_time: active_event.map(|marker| marker.tmin),
    }
}

/// Turns wheel and pinch input into a new zoom target aimed at the cursor.
fn apply_wheel(ui: &egui::Ui, map: &mut MapState, rect: Rect) {
    // `Ui::rect_contains_pointer` rather than the response's own hover flag: it
    // asks whether the pointer is over this rectangle in this window, without
    // caring that the `+`/`−` buttons are stacked on part of it.
    // Scrolling over the buttons should still zoom the map they sit on.
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let (scroll, pinch, cursor) = ui.input(|input| {
        (
            input.smooth_scroll_delta.y,
            input.zoom_delta(),
            input.pointer.hover_pos(),
        )
    });
    // egui routes ctrl+wheel and trackpad pinches into `zoom_delta` and zeroes the
    // scroll delta, so the two never double-count.
    let pinch_levels = if pinch > 0.0 {
        f64::from(pinch).log2()
    } else {
        0.0
    };
    let levels = f64::from(scroll / POINTS_PER_ZOOM_LEVEL) + pinch_levels;
    if levels.abs() < 1e-6 {
        return;
    }
    map.camera.nudge_zoom(levels, cursor, rect);
}

#[cfg(test)]
mod tests {
    use snuffler_web_core::markers::Marker;

    use super::*;

    #[test]
    fn map_projection_uses_selection_then_visible_range() {
        let mut state = AppState {
            tmin: 10.0,
            tmax: 20.0,
            ..AppState::default()
        };
        for (time, hash) in [(15.0, "visible"), (30.0, "outside")] {
            let mut marker = Marker::new(time, time, 0);
            marker.is_event = true;
            marker.event_hash = Some(hash.to_owned());
            state.markers.insert(marker);
        }

        assert_eq!(map_request(&state, false).event_hashes, ["visible"]);

        state
            .markers
            .select_where(|marker| marker.event_hash.as_deref() == Some("outside"));
        assert_eq!(map_request(&state, false).event_hashes, ["outside"]);

        for marker in &mut state.markers {
            marker.active = marker.event_hash.as_deref() == Some("visible");
        }
        let active = map_request(&state, true);
        assert_eq!(active.event_hashes, ["visible"]);
        assert_eq!(active.active_event_hash.as_deref(), Some("visible"));
        assert!(
            active
                .active_event_time
                .is_some_and(|time| (time - 15.0).abs() < f64::EPSILON)
        );
    }
}
