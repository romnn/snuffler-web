//! What is painted on top of the basemap: plate boundaries, stations, events, and
//! the small furniture in the corners.
//!
//! That furniture is the zoom buttons, the gesture hint, and the provider
//! attribution the tile sources ask for.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, Ui, Vec2, vec2};

use snuffler_web_core::bridge::MapData;
use snuffler_web_render::map::{Camera, World};

/// Side of one square zoom button.
const BUTTON_SIZE_PX: f32 = 26.0;

/// Inset of the corner furniture from the map's edges.
const MARGIN_PX: f32 = 8.0;

/// How opaque the footer pills are.
///
/// Dimmed, because they sit over the map permanently and are there to be read when
/// looked for, not to compete with the stations and events above them.
const FOOTER_ALPHA: f32 = 0.75;

/// Markers this far outside the viewport are still drawn, so one sitting on the
/// edge keeps its label instead of vanishing a few points early.
const CULL_SLACK_PX: f32 = 96.0;

/// How close the pointer has to be to a station triangle to count as over it.
///
/// Comfortably larger than the triangle, because these are small targets and a
/// double-click that misses by a point is worse than one that is slightly loose.
const STATION_REACH_PX: f32 = 10.0;

/// The same slack around an event's circle, on top of its own radius.
const EVENT_REACH_PX: f32 = 3.0;

/// How much a hovered station's triangle grows, so the pointer being on target is
/// obvious without the marker jumping somewhere else.
const HOVER_GROWTH: f32 = 1.3;

/// Gap between a station's position and the start of its label.
///
/// Fixed rather than scaled with [`HOVER_GROWTH`], so hovering cannot nudge the
/// text sideways: a label that shifts under the pointer reads as a glitch.
/// Wide enough to clear the grown triangle as well as the resting one.
const LABEL_GAP_PX: f32 = 9.0;

/// A station or event the pointer is over.
///
/// Indices into [`MapData`] rather than copies of the coordinates: the caller
/// hands the same data back when it wants the position, and nothing can go stale
/// between the hit test and the frame it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// `data.stations[_]`.
    Station(usize),
    /// `data.events[_]`.
    Event(usize),
}

impl Hit {
    /// Where the hit marker sits, for the camera to move onto.
    #[must_use]
    pub fn position(self, data: &MapData) -> Option<World> {
        match self {
            Self::Station(index) => data
                .stations
                .get(index)
                .map(|station| World::from_lat_lon(station.lat, station.lon)),
            Self::Event(index) => data
                .events
                .get(index)
                .map(|event| World::from_lat_lon(event.lat, event.lon)),
        }
    }
}

/// How event symbols are sized and coloured, which depends on the whole set: a
/// radius from the magnitude normalised against the largest, and a colour from
/// the depth against the deepest.
struct EventScale {
    max_magnitude: f64,
    max_depth_m: f64,
}

impl EventScale {
    fn of(data: &MapData) -> Self {
        Self {
            max_magnitude: data
                .events
                .iter()
                .map(|event| event.magnitude.max(0.0))
                .fold(0.0_f64, f64::max),
            max_depth_m: data
                .events
                .iter()
                .map(|event| event.depth)
                .fold(10.0_f64, f64::max),
        }
    }

    /// The symbol radius in screen points for a magnitude.
    fn radius(&self, magnitude: f64) -> f32 {
        let radius = if self.max_magnitude > 0.0 {
            2.0 + (2.0 * magnitude.max(0.0) / self.max_magnitude).exp()
        } else {
            8.0
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the magnitude curve yields a radius of a few screen points"
        )]
        let points = radius as f32;
        points
    }

    /// A depth in metres as `0..=1`, shallow to deep.
    fn depth_fraction(&self, depth_m: f64) -> f64 {
        if self.max_depth_m > 10.0 {
            ((depth_m - 10.0) / (self.max_depth_m - 10.0)).clamp(0.0, 1.0)
        } else {
            0.5
        }
    }
}

/// Whichever marker `pointer` is over, if any.
///
/// Events win ties because they are painted over the stations — the hit has to
/// agree with what the eye picks out, or a double-click lands on the wrong thing.
#[must_use]
pub fn hit_test(data: &MapData, pointer: Pos2, project: &impl Fn(f64, f64) -> Pos2) -> Option<Hit> {
    let scale = EventScale::of(data);
    let event = data.events.iter().position(|event| {
        pointer.distance(project(event.lat, event.lon))
            <= scale.radius(event.magnitude) + EVENT_REACH_PX
    });
    if let Some(index) = event {
        return Some(Hit::Event(index));
    }
    data.stations
        .iter()
        .position(|station| pointer.distance(project(station.lat, station.lon)) <= STATION_REACH_PX)
        .map(Hit::Station)
}

/// The ring drawn around whatever the pointer is over.
///
/// Dark under light so it reads on any basemap, and on the red triangles and
/// blue-to-red circles alike.
fn hover_ring(painter: &egui::Painter, at: Pos2, radius: f32) {
    painter.circle_stroke(
        at,
        radius,
        Stroke::new(3.0_f32, Color32::from_black_alpha(150)),
    );
    painter.circle_stroke(at, radius, Stroke::new(1.5_f32, Color32::WHITE));
}

/// Type size of a station label.
const LABEL_SIZE_PT: f32 = 11.0;

/// Where a haloed label is stamped around its own position, in screen points.
///
/// Eight directions rather than four: at the corners a four-point cross leaves
/// the diagonal strokes of a glyph touching the imagery directly, which is
/// exactly where a thin letter disappears.
const HALO_RING: [Vec2; 8] = [
    vec2(-1.0, -1.0),
    vec2(0.0, -1.0),
    vec2(1.0, -1.0),
    vec2(-1.0, 0.0),
    vec2(1.0, 0.0),
    vec2(-1.0, 1.0),
    vec2(0.0, 1.0),
    vec2(1.0, 1.0),
];

/// How a label has to be painted to stay readable on a given basemap.
#[derive(Clone, Copy, Debug)]
pub struct LabelStyle {
    /// Colour of the glyphs themselves.
    pub ink: Color32,
    /// Colour stamped around the glyphs, for imagery too varied for flat ink to
    /// win against.
    ///
    /// `None` on a basemap with a consistent enough tone.
    pub halo: Option<Color32>,
}

/// Bird (2003) tectonic plate boundaries, the map snuffling's signature overlay.
///
/// Polylines are cut where they cross the antimeridian; without that, a boundary
/// that leaves one edge and re-enters at the other is drawn as a single stroke
/// straight back across the whole world.
pub fn plate_boundaries(
    painter: &egui::Painter,
    data: &MapData,
    project: impl Fn(f64, f64) -> Pos2,
) {
    let stroke = Stroke::new(
        1.0_f32,
        Color32::from_rgba_unmultiplied(0xFF, 0xB0, 0x30, 200),
    );
    for boundary in &data.plate_boundaries {
        let mut segment: Vec<Pos2> = Vec::new();
        let mut previous_lon = f64::NAN;
        for (lon, lat) in boundary.lons.iter().zip(boundary.lats.iter()) {
            let wrapped = !previous_lon.is_nan() && (lon - previous_lon).abs() > 180.0;
            if wrapped {
                if segment.len() >= 2 {
                    painter.add(Shape::line(std::mem::take(&mut segment), stroke));
                }
                segment.clear();
            }
            previous_lon = *lon;
            segment.push(project(*lat, *lon));
        }
        if segment.len() >= 2 {
            painter.add(Shape::line(segment, stroke));
        }
    }
}

/// Station triangles with their NSLC labels.
///
/// The labels sit straight on the tiles with nothing behind them, so `style` has
/// to come from the basemap in use (see `Basemap::label_style`).
pub fn stations(
    painter: &egui::Painter,
    data: &MapData,
    rect: Rect,
    style: LabelStyle,
    hit: Option<Hit>,
    project: impl Fn(f64, f64) -> Pos2,
) {
    let visible = rect.expand(CULL_SLACK_PX);
    for (index, station) in data.stations.iter().enumerate() {
        let at = project(station.lat, station.lon);
        if !visible.contains(at) {
            continue;
        }
        let hovered = hit == Some(Hit::Station(index));
        if hovered {
            hover_ring(painter, at, STATION_REACH_PX);
        }
        let grow = if hovered { HOVER_GROWTH } else { 1.0 };
        painter.add(Shape::convex_polygon(
            vec![
                egui::pos2(at.x, at.y - 7.0 * grow),
                egui::pos2(at.x - 6.0 * grow, at.y + 4.0 * grow),
                egui::pos2(at.x + 6.0 * grow, at.y + 4.0 * grow),
            ],
            Color32::from_rgb(0xD0, 0x20, 0x20),
            Stroke::new(1.5_f32, Color32::BLACK),
        ));
        label(
            painter,
            egui::pos2(at.x + LABEL_GAP_PX, at.y),
            &station.label,
            style,
        );
    }
}

/// Draws one label, left-aligned on `at`, haloed if the basemap asks for it.
///
/// The halo is the glyphs themselves re-stamped around their own position rather
/// than a plate behind them: a plate would hide the ground the station sits on,
/// which on satellite imagery is often the reason to be looking.
fn label(painter: &egui::Painter, at: Pos2, text: &str, style: LabelStyle) {
    let font = FontId::proportional(LABEL_SIZE_PT);
    // Laid out once and stamped many times; egui caches galleys, so the repeats
    // cost a draw call each rather than a re-layout.
    let ink = painter.layout_no_wrap(text.to_owned(), font.clone(), style.ink);
    let origin = Align2::LEFT_CENTER.anchor_size(at, ink.size()).min;
    if let Some(halo) = style.halo {
        let backing = painter.layout_no_wrap(text.to_owned(), font, halo);
        for offset in HALO_RING {
            painter.galley(origin + offset, backing.clone(), halo);
        }
    }
    painter.galley(origin, ink, style.ink);
}

/// Event circles, plus the hover popup for whichever one is under the pointer.
///
/// Magnitude→radius and depth→colour are faithful to the Qt map snuffling's
/// *rendered* values: magmin=0, depthmin=10 m and magshift=0, which is how
/// `map_osm.html` actually feeds `map_util.js`.
/// The radius is 2+exp(2·mag/magmax), and the depth ramps blue (shallow) to red
/// (deep) over (depth−10)/(depthmax−10).
pub fn events(
    painter: &egui::Painter,
    data: &MapData,
    rect: Rect,
    hit: Option<Hit>,
    project: impl Fn(f64, f64) -> Pos2,
) {
    let visible = rect.expand(CULL_SLACK_PX);
    let scale = EventScale::of(data);

    for (index, event) in data.events.iter().enumerate() {
        let at = project(event.lat, event.lon);
        if !visible.contains(at) {
            continue;
        }
        let radius = scale.radius(event.magnitude);
        let depth = scale.depth_fraction(event.depth);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "depth is clamped to 0..=1, so both channels land inside 0..=255"
        )]
        let (red, blue) = ((255.0 * depth) as u8, (255.0 * (1.0 - depth)) as u8);
        let hovered = hit == Some(Hit::Event(index));
        if hovered {
            hover_ring(painter, at, radius + EVENT_REACH_PX);
        }
        painter.circle(
            at,
            radius,
            Color32::from_rgba_unmultiplied(red, 0, blue, 128),
            Stroke::new(1.0_f32, Color32::WHITE),
        );

        if hovered {
            let name = if event.name.is_empty() {
                "(event)"
            } else {
                &event.name
            };
            let text = format!(
                "{name}  M{:.1}\n{}\ndepth {:.1} km",
                event.magnitude,
                event.time,
                event.depth / 1000.0,
            );
            let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), Color32::WHITE);
            let pad = egui::vec2(6.0, 4.0);
            let origin = at + egui::vec2(radius + 8.0, -galley.size().y * 0.5 - pad.y);
            painter.rect_filled(
                Rect::from_min_size(origin, galley.size() + pad * 2.0),
                3.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 220),
            );
            painter.galley(origin + pad, galley, Color32::WHITE);
        }
    }
}

/// The `+` / `−` buttons in the map's top-right corner, returning the zoom levels
/// they asked for this frame.
///
/// Drawn after the map so the buttons land on top of it.
pub fn zoom_buttons(ui: &mut Ui, rect: Rect) -> f64 {
    let mut levels = 0.0;
    for (index, (label, step, tip)) in [("+", 1.0, "Zoom in"), ("−", -1.0, "Zoom out")]
        .into_iter()
        .enumerate()
    {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the index of one of two buttons converts exactly"
        )]
        let row = index as f32;
        let slot = Rect::from_min_size(
            egui::pos2(
                rect.right() - MARGIN_PX - BUTTON_SIZE_PX,
                row.mul_add(BUTTON_SIZE_PX + 1.0, rect.top() + MARGIN_PX),
            ),
            egui::Vec2::splat(BUTTON_SIZE_PX),
        );
        let clicked = ui
            .put(
                slot,
                egui::Button::new(egui::RichText::new(label).size(16.0).strong())
                    .corner_radius(CornerRadius::same(4)),
            )
            .on_hover_text(tip)
            .clicked();
        if clicked {
            levels += step;
        }
    }
    levels
}

/// The bottom strip: the gesture hint on the left, provider attribution on the
/// right.
///
/// Both are painted, not laid out as widgets, so they never take a click away
/// from the map.
///
/// The hint stays up for the life of the window rather than retiring once the
/// user has zoomed: the gestures it names are not all discoverable, and the map
/// is visited rarely enough that having done it once is no guarantee of
/// remembering next time.
pub fn footer(painter: &egui::Painter, rect: Rect, attribution: &str) {
    let baseline = rect.bottom() - MARGIN_PX;
    pill(
        painter,
        egui::pos2(rect.left() + MARGIN_PX, baseline),
        Align2::LEFT_BOTTOM,
        "Scroll to zoom  ·  drag to pan  ·  double-click a station or event",
        FOOTER_ALPHA,
    );
    pill(
        painter,
        egui::pos2(rect.right() - MARGIN_PX, baseline),
        Align2::RIGHT_BOTTOM,
        attribution,
        FOOTER_ALPHA,
    );
}

/// A centred message on its own plate, for when there is nothing else to show.
///
/// Plated rather than plain text because it has to stay readable over a pale
/// basemap, a dark one, and the flat background before any tile has arrived.
pub fn notice(painter: &egui::Painter, rect: Rect, text: &str) {
    pill(painter, rect.center(), Align2::CENTER_CENTER, text, 1.0);
}

/// Where the camera puts a geographic coordinate on screen, as the closure the
/// overlay painters project through.
pub fn projector(camera: Camera, rect: Rect) -> impl Fn(f64, f64) -> Pos2 {
    move |lat, lon| camera.world_to_screen(rect, World::from_lat_lon(lat, lon))
}

/// A rounded, dimmed label anchored at a corner.
fn pill(painter: &egui::Painter, at: Pos2, align: Align2, text: &str, alpha: f32) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(11.0),
        Color32::WHITE.gamma_multiply(alpha),
    );
    let pad = egui::vec2(7.0, 4.0);
    let box_rect = align.anchor_size(at, galley.size() + pad * 2.0);
    painter.rect_filled(
        box_rect,
        5.0,
        Color32::from_rgba_unmultiplied(0, 0, 0, 165).gamma_multiply(alpha),
    );
    painter.galley(
        box_rect.min + pad,
        galley,
        Color32::WHITE.gamma_multiply(alpha),
    );
}
