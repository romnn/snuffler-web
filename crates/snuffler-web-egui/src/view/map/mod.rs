//! A slippy-map view: OSM / Carto / satellite raster tiles with station and event
//! overlays.
//!
//! The browser fetches and decodes the tiles (CORS-clean providers) and pyrocko
//! supplies the station/event coordinates; Rust only projects and draws — the
//! same kind of rendering geometry as the time axis, not seismological
//! computation. The camera itself lives in
//! [`snuffler_web_render::map`], where it can be tested on the host.

mod overlay;
mod tiles;

use egui::{Color32, Rect, Sense};

use snuffler_web_core::bridge::{Bridge, MapData, MapRequest};
use snuffler_web_core::state::AppState;
use snuffler_web_render::map::{Camera, GeoBounds};
use strum::{EnumIter, IntoEnumIterator as _};

use tiles::TileCache;

/// The raster basemaps on offer.
///
/// An enum rather than an index into a table so that everything which varies per
/// provider — the tiles, the credit, and the colour of ink that stays legible on
/// top of them — is one exhaustive `match` away, and adding a provider cannot
/// compile until each of those has an answer. `EnumIter` extends that to the
/// picker: a hand-written array of the variants would keep compiling at its old
/// length and quietly leave the new basemap unofferable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter)]
enum Basemap {
    OpenStreetMap,
    CartoLight,
    CartoDark,
    Satellite,
}

impl Basemap {
    /// The name shown in the picker.
    fn name(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "OpenStreetMap",
            Self::CartoLight => "Carto light",
            Self::CartoDark => "Carto dark",
            Self::Satellite => "Satellite",
        }
    }

    /// Tile URL template. `{z}/{x}/{y}` are substituted; ESRI orders them
    /// `{z}/{y}/{x}`.
    fn url_template(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            Self::CartoLight => "https://a.basemaps.cartocdn.com/light_all/{z}/{x}/{y}.png",
            Self::CartoDark => "https://a.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png",
            Self::Satellite => {
                "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"
            }
        }
    }

    /// The credit this provider's terms of use ask for.
    fn attribution(self) -> &'static str {
        match self {
            Self::OpenStreetMap => "© OpenStreetMap contributors",
            Self::CartoLight | Self::CartoDark => "© OpenStreetMap contributors, © CARTO",
            Self::Satellite => "Esri, Maxar, Earthstar Geographics",
        }
    }

    /// How to paint labels straight onto this basemap's tiles, with no plate
    /// behind them.
    ///
    /// The two pale basemaps need dark ink and the two dark ones need light ink —
    /// there is no single colour that reads on both, which is why this is a
    /// property of the provider rather than a constant. Satellite imagery does
    /// not settle for either: snow, ocean, desert and forest can pass under one
    /// label, so its glyphs carry their own halo instead of trusting the ground.
    fn label_style(self) -> overlay::LabelStyle {
        match self {
            Self::OpenStreetMap | Self::CartoLight => overlay::LabelStyle {
                ink: Color32::BLACK,
                halo: None,
            },
            Self::CartoDark => overlay::LabelStyle {
                ink: Color32::WHITE,
                halo: None,
            },
            Self::Satellite => overlay::LabelStyle {
                ink: Color32::WHITE,
                halo: Some(Color32::from_black_alpha(200)),
            },
        }
    }

    /// The tile URL for one `(z, x, y)`.
    fn tile_url(self, z: u8, x: u32, y: u32) -> String {
        self.url_template()
            .replace("{z}", &z.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string())
    }
}

/// How often the station/event coordinates are refetched while the window is
/// open, so the map keeps up with Download/Catalog fetches unprompted.
const POLL_SECONDS: f64 = 2.0;

/// Screen points of wheel travel worth one zoom level. egui has already
/// normalized line- and page-mode wheels into points by the time this reads them,
/// so one detent of a typical mouse wheel comes out as one level either way.
const POINTS_PER_ZOOM_LEVEL: f32 = 120.0;

/// Flat colour behind the tiles, and the curtain the first view assembles behind.
const BACKDROP: Color32 = Color32::from_gray(40);

/// Zoom a double-click settles on, unless the reader is already closer in.
///
/// Chosen by looking, not computed: at 8 a station in Berlin shows Berlin and the
/// country around it — enough context to place it, not so close that orienting is
/// hard. Nothing about the data can pick this number, so it is a judgement.
const FOCUS_ZOOM: f64 = 8.0;

/// Zoom levels per second past which tiles are not requested.
///
/// Mid-flick the camera is crossing tile levels it will have left before their
/// tiles could arrive; the ancestors already on the GPU cover those frames, and
/// waiting until the glide is nearly done spends the connection on the level the
/// user is actually stopping at.
const REQUEST_ZOOM_SPEED: f64 = 2.0;

pub struct MapState {
    camera: Camera,
    /// The zoom the last frame drew, for measuring how fast the camera is moving.
    last_zoom: f64,
    source: Basemap,
    last_poll: f64,
    /// Fingerprint of the coordinates the view last auto-fitted to
    /// ([`fit_fingerprint`]), so a fit happens when the data changes and not
    /// when the user has merely panned away. `None` forces the next fit.
    fitted_key: Option<u64>,
    show_plates: bool,
    /// Qt's "Only active event" switch (map/__init__.py:242).
    only_active_event: bool,
    /// Event-marker projection used for the last coordinate request.
    last_request: Option<MapRequest>,
    tiles: TileCache,
    /// True until the window has drawn a real frame. The first fit cuts instead
    /// of easing: there is nothing on screen yet to animate away from, and easing
    /// would parade the default world view on the way to the one asked for.
    opening: bool,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            last_zoom: Camera::default().zoom(),
            source: Basemap::OpenStreetMap,
            last_poll: -1.0e9,
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
        // Re-opening starts over, so the map never flashes whatever it happened
        // to be showing when it was last closed.
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
/// Counts alone are not enough: switching between two selected events, or
/// swapping datasets with equal counts, moves the coordinates without changing
/// either count.
/// `None` while there is nothing to frame, so an empty poll never latches as fitted.
fn fit_fingerprint(d: &MapData) -> Option<u64> {
    use std::hash::Hasher;
    if d.stations.is_empty() && d.events.is_empty() {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_usize(d.stations.len());
    for s in &d.stations {
        h.write_u64(s.lat.to_bits());
        h.write_u64(s.lon.to_bits());
    }
    for e in &d.events {
        h.write_u64(e.lat.to_bits());
        h.write_u64(e.lon.to_bits());
    }
    Some(h.finish())
}

pub fn show(
    ui: &mut egui::Ui,
    st: &mut MapState,
    bridge: &Bridge,
    ctx: &egui::Context,
    state: &AppState,
) {
    let data = toolbar(ui, st, bridge, ctx, state);

    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;
    painter.rect_filled(rect, 0.0, BACKDROP);

    let Some(d) = &data else {
        // Nothing to point at yet. Holding a plain backdrop until the
        // coordinates land is what keeps opening still: otherwise the map shows
        // its default world view, pulls a screenful of tiles for ground nobody
        // asked to see, and then jumps elsewhere. `toolbar` already scheduled the
        // repaint that brings us back when the poll answers.
        overlay::notice(&painter, rect, "Loading map…");
        return;
    };

    let key = fit_fingerprint(d);
    if key.is_some() && key != st.fitted_key {
        let points = d
            .stations
            .iter()
            .map(|s| (s.lat, s.lon))
            .chain(d.events.iter().map(|e| (e.lat, e.lon)));
        if let Some(bounds) = GeoBounds::around(points) {
            if st.opening {
                st.camera.cut_to_fit(bounds, rect);
            } else {
                st.camera.fit(bounds, rect);
            }
        }
        st.fitted_key = key;
    }
    // Whether or not there was anything to frame, the window has now seen its
    // data; a later fit is a deliberate move and should ease.
    st.opening = false;

    let (now, dt) = ctx.input(|i| (i.time, f64::from(i.stable_dt)));
    // Drag and wheel are applied before the step so this frame already shows them.
    if response.dragged() {
        st.camera.pan(response.drag_delta(), rect);
    }
    apply_wheel(ui, st, rect);
    if st.camera.advance(dt, rect) {
        ctx.request_repaint();
    }
    let zoom_speed = ((st.camera.zoom() - st.last_zoom) / dt.max(1e-3)).abs();
    st.last_zoom = st.camera.zoom();

    let grid = st.camera.tile_grid(rect);
    st.tiles.absorb_ready(ctx, bridge, now);
    if st.tiles.paint(&painter, &grid, now) {
        ctx.request_repaint();
    }
    if zoom_speed < REQUEST_ZOOM_SPEED {
        let source = st.source;
        bridge.request_tiles(st.tiles.wanted(&grid, |z, x, y| source.tile_url(z, x, y)));
    }
    st.tiles.evict_stale(bridge);

    let project = overlay::projector(st.camera, rect);
    // One hit test feeds the hover styling, the pointer shape, and the
    // double-click, so what lights up under the pointer is exactly what a
    // double-click acts on.
    let hit = response
        .hover_pos()
        .and_then(|pointer| overlay::hit_test(d, pointer, &project));
    if hit.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.double_clicked()
        && let Some(target) = hit.and_then(|hit| hit.position(d))
    {
        st.camera.focus_on(target, FOCUS_ZOOM, rect);
    }

    if st.show_plates {
        overlay::plate_boundaries(&painter, d, &project);
    }
    overlay::stations(&painter, d, rect, st.source.label_style(), hit, &project);
    overlay::events(&painter, d, rect, hit, &project);

    overlay::footer(&painter, rect, st.source.attribution());

    let levels = overlay::zoom_buttons(ui, rect);
    if levels != 0.0 {
        // No focus: the buttons zoom the middle of the view, since there is no
        // gesture position to zoom towards.
        st.camera.nudge_zoom(levels, None, rect);
    }
}

/// The controls above the map, returning the coordinates this frame will draw.
fn toolbar(
    ui: &mut egui::Ui,
    st: &mut MapState,
    bridge: &Bridge,
    ctx: &egui::Context,
    state: &AppState,
) -> Option<MapData> {
    let previous_source = st.source;
    let mut refresh = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Basemap")
            .selected_text(st.source.name())
            .show_ui(ui, |ui| {
                for basemap in Basemap::iter() {
                    ui.selectable_value(&mut st.source, basemap, basemap.name());
                }
            });
        if ui.button("Refresh data").clicked() {
            refresh = true;
        }
        if ui.button("Fit").clicked() {
            st.fitted_key = None;
        }
        ui.checkbox(&mut st.show_plates, "Plate boundaries");
        refresh |= ui
            .checkbox(&mut st.only_active_event, "Only active event")
            .changed();
        ui.label(format!("zoom {:.1}", st.camera.zoom()));
    });

    // Both tile caches are keyed by (z, x, y) with no provider, so a new basemap
    // only reaches keys that were never fetched. Drop both to refetch every
    // visible tile from the provider now selected.
    if st.source != previous_source {
        st.tiles.clear();
        bridge.clear_tiles();
    }

    let request = map_request(state, st.only_active_event);
    if st.last_request.as_ref() != Some(&request) {
        refresh = true;
        st.last_request = Some(request.clone());
    }
    refresh |= !bridge.map_request_is_current(&request);
    let data = bridge.map_data();
    let now = ctx.input(|i| i.time);
    if refresh || now - st.last_poll > POLL_SECONDS {
        let _submission = bridge.fetch_map_data(request);
        st.last_poll = now;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(POLL_SECONDS));
    data
}

/// Selects the event-marker projection Qt hands to its map snuffling.
fn map_request(state: &AppState, only_active_event: bool) -> MapRequest {
    let any_selected = state.markers.iter().any(|marker| marker.selected);
    let active_event = state
        .markers
        .iter()
        .find(|marker| marker.is_event && marker.active);
    let mut event_hashes = Vec::new();
    for marker in &state.markers {
        let included = marker.is_event
            && if only_active_event {
                marker.active
            } else if any_selected {
                marker.selected
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
fn apply_wheel(ui: &egui::Ui, st: &mut MapState, rect: Rect) {
    // `Ui::rect_contains_pointer` rather than the response's own hover flag: it
    // asks whether the pointer is over this rectangle in this window, without
    // caring that the `+`/`−` buttons are stacked on part of it. Scrolling over
    // the buttons should still zoom the map they sit on.
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let (scroll, pinch, cursor) = ui.input(|i| {
        (
            i.smooth_scroll_delta.y,
            i.zoom_delta(),
            i.pointer.hover_pos(),
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
    st.camera.nudge_zoom(levels, cursor, rect);
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

        for marker in &mut state.markers {
            marker.selected = marker.event_hash.as_deref() == Some("outside");
        }
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
