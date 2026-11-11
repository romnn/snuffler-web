//! The basemap's raster tiles: what is on the GPU, what to ask for next, and how
//! a tile arrives on screen.
//!
//! Two things here make a zoom feel continuous rather than staged:
//!
//! - A tile that is not decoded yet is drawn from the nearest ancestor already in
//!   the cache, sampled through the sub-rectangle it occupies inside it.
//!   So the ground under the cursor is always *something*, blurry at worst, never a
//!   grey hole.
//! - A tile that has just landed fades in over that ancestor instead of popping.

use std::collections::BTreeMap;

use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions};

use snuffler_web_core::bridge::{Bridge, TileRequest};
use snuffler_web_render::map::{TileGrid, ease_out};

/// `(zoom, column, row)`, the address of one tile.
type TileKey = (u8, u32, u32);

/// How long a freshly decoded tile takes to fade in over its parent, in seconds.
const FADE_DURATION_S: f64 = 0.22;

/// How many zoom levels up to look for a stand-in while a tile loads.
///
/// Five levels is a 32x magnification; past that the parent carries no usable
/// detail, and the flat background reads better than a smear.
const FALLBACK_DEPTH: u8 = 5;

/// Textures kept on the GPU.
///
/// A screenful is a few dozen; the rest are ancestors and recently left levels
/// worth keeping so zooming back out is instant.
/// At 256 KiB of RGBA each this is a ~48 MiB ceiling.
const CACHE_CAPACITY: usize = 192;

/// Tile rectangles are grown by this much before painting.
///
/// Two rectangles that share an edge at a fractional scale each antialias their own
/// half of it and leave a hairline of background between them.
/// Overlapping by a fraction of a point costs nothing visible and closes the seam.
const SEAM_OVERLAP: f32 = 0.5;

/// One decoded tile on the GPU.
struct CachedTile {
    texture: TextureHandle,
    /// When the tile was uploaded, for the fade-in.
    ready_at: f64,
    /// When it was last painted, for eviction.
    used_at: f64,
}

/// The decoded-tile cache for one map window.
#[derive(Default)]
pub struct TileCache {
    entries: BTreeMap<TileKey, CachedTile>,
}

impl TileCache {
    /// Uploads everything the bridge has finished decoding.
    pub fn absorb_ready(&mut self, ctx: &egui::Context, bridge: &Bridge, now: f64) {
        for tile in bridge.take_ready_tiles() {
            let image = ColorImage::from_rgba_unmultiplied(
                [tile.width as usize, tile.height as usize],
                &tile.rgba,
            );
            let texture = ctx.load_texture(
                format!("map-tile-{}-{}-{}", tile.z, tile.x, tile.y),
                image,
                TextureOptions::LINEAR,
            );
            self.entries.insert(
                (tile.z, tile.x, tile.y),
                CachedTile {
                    texture,
                    ready_at: now,
                    used_at: now,
                },
            );
        }
    }

    /// Paints every tile in `grid`, standing in with ancestors where a tile has not landed or is
    /// still fading.
    ///
    /// Returns whether a fade is still running, in which case the frame owes itself a repaint.
    pub fn paint(&mut self, painter: &Painter, grid: &TileGrid, now: f64) -> bool {
        let mut fading = false;
        for (x, y) in grid.tiles() {
            let key = (grid.zoom, x, y);
            let rect = grid.rect_of(x, y).expand(SEAM_OVERLAP);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the eased fraction is clamped to 0..=1 before it becomes an alpha"
            )]
            let alpha = self.entries.get(&key).map_or(0.0, |entry| {
                ease_out((now - entry.ready_at) / FADE_DURATION_S) as f32
            });
            if alpha < 1.0 {
                self.paint_ancestor(painter, key, rect, now);
                fading |= self.entries.contains_key(&key);
            }
            if alpha > 0.0
                && let Some(entry) = self.entries.get_mut(&key)
            {
                entry.used_at = now;
                painter.image(
                    entry.texture.id(),
                    rect,
                    UNIT_RECT,
                    Color32::WHITE.gamma_multiply(alpha),
                );
            }
        }
        fading
    }

    /// The tiles in `grid` that are neither on the GPU nor already asked for,
    /// scored so the middle of the screen resolves first.
    pub fn wanted(
        &self,
        grid: &TileGrid,
        url_of: impl Fn(u8, u32, u32) -> String,
    ) -> Vec<TileRequest> {
        grid.tiles()
            .filter(|&(x, y)| !self.entries.contains_key(&(grid.zoom, x, y)))
            .map(|(x, y)| TileRequest {
                z: grid.zoom,
                x,
                y,
                url: url_of(grid.zoom, x, y),
                priority: grid.priority_of(x, y),
            })
            .collect()
    }

    /// Drops the least recently painted textures once the cache is over capacity,
    /// telling the bridge to forget them so they can be fetched again later.
    pub fn evict_stale(&mut self, bridge: &Bridge) {
        let excess = self.entries.len().saturating_sub(CACHE_CAPACITY);
        if excess == 0 {
            return;
        }
        let mut by_age: Vec<(f64, TileKey)> = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.used_at, *key))
            .collect();
        by_age.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, key) in by_age.iter().take(excess) {
            self.entries.remove(&key);
            bridge.forget_tile(key.0, key.1, key.2);
        }
    }

    /// Throws away every texture, for when the basemap provider changes and the
    /// imagery behind each key is no longer the imagery on screen.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Paints the nearest decoded ancestor of `key` into `rect`, cropped to the
    /// part of it this tile covers.
    fn paint_ancestor(&mut self, painter: &Painter, key: TileKey, rect: Rect, now: f64) {
        let (zoom, x, y) = key;
        let Some((ancestor, uv)) = (1..=FALLBACK_DEPTH.min(zoom))
            .map(|up| ((zoom - up, x >> up, y >> up), up))
            .find(|(candidate, _)| self.entries.contains_key(candidate))
            .map(|(ancestor, up)| {
                // `up` levels up, the ancestor holds 2^up tiles per side; this
                // tile is one cell of that, addressed by the bits the shift lost.
                let across = f32::from(1u16 << up);
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "the masked index is bounded by 2^FALLBACK_DEPTH, exact in f32"
                )]
                let within = |index: u32| (index & ((1u32 << up) - 1)) as f32 / across;
                (
                    ancestor,
                    Rect::from_min_size(
                        egui::pos2(within(x), within(y)),
                        egui::Vec2::splat(1.0 / across),
                    ),
                )
            })
        else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(&ancestor) {
            entry.used_at = now;
            painter.image(entry.texture.id(), rect, uv, Color32::WHITE);
        }
    }
}

/// The whole of a texture, in UV space.
const UNIT_RECT: Rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
