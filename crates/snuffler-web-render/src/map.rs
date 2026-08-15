//! Slippy-map camera geometry: the Web-Mercator projection, a spring-eased zoom
//! that keeps a chosen point pinned under the cursor, world bounds that forbid
//! wrap-around, and the tile grid a frame needs to paint.
//!
//! Toolkit-neutral and free of I/O, so it runs (and is tested) on the host while
//! the egui view only supplies a viewport rectangle and a pointer position.
use std::ops::RangeInclusive;

use emath::{Pos2, Rect, Vec2, pos2};

/// Side length of one raster tile, in map pixels at its own zoom level.
pub const TILE_SIZE: f64 = 256.0;

/// Latitude where the Web-Mercator projection runs away to infinity. Cutting the
/// world here is what makes it exactly square, and every tile server does it.
pub const MAX_LATITUDE: f64 = 85.051_128_78;

/// Deepest zoom level the raster basemaps serve.
pub const MAX_ZOOM: f64 = 18.0;

/// How fast the zoom converges on its target, in rad/s. A critically damped
/// spring is ~99% of the way there after `5 / omega` seconds, so this is a
/// ~310 ms glide: long enough to read as motion, short enough to feel direct.
const ZOOM_OMEGA: f64 = 16.0;

/// Same, for the recentring that `fit` asks for. Slower than the zoom so a fit
/// reads as one camera move rather than two racing ones.
const PAN_OMEGA: f64 = 11.0;

/// Zoom levels below which the zoom spring is considered parked. A twentieth of a
/// level is well under one pixel of tile scale.
const ZOOM_EPSILON: f64 = 0.001;

/// A backgrounded tab hands back one enormous frame delta on return; stepping the
/// springs by it would teleport the camera. Cap the step instead.
const MAX_STEP_SECONDS: f64 = 1.0 / 15.0;

/// Fraction of the viewport a [`Camera::fit`] fills, leaving a margin so markers
/// at the extremes are not painted half off the edge.
const FIT_FILL: f64 = 0.82;

/// Ceiling for a fitted zoom, so a single station (whose bounds have no extent)
/// lands on a recognizable region rather than on rooftops.
const FIT_MAX_ZOOM: f64 = 9.0;

/// A point in normalized Web-Mercator world space: `0..=1` on both axes, with
/// `(0, 0)` at the north-west corner (180° W, [`MAX_LATITUDE`] N).
///
/// Normalized rather than per-zoom-level pixels on purpose — a world position
/// means the same thing at every zoom, which is exactly what an anchored zoom
/// needs to hold on to while the scale under it changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct World {
    /// Eastings, `0.0` at 180° W and `1.0` at 180° E.
    pub x: f64,
    /// Northings, `0.0` at the north edge and `1.0` at the south edge.
    pub y: f64,
}

impl World {
    /// Projects geographic coordinates, clamping to the square Mercator world.
    #[must_use]
    pub fn from_lat_lon(lat: f64, lon: f64) -> Self {
        let radians = lat.clamp(-MAX_LATITUDE, MAX_LATITUDE).to_radians();
        Self {
            x: ((lon.clamp(-180.0, 180.0) + 180.0) / 360.0).clamp(0.0, 1.0),
            y: ((1.0 - (radians.tan() + 1.0 / radians.cos()).ln() / std::f64::consts::PI) / 2.0)
                .clamp(0.0, 1.0),
        }
    }

    /// Inverts [`World::from_lat_lon`], returning `(lat, lon)` in degrees.
    #[must_use]
    pub fn to_lat_lon(self) -> (f64, f64) {
        let lat = std::f64::consts::PI
            .mul_add(-2.0 * self.y, std::f64::consts::PI)
            .sinh()
            .atan()
            .to_degrees();
        (lat, self.x.mul_add(360.0, -180.0))
    }
}

/// A critically damped spring.
///
/// Critically damped is the whole point: it eases *in* from rest and *out* into
/// its target without ever overshooting, and re-aiming it mid-flight carries the
/// current velocity over instead of restarting — which is what a wheel does when
/// it fires three notches in as many frames. The step below is the closed-form
/// solution, so it is exact and stable at any frame rate rather than an Euler
/// integration that blows up on a long frame.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    value: f64,
    velocity: f64,
    target: f64,
    omega: f64,
}

impl Spring {
    /// A spring parked at `value`, converging at `omega` rad/s.
    #[must_use]
    pub fn new(value: f64, omega: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            omega,
        }
    }

    /// Where the spring is right now — what a frame draws.
    #[must_use]
    pub fn value(self) -> f64 {
        self.value
    }

    /// Where it is heading — what the next input should be applied to.
    #[must_use]
    pub fn target(self) -> f64 {
        self.target
    }

    /// Re-aims the spring without disturbing its current value or velocity.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
    }

    /// Teleports the spring: value, target, and velocity all agree at `value`.
    pub fn snap_to(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Confines the current value to `lo..=hi`, killing the velocity that was
    /// pushing past the wall so the spring rests against it instead of shoving.
    pub fn clamp_value(&mut self, lo: f64, hi: f64) {
        if self.value < lo || self.value > hi {
            self.value = self.value.clamp(lo, hi);
            self.velocity = 0.0;
        }
    }

    /// Confines the target to `lo..=hi`, leaving the value and velocity alone.
    ///
    /// Separate from [`Spring::clamp_value`] because the two are not always
    /// bounded by the same thing: where a camera may *be* now and where it may
    /// *arrive* differ whenever the scale is still moving under it.
    pub fn clamp_target(&mut self, lo: f64, hi: f64) {
        self.target = self.target.clamp(lo, hi);
    }

    /// Confines value and target alike, for a limit that does not depend on the
    /// spring's own motion.
    pub fn clamp_to(&mut self, lo: f64, hi: f64) {
        self.clamp_value(lo, hi);
        self.clamp_target(lo, hi);
    }

    /// Whether the spring has come to rest within `epsilon` of its target.
    #[must_use]
    pub fn is_settled(self, epsilon: f64) -> bool {
        (self.value - self.target).abs() <= epsilon && self.velocity.abs() <= epsilon * self.omega
    }

    /// Advances by `dt` seconds, returning whether the spring is still moving (and
    /// the frame therefore needs another repaint).
    pub fn advance(&mut self, dt: f64, epsilon: f64) -> bool {
        if self.is_settled(epsilon) {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let dt = dt.clamp(0.0, MAX_STEP_SECONDS);
        let offset = self.value - self.target;
        let decay = (-self.omega * dt).exp();
        // x(t) = (x0 + (v0 + omega*x0)*t) * e^-omega*t, for x measured from target.
        let slope = self.omega.mul_add(offset, self.velocity);
        self.value = self.target + slope.mul_add(dt, offset) * decay;
        self.velocity = self.omega.mul_add(-slope * dt, self.velocity) * decay;
        true
    }
}

/// Cubic ease-out over `0..=1`, saturating outside it.
///
/// The spring above is for anything chasing a target that can move; this is for
/// the fixed-length fades — a tile arriving, a window revealing itself — where
/// the duration is known up front and there is nothing to re-aim at.
#[must_use]
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A world point the camera is holding on screen while the scale changes under it.
///
/// Holding it for the whole animation — not just for the frame the gesture
/// happened — is what makes the map zoom *towards* something: the centre is
/// re-derived from this every frame rather than interpolated in world space.
///
/// That distinction is the whole point. World-space interpolation looks correct
/// and is not: the scale grows exponentially while the pan shrinks linearly, so
/// their product — where the target actually appears — swings *outward* before
/// coming back. Deciding where the point should sit on screen and solving for the
/// centre inverts that, and the target simply slides in.
#[derive(Clone, Copy, Debug)]
struct Anchor {
    /// The world point being held.
    world: World,
    /// Where it sat when the gesture began, in screen points from the centre.
    from: Vec2,
    /// Where it should end up. The same spot for a wheel zoom, which pins the
    /// ground under the pointer; the middle for a focus, which brings its target
    /// home.
    to: Vec2,
    /// The zoom span the slide is paced against, so the point arrives exactly as
    /// the zoom does.
    zoom_from: f64,
    zoom_to: f64,
}

impl Anchor {
    /// Where the held point belongs right now, for a zoom part-way through.
    fn offset_at(self, zoom: f64) -> Vec2 {
        let span = self.zoom_to - self.zoom_from;
        if span.abs() < f64::EPSILON {
            return self.to;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a fraction clamped to 0..=1 becomes an f32 interpolant"
        )]
        let progress = ((zoom - self.zoom_from) / span).clamp(0.0, 1.0) as f32;
        self.from + (self.to - self.from) * progress
    }
}

/// Where the map is looking: a centre, a continuous zoom, and what both are
/// easing towards.
///
/// The zoom is continuous rather than one integer level per wheel notch, so the
/// scale between tile levels is real geometry rather than a jump; the view picks
/// the nearest integer level to fetch and scales it to fit.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    x: Spring,
    y: Spring,
    zoom: Spring,
    anchor: Option<Anchor>,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new(World::from_lat_lon(20.0, 0.0), 2.0)
    }
}

impl Camera {
    /// A camera parked at `center` and `zoom`.
    #[must_use]
    pub fn new(center: World, zoom: f64) -> Self {
        Self {
            x: Spring::new(center.x, PAN_OMEGA),
            y: Spring::new(center.y, PAN_OMEGA),
            zoom: Spring::new(zoom, ZOOM_OMEGA),
            anchor: None,
        }
    }

    /// The zoom this frame draws at — fractional while a zoom is in flight.
    #[must_use]
    pub fn zoom(self) -> f64 {
        self.zoom.value()
    }

    /// The zoom the camera is heading for; the next wheel notch adds to this, not
    /// to the animated value, so notches accumulate instead of cancelling.
    #[must_use]
    pub fn zoom_target(self) -> f64 {
        self.zoom.target()
    }

    /// The world position at the centre of the viewport.
    #[must_use]
    pub fn center(self) -> World {
        World {
            x: self.x.value(),
            y: self.y.value(),
        }
    }

    /// Side length of the whole world in screen points at the current zoom.
    #[must_use]
    pub fn world_size(self) -> f64 {
        TILE_SIZE * self.zoom.value().exp2()
    }

    /// The shallowest zoom at which the world still covers `viewport`.
    ///
    /// Zooming out past this is what lets a map wrap around and show the same
    /// ground twice — the second copy carrying none of the stations painted on
    /// the first. Refusing to go there is cheaper than drawing every overlay
    /// once per world copy, and matches what the data means: there is one Earth.
    #[must_use]
    pub fn min_zoom(viewport: Vec2) -> f64 {
        (f64::from(viewport.x.max(viewport.y).max(1.0)) / TILE_SIZE)
            .log2()
            .clamp(0.0, MAX_ZOOM)
    }

    /// Screen position of a world point.
    #[must_use]
    pub fn screen_of(self, viewport: Rect, world: World) -> Pos2 {
        let size = self.world_size();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an offset from the viewport centre is a screen distance, which f32 holds"
        )]
        let offset = Vec2::new(
            ((world.x - self.x.value()) * size) as f32,
            ((world.y - self.y.value()) * size) as f32,
        );
        viewport.center() + offset
    }

    /// World position under a screen point — the inverse of [`Camera::screen_of`].
    #[must_use]
    pub fn world_of(self, viewport: Rect, screen: Pos2) -> World {
        let size = self.world_size();
        let center = viewport.center();
        World {
            x: f64::from(screen.x - center.x) / size + self.x.value(),
            y: f64::from(screen.y - center.y) / size + self.y.value(),
        }
    }

    /// Aims the zoom `levels` deeper (negative: shallower).
    ///
    /// With a `focus`, the world point under it is pinned there for the whole
    /// glide — the wheel zooms towards the cursor. Without one (the `+`/`−`
    /// buttons), the viewport centre stays put.
    pub fn nudge_zoom(&mut self, levels: f64, focus: Option<Pos2>, viewport: Rect) {
        let target = (self.zoom.target() + levels).clamp(Self::min_zoom(viewport.size()), MAX_ZOOM);
        self.anchor = focus.map(|screen| Anchor {
            world: self.world_of(viewport, screen),
            // A wheel zoom pins the ground where it is: same spot start to end.
            from: screen - viewport.center(),
            to: screen - viewport.center(),
            zoom_from: self.zoom.value(),
            zoom_to: target,
        });
        self.zoom.set_target(target);
    }

    /// Drags the map by a pointer delta, one screen point for one screen point.
    pub fn pan(&mut self, delta: Vec2, viewport: Rect) {
        self.anchor = None;
        let size = self.world_size();
        self.x.snap_to(self.x.value() - f64::from(delta.x) / size);
        self.y.snap_to(self.y.value() - f64::from(delta.y) / size);
        self.clamp_to_world(viewport);
    }

    /// Steps every spring by `dt` seconds, returning whether the camera is still
    /// moving and the view therefore owes itself another frame.
    pub fn advance(&mut self, dt: f64, viewport: Rect) -> bool {
        // The viewport may have been resized since the last frame, which moves the
        // floor the zoom is allowed to reach.
        self.zoom
            .clamp_to(Self::min_zoom(viewport.size()), MAX_ZOOM);
        let zooming = self.zoom.advance(dt, ZOOM_EPSILON);
        if let Some(anchor) = self.anchor {
            // Re-derive the centre from where the held point belongs on screen at
            // the *new* scale. This one step is what pins the ground under the
            // cursor for a wheel zoom, and what flies a focus target straight in.
            let offset = anchor.offset_at(self.zoom.value());
            let size = self.world_size();
            self.x.snap_to(anchor.world.x - f64::from(offset.x) / size);
            self.y.snap_to(anchor.world.y - f64::from(offset.y) / size);
        }
        // A quarter of a screen point is the point at which further easing of the
        // centre stops being visible, and it has to be expressed at the current
        // scale — a fixed epsilon in world units is kilometres when zoomed out and
        // sub-pixel when zoomed in.
        let epsilon = 0.25 / self.world_size();
        let panning = self.x.advance(dt, epsilon) | self.y.advance(dt, epsilon);
        self.clamp_to_world(viewport);
        if !zooming {
            self.anchor = None;
        }
        zooming || panning
    }

    /// Frames `bounds` in `viewport`, easing there unless the move is long enough
    /// that easing would be a blurry sprint across ground that cannot load in
    /// time — then it cuts.
    pub fn fit(&mut self, bounds: GeoBounds, viewport: Rect) {
        let (center, zoom) = Self::framing(bounds, viewport);
        self.anchor = None;
        let travel =
            (center.x - self.x.value()).hypot(center.y - self.y.value()) * TILE_SIZE * zoom.exp2();
        if travel > f64::from(viewport.size().length()) * 2.0 {
            self.cut(center, zoom);
        } else {
            self.x.set_target(center.x);
            self.y.set_target(center.y);
            self.zoom.set_target(zoom);
        }
        self.clamp_to_world(viewport);
    }

    /// Frames `bounds` instantly, landing exactly where [`Camera::fit`] would.
    ///
    /// For a view that has only just appeared: there is nothing on screen yet to
    /// animate away from, so easing would only show the user a place they never
    /// asked to see on the way to the place they did.
    pub fn cut_to_fit(&mut self, bounds: GeoBounds, viewport: Rect) {
        let (center, zoom) = Self::framing(bounds, viewport);
        self.anchor = None;
        self.cut(center, zoom);
        self.clamp_to_world(viewport);
    }

    /// Eases the camera onto `target`, zooming in to at least `at_least`.
    ///
    /// Never *out*: a reader who has deliberately zoomed in close should not be
    /// yanked back out by asking to look at something. So this is a floor on the
    /// zoom, not a setting of it.
    pub fn focus_on(&mut self, target: World, at_least: f64, viewport: Rect) {
        let zoom = self
            .zoom
            .target()
            .max(at_least)
            .clamp(Self::min_zoom(viewport.size()), MAX_ZOOM);
        let from = self.screen_of(viewport, target) - viewport.center();
        if (zoom - self.zoom.value()).abs() < f64::EPSILON {
            // No change of scale to ride, so a plain glide is already straight.
            self.anchor = None;
            self.x.set_target(target.x);
            self.y.set_target(target.y);
        } else {
            self.anchor = Some(Anchor {
                world: target,
                from,
                to: Vec2::ZERO,
                zoom_from: self.zoom.value(),
                zoom_to: zoom,
            });
        }
        self.zoom.set_target(zoom);
        self.clamp_to_world(viewport);
    }

    /// Where framing `bounds` in `viewport` puts the camera. Shared by both fit
    /// paths, so an eased fit and a cut can never disagree about the destination.
    fn framing(bounds: GeoBounds, viewport: Rect) -> (World, f64) {
        let (north_west, south_east) = bounds.corners();
        let center = World {
            x: f64::midpoint(north_west.x, south_east.x),
            y: f64::midpoint(north_west.y, south_east.y),
        };
        let span = |a: f64, b: f64| (a - b).abs().max(f64::MIN_POSITIVE);
        let zoom_for = |extent: f32, span: f64| {
            (f64::from(extent.max(1.0)) * FIT_FILL / (TILE_SIZE * span)).log2()
        };
        let zoom = zoom_for(viewport.width(), span(south_east.x, north_west.x))
            .min(zoom_for(
                viewport.height(),
                span(south_east.y, north_west.y),
            ))
            .min(FIT_MAX_ZOOM)
            .clamp(Self::min_zoom(viewport.size()), MAX_ZOOM);
        (center, zoom)
    }

    /// Teleports the whole camera, leaving no easing in flight.
    fn cut(&mut self, center: World, zoom: f64) {
        self.x.snap_to(center.x);
        self.y.snap_to(center.y);
        self.zoom.snap_to(zoom);
    }

    /// The tiles covering `viewport`, at the integer zoom nearest the current one.
    #[must_use]
    pub fn tile_grid(self, viewport: Rect) -> TileGrid {
        // Round rather than floor: it keeps the on-screen tile scale within
        // ±41% of native instead of stretching every tile by up to 2x, which is
        // the difference between a crisp basemap and a soft one mid-zoom.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=MAX_ZOOM first, which a u8 holds exactly"
        )]
        let zoom = self.zoom.value().round().clamp(0.0, MAX_ZOOM) as u8;
        let last_index = f64::from((1u32 << zoom) - 1);
        let scale = TILE_SIZE * (self.zoom.value() - f64::from(zoom)).exp2();
        // Screen position of the world's north-west corner. Kept in f64: at zoom
        // 18 the world is 67 million points wide, well past where an f32 can
        // still resolve a single pixel.
        let size = self.world_size();
        let world_origin = (
            f64::from(viewport.center().x) - self.x.value() * size,
            f64::from(viewport.center().y) - self.y.value() * size,
        );
        let span = |min: f32, max: f32, origin: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped into the world's 0..=2^zoom-1 range before the cast"
            )]
            let index = |edge: f32| {
                ((f64::from(edge) - origin) / scale)
                    .floor()
                    .clamp(0.0, last_index) as u32
            };
            index(min)..=index(max)
        };
        let x = span(viewport.left(), viewport.right(), world_origin.0);
        let y = span(viewport.top(), viewport.bottom(), world_origin.1);
        TileGrid {
            zoom,
            first: (*x.start(), *y.start()),
            // Anchor on the first *visible* tile so `rect_of` does its arithmetic
            // near the viewport rather than millions of points away from it.
            origin: (
                f64::from(*x.start()).mul_add(scale, world_origin.0),
                f64::from(*y.start()).mul_add(scale, world_origin.1),
            ),
            x,
            y,
            scale,
        }
    }

    /// Confines the centre so the viewport never leaves the world.
    ///
    /// The value is bounded at the zoom being *drawn*, and the target at the zoom
    /// it is *heading for* — they are different limits whenever a zoom is in
    /// flight, and conflating them is a real bug rather than a nicety. Bounding
    /// the target at the current zoom means a fit that zooms in gets its centre
    /// clipped to what the old, wider view allowed: pressing Fit from far out
    /// would land short and leave an edge station off screen, and only a second
    /// press — by then already at the fitted zoom — would frame it properly.
    fn clamp_to_world(&mut self, viewport: Rect) {
        let (drawn, heading_for) = (self.zoom.value(), self.zoom.target());
        for (spring, extent) in [
            (&mut self.x, viewport.width()),
            (&mut self.y, viewport.height()),
        ] {
            let (lo, hi) = centre_limits(drawn, extent);
            spring.clamp_value(lo, hi);
            let (lo, hi) = centre_limits(heading_for, extent);
            spring.clamp_target(lo, hi);
        }
    }
}

/// How far the centre may sit from the world's edge, along an axis `extent`
/// screen points long, at a given zoom.
fn centre_limits(zoom: f64, extent: f32) -> (f64, f64) {
    let half = f64::from(extent) / (2.0 * TILE_SIZE * zoom.exp2());
    if half >= 0.5 {
        // The world is narrower than the viewport (only reachable mid-resize,
        // since `min_zoom` forbids settling here): centre it and stay there.
        (0.5, 0.5)
    } else {
        (half, 1.0 - half)
    }
}

/// The tiles that cover a viewport at one integer zoom level, each with the
/// screen rectangle it paints into.
#[derive(Clone, Debug)]
pub struct TileGrid {
    /// Integer zoom the tiles are addressed at.
    pub zoom: u8,
    /// Inclusive column range, already clipped to the world's edges.
    pub x: RangeInclusive<u32>,
    /// Inclusive row range, already clipped to the world's edges.
    pub y: RangeInclusive<u32>,
    /// Column and row of the tile `origin` refers to.
    first: (u32, u32),
    /// Screen position of the north-west corner of tile `first`.
    origin: (f64, f64),
    /// On-screen side length of one tile: `TILE_SIZE * 2^(zoom - camera zoom)`.
    scale: f64,
}

impl TileGrid {
    /// Every tile in the grid, row by row.
    pub fn tiles(&self) -> impl Iterator<Item = (u32, u32)> {
        let (x, y) = (self.x.clone(), self.y.clone());
        y.flat_map(move |row| x.clone().map(move |column| (column, row)))
    }

    /// How many tiles the grid holds.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.x.end() - self.x.start() + 1) as usize * (self.y.end() - self.y.start() + 1) as usize
    }

    /// Whether the grid covers nothing (it never does; present for `len`'s sake).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fetch order for a tile: squared tile distance from the middle of the grid.
    ///
    /// The centre is where the eye is, so it is what should arrive first when the
    /// queue is longer than the connection can service at once.
    #[must_use]
    pub fn priority_of(&self, x: u32, y: u32) -> i32 {
        let middle = (
            u32::midpoint(*self.x.start(), *self.x.end()),
            u32::midpoint(*self.y.start(), *self.y.end()),
        );
        let dx = i64::from(x) - i64::from(middle.0);
        let dy = i64::from(y) - i64::from(middle.1);
        i32::try_from(dx * dx + dy * dy).unwrap_or(i32::MAX)
    }

    /// Screen rectangle tile `(x, y)` paints into.
    #[must_use]
    pub fn rect_of(&self, x: u32, y: u32) -> Rect {
        let offset = |index: u32, first: u32, origin: f64| {
            (f64::from(index) - f64::from(first)).mul_add(self.scale, origin)
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the grid is anchored on its first visible tile, so this is a screen point"
        )]
        let min = pos2(
            offset(x, self.first.0, self.origin.0) as f32,
            offset(y, self.first.1, self.origin.1) as f32,
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "one tile spans at most 2 * TILE_SIZE screen points"
        )]
        let side = self.scale as f32;
        Rect::from_min_size(min, Vec2::splat(side))
    }
}

/// A latitude/longitude box, as produced by everything the map wants on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoBounds {
    /// Southern edge, in degrees.
    pub south: f64,
    /// Northern edge, in degrees.
    pub north: f64,
    /// Western edge, in degrees.
    pub west: f64,
    /// Eastern edge, in degrees.
    pub east: f64,
}

impl GeoBounds {
    /// The box enclosing every `(lat, lon)` yielded, or `None` if there were none.
    pub fn around(points: impl IntoIterator<Item = (f64, f64)>) -> Option<Self> {
        points
            .into_iter()
            .filter(|(lat, lon)| lat.is_finite() && lon.is_finite())
            .fold(None, |bounds: Option<Self>, (lat, lon)| {
                Some(bounds.map_or(
                    Self {
                        south: lat,
                        north: lat,
                        west: lon,
                        east: lon,
                    },
                    |b| Self {
                        south: b.south.min(lat),
                        north: b.north.max(lat),
                        west: b.west.min(lon),
                        east: b.east.max(lon),
                    },
                ))
            })
    }

    /// The box's north-west and south-east corners, projected.
    #[must_use]
    fn corners(self) -> (World, World) {
        (
            World::from_lat_lon(self.north, self.west),
            World::from_lat_lon(self.south, self.east),
        )
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, bail, ensure};
    use emath::{Rect, pos2, vec2};

    use super::{Camera, GeoBounds, MAX_ZOOM, Spring, TILE_SIZE, World};

    const FRAME: f64 = 1.0 / 60.0;

    /// Ten seconds of frames: far longer than any glide here, so running out of
    /// them means the camera is not converging rather than merely being slow.
    const GIVE_UP_AFTER: usize = 600;

    fn viewport() -> Rect {
        Rect::from_min_size(pos2(12.0, 30.0), vec2(640.0, 480.0))
    }

    /// Runs the camera to rest, so assertions see where it ended up rather than
    /// where it was passing through.
    fn settle(camera: &mut Camera, viewport: Rect) -> eyre::Result<()> {
        for _ in 0..GIVE_UP_AFTER {
            if !camera.advance(FRAME, viewport) {
                return Ok(());
            }
        }
        bail!("the camera never settled")
    }

    #[test]
    fn projecting_a_coordinate_round_trips() {
        for &(lat, lon) in &[(0.0, 0.0), (27.83, 140.49), (-33.9, -18.4), (85.0, 179.9)] {
            let (back_lat, back_lon) = World::from_lat_lon(lat, lon).to_lat_lon();
            assert!(
                (back_lat - lat).abs() < 1e-9 && (back_lon - lon).abs() < 1e-9,
                "{lat},{lon} came back as {back_lat},{back_lon}"
            );
        }
    }

    #[test]
    fn a_spring_eases_in_then_out_without_overshooting() -> eyre::Result<()> {
        let mut spring = Spring::new(0.0, 16.0);
        spring.set_target(1.0);

        let mut steps = Vec::new();
        let mut previous = 0.0;
        let mut settled_after = None;
        for frame in 0..GIVE_UP_AFTER {
            let moving = spring.advance(FRAME, 1e-4);
            ensure!(spring.value() <= 1.0, "overshot to {}", spring.value());
            ensure!(spring.value() >= previous, "reversed at frame {frame}");
            steps.push(spring.value() - previous);
            previous = spring.value();
            if !moving {
                settled_after = Some(frame);
                break;
            }
        }
        let Some(frames) = settled_after else {
            bail!("the spring never settled")
        };
        ensure!(
            (20..90).contains(&frames),
            "settled after {frames} frames, wanted a glide of roughly a third of a second"
        );

        // Ease in, then out: starting from rest the step has to grow for a few
        // frames before it decays, which is exactly what a linear tween does not do.
        let peak = steps
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map_or(0, |(frame, _)| frame);
        ensure!(
            (1..8).contains(&peak),
            "the fastest frame was {peak}, so the glide did not accelerate out of rest"
        );
        // The final frame is the sub-epsilon snap onto the target, not a step.
        let glide = steps.get(..steps.len() - 1).unwrap_or_default();
        ensure!(
            glide.iter().skip(peak).is_sorted_by(|a, b| a >= b),
            "the glide sped up again after its peak instead of easing out"
        );
        Ok(())
    }

    #[test]
    fn the_world_covers_the_viewport_at_the_minimum_zoom() {
        let viewport = viewport();
        let size = TILE_SIZE * Camera::min_zoom(viewport.size()).exp2();
        assert!(
            size >= f64::from(viewport.width()) - 1e-9
                && size >= f64::from(viewport.height()) - 1e-9,
            "a {size}-point world does not cover {viewport:?}, so it could wrap"
        );
    }

    #[test]
    fn the_viewport_never_leaves_the_world() -> eyre::Result<()> {
        let viewport = viewport();
        // Every direction a drag can go, plus a zoom-out that would otherwise let
        // the world shrink inside the viewport and wrap.
        for &delta in &[
            vec2(9000.0, 0.0),
            vec2(-9000.0, 0.0),
            vec2(0.0, 9000.0),
            vec2(0.0, -9000.0),
            vec2(9000.0, -9000.0),
        ] {
            let mut camera = Camera::default();
            camera.nudge_zoom(-20.0, None, viewport);
            settle(&mut camera, viewport)?;
            camera.pan(delta, viewport);
            settle(&mut camera, viewport)?;

            let north_west = camera.world_of(viewport, viewport.left_top());
            let south_east = camera.world_of(viewport, viewport.right_bottom());
            ensure!(
                north_west.x >= -1e-6 && north_west.y >= -1e-6,
                "panning by {delta:?} exposed {north_west:?} off the north-west corner"
            );
            ensure!(
                south_east.x <= 1.0 + 1e-6 && south_east.y <= 1.0 + 1e-6,
                "panning by {delta:?} exposed {south_east:?} off the south-east corner"
            );
        }
        Ok(())
    }

    #[test]
    fn zooming_holds_the_point_under_the_cursor() -> eyre::Result<()> {
        let viewport = viewport();
        for &levels in &[1.0, 3.0, -1.0, 0.5] {
            let mut camera = Camera::new(World::from_lat_lon(27.83, 140.49), 6.0);
            let cursor = viewport.center() + vec2(180.0, -120.0);
            let grabbed = camera.world_of(viewport, cursor);

            camera.nudge_zoom(levels, Some(cursor), viewport);
            // Mid-flight, not just at the end: the point must not drift and snap back.
            camera.advance(FRAME * 4.0, viewport);
            let midway = camera.screen_of(viewport, grabbed);
            ensure!(
                midway.distance(cursor) < 0.5,
                "{levels} levels in, the grabbed point had drifted to {midway:?} from {cursor:?}"
            );

            settle(&mut camera, viewport)?;
            let landed = camera.screen_of(viewport, grabbed);
            ensure!(
                landed.distance(cursor) < 0.5,
                "after {levels} levels the grabbed point ended at {landed:?}, not {cursor:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn zooming_without_a_focus_holds_the_centre() -> eyre::Result<()> {
        let viewport = viewport();
        let mut camera = Camera::new(World::from_lat_lon(27.83, 140.49), 6.0);
        let before = camera.center();
        camera.nudge_zoom(2.0, None, viewport);
        settle(&mut camera, viewport)?;
        let after = camera.center();
        ensure!(
            (after.x - before.x).abs() < 1e-9 && (after.y - before.y).abs() < 1e-9,
            "the centre drifted from {before:?} to {after:?}"
        );
        Ok(())
    }

    #[test]
    fn the_zoom_stays_between_its_bounds() -> eyre::Result<()> {
        let viewport = viewport();
        let mut camera = Camera::default();
        camera.nudge_zoom(99.0, None, viewport);
        settle(&mut camera, viewport)?;
        ensure!(
            (camera.zoom() - MAX_ZOOM).abs() < 1e-9,
            "zooming in ran past the deepest level to {}",
            camera.zoom()
        );

        camera.nudge_zoom(-99.0, None, viewport);
        settle(&mut camera, viewport)?;
        let floor = Camera::min_zoom(viewport.size());
        ensure!(
            (camera.zoom() - floor).abs() < 1e-9,
            "zooming out ran past {floor} to {}, where the world would wrap",
            camera.zoom()
        );
        Ok(())
    }

    #[test]
    fn the_tile_grid_covers_the_viewport() -> eyre::Result<()> {
        let viewport = viewport();
        // Fractional zooms are the interesting case: they are where the grid has
        // to scale tiles rather than lay them out one-to-one.
        for zoom in [2.0, 4.3, 7.5, 11.9, 18.0] {
            let mut camera = Camera::new(World::from_lat_lon(27.83, 140.49), zoom);
            settle(&mut camera, viewport)?;
            let grid = camera.tile_grid(viewport);
            let Some(covered) = grid
                .tiles()
                .map(|(x, y)| grid.rect_of(x, y))
                .reduce(Rect::union)
            else {
                bail!("the grid at zoom {zoom} held no tiles at all")
            };
            ensure!(
                covered.contains_rect(viewport),
                "at zoom {zoom} the grid covered {covered:?}, leaving {viewport:?} exposed"
            );
            let count = 1u32 << grid.zoom;
            ensure!(
                *grid.x.end() < count && *grid.y.end() < count,
                "the grid reached tile {:?},{:?}, outside a world {count} tiles wide",
                grid.x.end(),
                grid.y.end()
            );
            ensure!(
                grid.len() == grid.tiles().count(),
                "len disagrees with tiles"
            );
        }
        Ok(())
    }

    #[test]
    fn the_tile_grid_prioritizes_the_middle() -> eyre::Result<()> {
        let viewport = viewport();
        let camera = Camera::new(World::from_lat_lon(27.83, 140.49), 8.0);
        let grid = camera.tile_grid(viewport);
        let Some((x, y)) = grid.tiles().min_by_key(|&(x, y)| grid.priority_of(x, y)) else {
            bail!("the grid held no tiles at all")
        };
        ensure!(
            grid.rect_of(x, y).contains(viewport.center()),
            "the first tile fetched should be the one under the middle of the view"
        );
        Ok(())
    }

    /// A regional network: close enough together that a fit has to zoom in, and
    /// far enough from the antimeridian that a centre clamped at the wrong scale
    /// lands visibly wrong. This is the shape that caught the fit-lands-short bug.
    const REGIONAL: [(f64, f64); 4] = [
        (34.95, -106.46),
        (33.61, -116.46),
        (32.31, -110.78),
        (42.51, -71.56),
    ];

    /// Station spreads a fit has to cope with. A fixed table rather than random
    /// input, so a failure here is always the same failure.
    const NETWORKS: [(&str, &[(f64, f64)]); 6] = [
        ("a regional network", &REGIONAL),
        (
            "a hemisphere",
            &[
                (34.95, -106.46),
                (21.42, -158.01),
                (27.83, 140.49),
                (42.51, -71.56),
            ],
        ),
        ("a single station", &[(27.83, 140.49)]),
        (
            "a pair straddling the antimeridian",
            &[(-17.6, 168.3), (51.9, -176.6)],
        ),
        (
            "a local array",
            &[(50.23, 6.72), (50.31, 6.85), (50.18, 6.60)],
        ),
        ("a high-latitude pair", &[(78.93, 11.87), (69.65, 18.94)]),
    ];

    /// Every camera a user can be sitting at when they press Fit, since where a
    /// fit starts must not change where it ends.
    fn starting_cameras(viewport: Rect) -> eyre::Result<Vec<(&'static str, Camera)>> {
        let mut all = vec![("the default view", Camera::default())];

        let mut zoomed_out = Camera::default();
        zoomed_out.nudge_zoom(-30.0, None, viewport);
        settle(&mut zoomed_out, viewport)?;
        all.push(("zoomed all the way out", zoomed_out));

        let mut far_in = Camera::new(World::from_lat_lon(-33.9, 18.4), 12.0);
        settle(&mut far_in, viewport)?;
        all.push(("deep in, half a world away", far_in));

        let mut near_in = Camera::new(World::from_lat_lon(35.0, -100.0), 6.0);
        settle(&mut near_in, viewport)?;
        all.push(("deep in, already nearby", near_in));

        let mut deepest = Camera::new(World::from_lat_lon(0.0, 0.0), MAX_ZOOM);
        settle(&mut deepest, viewport)?;
        all.push(("at the deepest zoom", deepest));

        Ok(all)
    }

    /// Whether two cameras show the same thing, to within half a screen point —
    /// the resolution at which a difference could be seen at all.
    fn same_view(a: Camera, b: Camera, viewport: Rect) -> bool {
        (a.zoom() - b.zoom()).abs() < 1e-6
            && a.screen_of(viewport, b.center())
                .distance(viewport.center())
                < 0.5
    }

    /// Whether the viewport is showing only real world, with no wrapped copy or
    /// void past the edges.
    fn inside_the_world(camera: Camera, viewport: Rect) -> bool {
        let north_west = camera.world_of(viewport, viewport.left_top());
        let south_east = camera.world_of(viewport, viewport.right_bottom());
        north_west.x >= -1e-6
            && north_west.y >= -1e-6
            && south_east.x <= 1.0 + 1e-6
            && south_east.y <= 1.0 + 1e-6
    }

    fn bounds_of(points: &[(f64, f64)]) -> eyre::Result<GeoBounds> {
        match GeoBounds::around(points.iter().copied()) {
            Some(bounds) => Ok(bounds),
            None => bail!("{points:?} should enclose a box"),
        }
    }

    #[test]
    fn a_fit_lands_in_one_press_from_any_camera() -> eyre::Result<()> {
        let viewport = viewport();
        for (network, points) in NETWORKS {
            let bounds = bounds_of(points)?;
            for (from, start) in starting_cameras(viewport)? {
                let mut camera = start;
                camera.fit(bounds, viewport);
                settle(&mut camera, viewport)?;
                let once = camera;

                camera.fit(bounds, viewport);
                settle(&mut camera, viewport)?;

                ensure!(
                    same_view(once, camera, viewport),
                    "fitting {network} from {from}: one press landed at \
                     {:?} / zoom {:.3}, a second press moved it to {:?} / zoom {:.3} — \
                     the first press should already be final",
                    once.center(),
                    once.zoom(),
                    camera.center(),
                    camera.zoom()
                );
            }
        }
        Ok(())
    }

    /// A window that has only just opened cuts straight to its fit rather than
    /// easing there. That is only sound if the cut lands where the ease would
    /// have: otherwise opening the map and pressing Fit would frame differently.
    #[test]
    fn cutting_to_a_fit_lands_where_easing_would() -> eyre::Result<()> {
        let viewport = viewport();
        for (network, points) in NETWORKS {
            let bounds = bounds_of(points)?;
            for (from, start) in starting_cameras(viewport)? {
                let mut eased = start;
                eased.fit(bounds, viewport);
                settle(&mut eased, viewport)?;

                let mut cut = start;
                cut.cut_to_fit(bounds, viewport);
                ensure!(
                    !cut.advance(FRAME, viewport),
                    "cutting to {network} from {from} left the camera still moving"
                );
                ensure!(
                    same_view(eased, cut, viewport),
                    "fitting {network} from {from}: easing landed at {:?} / zoom {:.3} \
                     but cutting landed at {:?} / zoom {:.3}",
                    eased.center(),
                    eased.zoom(),
                    cut.center(),
                    cut.zoom()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn focusing_centres_the_target_and_never_zooms_out() -> eyre::Result<()> {
        let viewport = viewport();
        let target = World::from_lat_lon(35.0, 139.7);
        for (from, start) in starting_cameras(viewport)? {
            let mut camera = start;
            let before = camera.zoom();
            camera.focus_on(target, 8.0, viewport);
            settle(&mut camera, viewport)?;

            let at = camera.screen_of(viewport, target);
            ensure!(
                at.distance(viewport.center()) < 0.5,
                "focusing from {from} left the target at {at:?}, not in the middle"
            );
            ensure!(
                camera.zoom() >= before - 1e-9,
                "focusing from {from} zoomed out, {before:.3} -> {:.3}",
                camera.zoom()
            );
            ensure!(
                camera.zoom() >= 8.0 - 1e-9,
                "focusing from {from} stopped at zoom {:.3}, too far out to read a city",
                camera.zoom()
            );
        }
        Ok(())
    }

    /// A double-click should bring its target straight in. Interpolating the
    /// centre in world space does not: the scale grows exponentially underneath
    /// the pan, so the target swings *outward* before coming back — which is the
    /// arc the flight is supposed to avoid.
    #[test]
    fn focusing_brings_the_target_straight_in() -> eyre::Result<()> {
        let viewport = viewport();
        for (from, start) in starting_cameras(viewport)? {
            let target = World::from_lat_lon(35.0, -106.46);
            let mut camera = start;
            let opening = camera.screen_of(viewport, target) - viewport.center();
            camera.focus_on(target, 8.0, viewport);

            let mut previous = opening.length();
            for frame in 0..GIVE_UP_AFTER {
                let moving = camera.advance(FRAME, viewport);
                let offset = camera.screen_of(viewport, target) - viewport.center();

                ensure!(
                    offset.length() <= previous + 0.5,
                    "focusing from {from}: on frame {frame} the target moved further from \
                     the middle, {previous:.1} -> {:.1} points — it is swinging out, not \
                     flying in",
                    offset.length()
                );
                // And straight: it stays on the ray it started on.
                if offset.length() > 1.0 && opening.length() > 1.0 {
                    let cross = opening.normalized().x * offset.normalized().y
                        - opening.normalized().y * offset.normalized().x;
                    ensure!(
                        cross.abs() < 1e-3,
                        "focusing from {from}: on frame {frame} the target left its line, \
                         {opening:?} -> {offset:?}"
                    );
                }
                previous = offset.length();
                if !moving {
                    break;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_fit_frames_the_network_from_any_camera() -> eyre::Result<()> {
        let viewport = viewport();
        for (network, points) in NETWORKS {
            let bounds = bounds_of(points)?;
            for (from, start) in starting_cameras(viewport)? {
                let mut camera = start;
                camera.fit(bounds, viewport);
                settle(&mut camera, viewport)?;

                for &(lat, lon) in points {
                    let at = camera.screen_of(viewport, World::from_lat_lon(lat, lon));
                    ensure!(
                        viewport.contains(at),
                        "fitting {network} from {from}: {lat},{lon} landed at {at:?}, \
                         outside {viewport:?}"
                    );
                }
            }
        }
        Ok(())
    }

    /// The world bound has to hold on every frame of a move, not merely once it
    /// stops — a camera that wanders off the map for 20 frames and comes back has
    /// still shown 20 frames of nothing.
    #[test]
    fn the_world_holds_on_every_frame_of_a_fit() -> eyre::Result<()> {
        let viewport = viewport();
        for (network, points) in NETWORKS {
            let bounds = bounds_of(points)?;
            for (from, start) in starting_cameras(viewport)? {
                let mut camera = start;
                camera.fit(bounds, viewport);
                for frame in 0..GIVE_UP_AFTER {
                    let moving = camera.advance(FRAME, viewport);
                    ensure!(
                        inside_the_world(camera, viewport),
                        "fitting {network} from {from}: frame {frame} showed past the \
                         world edge, at {:?} / zoom {:.3}",
                        camera.center(),
                        camera.zoom()
                    );
                    if !moving {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_fit_puts_every_point_on_screen() -> eyre::Result<()> {
        let viewport = viewport();
        // The Bonin Islands example: six GSN stations spread most of a hemisphere.
        let stations = [
            (34.95, -106.46),
            (64.87, -147.86),
            (42.51, -71.56),
            (21.42, -158.01),
            (33.61, -116.46),
            (32.31, -110.78),
            (27.83, 140.49),
        ];
        let Some(bounds) = GeoBounds::around(stations) else {
            bail!("the station list should enclose a box")
        };
        let mut camera = Camera::default();
        camera.fit(bounds, viewport);
        settle(&mut camera, viewport)?;

        for (lat, lon) in stations {
            let at = camera.screen_of(viewport, World::from_lat_lon(lat, lon));
            ensure!(
                viewport.contains(at),
                "{lat},{lon} landed at {at:?}, outside {viewport:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_fit_onto_one_point_stops_short_of_the_rooftops() -> eyre::Result<()> {
        let viewport = viewport();
        let Some(bounds) = GeoBounds::around([(27.83, 140.49)]) else {
            bail!("one point should still enclose a box")
        };
        let mut camera = Camera::default();
        camera.fit(bounds, viewport);
        settle(&mut camera, viewport)?;
        ensure!(
            camera.zoom() <= super::FIT_MAX_ZOOM + 1e-9,
            "a zero-extent fit ran to zoom {}",
            camera.zoom()
        );
        let at = camera.screen_of(viewport, World::from_lat_lon(27.83, 140.49));
        ensure!(
            at.distance(viewport.center()) < 0.5,
            "the fitted point landed at {at:?}, not in the middle"
        );
        Ok(())
    }

    #[test]
    fn bounds_around_nothing_are_nothing() {
        assert!(GeoBounds::around([]).is_none(), "no points enclose no box");
        assert!(
            GeoBounds::around([(f64::NAN, 0.0)]).is_none(),
            "a point with no coordinates should not widen the box to everything"
        );
    }
}
