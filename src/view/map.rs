//! A slippy-map view: OSM / Carto / satellite raster tiles with station and
//! event overlays. The browser fetches + decodes the tiles (CORS-clean
//! providers) and pyrocko supplies the station/event coordinates; Rust only does
//! the Web-Mercator projection and drawing — the same kind of rendering geometry
//! as the time axis, not seismological computation.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "Web-Mercator projection casts between f64 lat/lon, f32 pixels and \
              integer tile/pixel indices; inherent to the map rendering geometry"
)]

use std::collections::HashMap;
use std::f64::consts::PI;

use egui::{
    Align2, Color32, ColorImage, FontId, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions,
};

use crate::bridge::{Bridge, MapData};

const TILE: f64 = 256.0;

/// (name, url template). `{z}/{x}/{y}` are substituted; ESRI uses `{z}/{y}/{x}`.
const SOURCES: [(&str, &str); 4] = [
    (
        "OpenStreetMap",
        "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
    ),
    (
        "Carto light",
        "https://a.basemaps.cartocdn.com/light_all/{z}/{x}/{y}.png",
    ),
    (
        "Carto dark",
        "https://a.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png",
    ),
    (
        "Satellite",
        "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
    ),
];

pub struct MapState {
    center_lat: f64,
    center_lon: f64,
    zoom: u8,
    source: usize,
    last_poll: f64,
    fitted_counts: (usize, usize),
    scroll_acc: f32,
    show_plates: bool,
    tex: HashMap<(u8, u32, u32), TextureHandle>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            center_lat: 20.0,
            center_lon: 0.0,
            zoom: 2,
            source: 0,
            last_poll: -1.0e9,
            fitted_counts: (usize::MAX, usize::MAX),
            scroll_acc: 0.0,
            show_plates: true,
            tex: HashMap::new(),
        }
    }
}

fn lon_to_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * (1u64 << z) as f64
}
fn lat_to_y(lat: f64, z: u8) -> f64 {
    let r = lat.clamp(-85.05, 85.05).to_radians();
    (1.0 - (r.tan() + 1.0 / r.cos()).ln() / PI) / 2.0 * (1u64 << z) as f64
}
fn x_to_lon(x: f64, z: u8) -> f64 {
    x / (1u64 << z) as f64 * 360.0 - 180.0
}
fn y_to_lat(y: f64, z: u8) -> f64 {
    PI.mul_add(-2.0 * y / (1u64 << z) as f64, PI)
        .sinh()
        .atan()
        .to_degrees()
}

fn tile_url(src: usize, z: u8, x: u32, y: u32) -> String {
    let template = SOURCES.get(src).map_or("", |s| s.1);
    template
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string())
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive draw/update routine; splitting would obscure the control flow"
)]
pub fn show(ui: &mut egui::Ui, st: &mut MapState, bridge: &Bridge, ctx: &egui::Context) {
    // Zoom: accumulate raw wheel scroll and step one zoom level per notch, so a
    // wheel flick doesn't blow through several levels at once.
    const ZOOM_NOTCH: f32 = 50.0;
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Basemap")
            .selected_text(SOURCES.get(st.source).map_or("", |s| s.0))
            .show_ui(ui, |ui| {
                for (i, (name, _)) in SOURCES.iter().enumerate() {
                    ui.selectable_value(&mut st.source, i, *name);
                }
            });
        if ui.button("Refresh data").clicked() {
            bridge.fetch_map_data(ctx);
        }
        if ui.button("Fit").clicked() {
            st.fitted_counts = (usize::MAX, usize::MAX);
        }
        ui.checkbox(&mut st.show_plates, "Plate boundaries");
        ui.label(format!("zoom {}", st.zoom));
    });

    // While the window is open, refresh the station/event coords periodically so
    // the map keeps up with Download/Catalog fetches without a manual refresh.
    let data = bridge.map_data();
    let now = ctx.input(|i| i.time);
    if now - st.last_poll > 2.0 {
        bridge.fetch_map_data(ctx);
        st.last_poll = now;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs(2));

    // Auto-fit only when the data *content* changes (so user pan/zoom is kept).
    if let Some(d) = &data {
        let counts = (d.stations.len(), d.events.len());
        if counts != st.fitted_counts && counts != (0, 0) {
            fit_to_data(st, ui.available_size(), d);
            st.fitted_counts = counts;
        }
    }

    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, Color32::from_gray(40));

    let z = st.zoom;
    let n = 1i64 << z;
    let cwx = lon_to_x(st.center_lon, z) * TILE;
    let cwy = lat_to_y(st.center_lat, z) * TILE;
    let tlx = cwx - f64::from(rect.width()) / 2.0;
    let tly = cwy - f64::from(rect.height()) / 2.0;
    let to_screen = |wx: f64, wy: f64| {
        egui::pos2(
            (wx - tlx) as f32 + rect.left(),
            (wy - tly) as f32 + rect.top(),
        )
    };
    let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

    let tx0 = (tlx / TILE).floor() as i64;
    let ty0 = (tly / TILE).floor() as i64;
    let tx1 = ((tlx + f64::from(rect.width())) / TILE).floor() as i64;
    let ty1 = ((tly + f64::from(rect.height())) / TILE).floor() as i64;
    for tyi in ty0..=ty1 {
        if tyi < 0 || tyi >= n {
            continue;
        }
        let y = tyi as u32;
        for txi in tx0..=tx1 {
            let xw = txi.rem_euclid(n) as u32; // wrap around the antimeridian
            let key = (z, xw, y);
            let pos = to_screen(txi as f64 * TILE, tyi as f64 * TILE);
            let trect = Rect::from_min_size(pos, egui::vec2(TILE as f32, TILE as f32));
            if let Some(tex) = st.tex.get(&key) {
                painter.image(tex.id(), trect, full, Color32::WHITE);
            } else if let Some((w, h, rgba)) = bridge.take_tile_rgba(z, xw, y) {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                let tex =
                    ctx.load_texture(format!("tile{z}_{xw}_{y}"), img, TextureOptions::LINEAR);
                painter.image(tex.id(), trect, full, Color32::WHITE);
                st.tex.insert(key, tex);
            } else {
                bridge.request_tile(ctx, z, xw, y, tile_url(st.source, z, xw, y));
            }
        }
    }

    if let Some(d) = &data {
        // Bird (2003) tectonic plate boundaries under the markers (the map
        // snuffling's signature overlay). Polylines are split at antimeridian
        // jumps so a wrap doesn't draw a line across the whole map.
        if st.show_plates {
            let stroke = Stroke::new(
                1.0_f32,
                Color32::from_rgba_unmultiplied(0xFF, 0xB0, 0x30, 200),
            );
            for pb in &d.plate_boundaries {
                let mut seg: Vec<egui::Pos2> = Vec::new();
                let mut prev_lon = f64::NAN;
                for (lon, lat) in pb.lons.iter().zip(pb.lats.iter()) {
                    if !prev_lon.is_nan() && (lon - prev_lon).abs() > 180.0 && seg.len() >= 2 {
                        painter.add(Shape::line(std::mem::take(&mut seg), stroke));
                    }
                    if !prev_lon.is_nan() && (lon - prev_lon).abs() > 180.0 {
                        seg.clear();
                    }
                    prev_lon = *lon;
                    seg.push(to_screen(
                        lon_to_x(*lon, z) * TILE,
                        lat_to_y(*lat, z) * TILE,
                    ));
                }
                if seg.len() >= 2 {
                    painter.add(Shape::line(seg, stroke));
                }
            }
        }
        for s in &d.stations {
            let p = to_screen(lon_to_x(s.lon, z) * TILE, lat_to_y(s.lat, z) * TILE);
            if rect.contains(p) {
                painter.add(Shape::convex_polygon(
                    vec![
                        egui::pos2(p.x, p.y - 7.0),
                        egui::pos2(p.x - 6.0, p.y + 4.0),
                        egui::pos2(p.x + 6.0, p.y + 4.0),
                    ],
                    Color32::from_rgb(0xD0, 0x20, 0x20),
                    Stroke::new(1.5_f32, Color32::BLACK),
                ));
                painter.text(
                    egui::pos2(p.x + 7.0, p.y),
                    Align2::LEFT_CENTER,
                    &s.label,
                    FontId::proportional(11.0),
                    Color32::WHITE,
                );
            }
        }
        // Magnitude→radius and depth→colour, faithful to the Qt map snuffling's
        // *rendered* values (magmin=0, depthmin=10 m, magshift=0 — how map_osm.html
        // actually feeds map_util.js): radius = 2+exp(2·mag/magmax), depth ramps
        // blue(shallow)→red(deep) over (depth−10)/(depthmax−10).
        let magmax = d
            .events
            .iter()
            .map(|e| e.mag.max(0.0))
            .fold(0.0_f64, f64::max);
        let depthmax = d.events.iter().map(|e| e.depth).fold(10.0_f64, f64::max);
        let hover = resp.hover_pos();
        for e in &d.events {
            let p = to_screen(lon_to_x(e.lon, z) * TILE, lat_to_y(e.lat, z) * TILE);
            if !rect.contains(p) {
                continue;
            }
            let radius = if magmax > 0.0 {
                2.0 + (2.0 * e.mag.max(0.0) / magmax).exp()
            } else {
                8.0
            } as f32;
            let scale = if depthmax > 10.0 {
                ((e.depth - 10.0) / (depthmax - 10.0)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let cr = (255.0 * scale) as u8;
            let cb = (255.0 * (1.0 - scale)) as u8;
            painter.circle(
                p,
                radius,
                Color32::from_rgba_unmultiplied(cr, 0, cb, 128),
                Stroke::new(1.0_f32, Color32::WHITE),
            );
            // Hover popup: name / magnitude / time / depth (km), like the ol popups.
            if let Some(hp) = hover
                && hp.distance(p) <= radius + 3.0
            {
                let name = if e.name.is_empty() {
                    "(event)"
                } else {
                    &e.name
                };
                let txt = format!(
                    "{name}  M{:.1}\n{}\ndepth {:.1} km",
                    e.mag,
                    e.time,
                    e.depth / 1000.0,
                );
                let galley =
                    painter.layout_no_wrap(txt, FontId::proportional(11.0), Color32::WHITE);
                let pad = egui::vec2(6.0, 4.0);
                let origin = p + egui::vec2(10.0, -galley.size().y * 0.5 - pad.y);
                let box_rect = Rect::from_min_size(origin, galley.size() + pad * 2.0);
                painter.rect_filled(box_rect, 3.0, Color32::from_rgba_unmultiplied(0, 0, 0, 220));
                painter.galley(origin + pad, galley, Color32::WHITE);
            }
        }
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Loading map…",
            FontId::proportional(14.0),
            Color32::WHITE,
        );
    }

    if resp.dragged() {
        let d = resp.drag_delta();
        st.center_lon = x_to_lon((cwx - f64::from(d.x)) / TILE, z);
        st.center_lat = y_to_lat((cwy - f64::from(d.y)) / TILE, z);
    }
    if resp.hovered() {
        st.scroll_acc += ui.input(|i| i.smooth_scroll_delta.y);
    } else {
        st.scroll_acc = 0.0;
    }
    let step = if st.scroll_acc >= ZOOM_NOTCH {
        st.scroll_acc = 0.0;
        1
    } else if st.scroll_acc <= -ZOOM_NOTCH {
        st.scroll_acc = 0.0;
        -1
    } else {
        0
    };
    if step != 0
        && let Some(cur) = resp.hover_pos()
    {
        let lon = x_to_lon((tlx + f64::from(cur.x - rect.left())) / TILE, z);
        let lat = y_to_lat((tly + f64::from(cur.y - rect.top())) / TILE, z);
        let nz = (i32::from(z) + step).clamp(1, 18) as u8;
        if nz != z {
            let off = cur - rect.center();
            st.center_lon = x_to_lon((lon_to_x(lon, nz) * TILE - f64::from(off.x)) / TILE, nz);
            st.center_lat = y_to_lat((lat_to_y(lat, nz) * TILE - f64::from(off.y)) / TILE, nz);
            st.zoom = nz;
        }
    }
}

fn fit_to_data(st: &mut MapState, size: egui::Vec2, d: &MapData) {
    let (mut latmin, mut latmax) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lonmin, mut lonmax) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut acc = |lat: f64, lon: f64| {
        latmin = latmin.min(lat);
        latmax = latmax.max(lat);
        lonmin = lonmin.min(lon);
        lonmax = lonmax.max(lon);
    };
    for s in &d.stations {
        acc(s.lat, s.lon);
    }
    for e in &d.events {
        acc(e.lat, e.lon);
    }
    if !latmin.is_finite() {
        return;
    }
    st.center_lat = f64::midpoint(latmin, latmax);
    st.center_lon = f64::midpoint(lonmin, lonmax);
    if (latmax - latmin).abs() < 1e-6 && (lonmax - lonmin).abs() < 1e-6 {
        st.zoom = 7; // single point
        return;
    }
    let (w, h) = (f64::from(size.x.max(64.0)), f64::from(size.y.max(64.0)));
    st.zoom = 2;
    for z in (2u8..=16).rev() {
        let dw = (lon_to_x(lonmax, z) - lon_to_x(lonmin, z)).abs() * TILE;
        let dh = (lat_to_y(latmin, z) - lat_to_y(latmax, z)).abs() * TILE;
        if dw <= w * 0.9 && dh <= h * 0.9 {
            st.zoom = z;
            break;
        }
    }
}
