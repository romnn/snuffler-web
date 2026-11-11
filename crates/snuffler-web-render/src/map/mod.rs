//! Slippy-map camera geometry: the Web-Mercator projection, a spring-eased zoom
//! that keeps a chosen point pinned under the cursor, world bounds that forbid
//! wrap-around, and the tile grid a frame needs to paint.
//!
//! Toolkit-neutral and free of I/O, so it runs (and is tested) on the host while
//! the egui view only supplies a viewport rectangle and a pointer position.

mod camera;
mod spring;
mod tiles;

pub use camera::Camera;
pub use spring::{Spring, ease_out};
pub use tiles::TileGrid;

/// Side length of one raster tile, in map pixels at its own zoom level.
pub const TILE_SIZE: f64 = 256.0;

/// Latitude where the Web-Mercator projection runs away to infinity.
///
/// Cutting the world here is what makes it exactly square, and every tile server does it.
pub const MAX_LATITUDE: f64 = 85.051_128_78;

/// Deepest zoom level the raster basemaps serve.
pub const MAX_ZOOM: f64 = 18.0;

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

    /// The box's north-west corner, projected.
    fn north_west(self) -> World {
        World::from_lat_lon(self.north, self.west)
    }

    /// The box's south-east corner, projected.
    fn south_east(self) -> World {
        World::from_lat_lon(self.south, self.east)
    }
}

#[cfg(test)]
mod tests {
    use super::GeoBounds;

    #[test]
    fn bounds_around_nothing_are_nothing() {
        assert!(GeoBounds::around([]).is_none(), "no points enclose no box");
        assert!(
            GeoBounds::around([(f64::NAN, 0.0)]).is_none(),
            "a point with no coordinates should not widen the box to everything"
        );
    }
}
