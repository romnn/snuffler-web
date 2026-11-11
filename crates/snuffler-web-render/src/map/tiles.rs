//! The raster tiles that cover a viewport.

use std::ops::RangeInclusive;

use emath::{Rect, Vec2, pos2};

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
    /// Column of the tile the origin refers to, the first visible one.
    pub(super) first_column: u32,
    /// Row of the tile the origin refers to, the first visible one.
    pub(super) first_row: u32,
    /// Screen x of the north-west corner of the first visible tile.
    pub(super) origin_x: f64,
    /// Screen y of the north-west corner of the first visible tile.
    pub(super) origin_y: f64,
    /// On-screen side length of one tile: `TILE_SIZE * 2^(zoom - camera zoom)`.
    pub(super) scale: f64,
}

impl TileGrid {
    /// Every tile in the grid, row by row.
    pub fn tiles(&self) -> impl Iterator<Item = (u32, u32)> {
        let (x, y) = (self.x.clone(), self.y.clone());
        y.flat_map(move |row| x.clone().map(move |column| (column, row)))
    }

    /// Fetch order for a tile: squared tile distance from the middle of the grid.
    ///
    /// The centre is where the eye is, so it is what should arrive first when the
    /// queue is longer than the connection can service at once.
    #[must_use]
    pub fn priority_of(&self, x: u32, y: u32) -> i32 {
        let middle_column = u32::midpoint(*self.x.start(), *self.x.end());
        let middle_row = u32::midpoint(*self.y.start(), *self.y.end());
        let dx = i64::from(x) - i64::from(middle_column);
        let dy = i64::from(y) - i64::from(middle_row);
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
            offset(x, self.first_column, self.origin_x) as f32,
            offset(y, self.first_row, self.origin_y) as f32,
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "one tile spans at most 2 * TILE_SIZE screen points"
        )]
        let side = self.scale as f32;
        Rect::from_min_size(min, Vec2::splat(side))
    }
}
