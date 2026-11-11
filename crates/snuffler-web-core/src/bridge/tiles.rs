//! The map's basemap tile cache: which tiles are wanted, in flight, decoded or failed.
//!
//! The browser fetches and decodes the tiles (`bridge::web::tiles`); this half only
//! tracks their state, so the view can drain finished tiles each frame.

use std::collections::HashMap;

use super::Bridge;

/// How many tiles one browser fetch batch carries.
pub(super) const TILE_BATCH_SIZE: usize = 8;
/// How many fetch batches may be out at once.
pub(super) const TILE_BATCHES_IN_FLIGHT: usize = 2;

/// A map tile the view wants on screen.
#[derive(Clone, Debug)]
pub struct TileRequest {
    /// Integer zoom level.
    pub z: u8,
    /// Tile column.
    pub x: u32,
    /// Tile row.
    pub y: u32,
    /// Fully substituted basemap URL.
    pub url: String,
    /// Fetch order; lower runs first.
    pub priority: i32,
}

/// A decoded tile handed to the view exactly once.
#[derive(Clone, Debug)]
pub struct ReadyTile {
    /// Integer zoom level.
    pub z: u8,
    /// Tile column.
    pub x: u32,
    /// Tile row.
    pub y: u32,
    /// Decoded pixel width.
    pub width: u32,
    /// Decoded pixel height.
    pub height: u32,
    /// Unmultiplied RGBA samples.
    pub rgba: Vec<u8>,
}

/// Slippy-map coordinates of one tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct TileKey {
    pub(super) z: u8,
    pub(super) x: u32,
    pub(super) y: u32,
}

impl From<&TileRequest> for TileKey {
    fn from(request: &TileRequest) -> Self {
        Self {
            z: request.z,
            x: request.x,
            y: request.y,
        }
    }
}

pub(super) enum TileState {
    InFlight,
    Ready {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Handed to the view, which now owns the texture.
    Consumed,
    Failed,
}

/// Every tile the bridge knows about, plus the queue of tiles not yet fetched.
#[derive(Default)]
pub(super) struct TileCache {
    pub(super) states: HashMap<TileKey, TileState>,
    /// Bumped by [`Bridge::clear_tiles`].
    /// Tiles are keyed by coordinates alone, so a fetch still in flight when the basemap
    /// changes would otherwise land in the cleared map under the same key as its
    /// replacement.
    pub(super) epoch: u64,
    /// Wanted tiles that no batch has started yet, ordered by priority.
    /// Replaced wholesale on every request, which is what lets a zoom or a pan abandon
    /// the tiles it just moved away from instead of fetching them.
    pub(super) queue: Vec<TileRequest>,
    /// How many batches are out; see [`TILE_BATCHES_IN_FLIGHT`].
    pub(super) batches_in_flight: usize,
}

impl Bridge {
    /// Take every tile that has finished decoding, each exactly once, for the view
    /// to upload as a texture.
    ///
    /// Draining all of them, rather than asking per key, matters because the view
    /// moves on.
    /// A tile that lands after a zoom has left its level would otherwise sit here
    /// holding a megabyte of RGBA that nobody ever asks for.
    #[must_use]
    pub fn take_ready_tiles(&self) -> Vec<ReadyTile> {
        let mut state = self.inner.borrow_mut();
        let ready: Vec<TileKey> = state
            .tiles
            .states
            .iter()
            .filter(|(_, tile)| matches!(tile, TileState::Ready { .. }))
            .map(|(key, _)| *key)
            .collect();
        ready
            .into_iter()
            .filter_map(
                |key| match state.tiles.states.insert(key, TileState::Consumed) {
                    Some(TileState::Ready {
                        width,
                        height,
                        rgba,
                    }) => Some(ReadyTile {
                        z: key.z,
                        x: key.x,
                        y: key.y,
                        width,
                        height,
                        rgba,
                    }),
                    _ => None,
                },
            )
            .collect()
    }

    /// Forget a tile so a later request fetches it again.
    ///
    /// The view calls this when it evicts a texture.
    /// A tile it has already consumed is remembered here as fetched, so without this
    /// the evicted tile could never be asked for a second time.
    pub fn forget_tile(&self, z: u8, x: u32, y: u32) {
        self.inner
            .borrow_mut()
            .tiles
            .states
            .remove(&TileKey { z, x, y });
    }

    /// Discard every cached, in-flight, and failed tile so later requests refetch.
    ///
    /// The frontend calls this when the basemap provider changes.
    /// Tile keys carry no provider, so without a reset the already-fetched tiles would
    /// keep showing the previous basemap while only new keys use the new one.
    pub fn clear_tiles(&self) {
        let mut state = self.inner.borrow_mut();
        state.tiles.states.clear();
        state.tiles.queue.clear();
        state.tiles.epoch = state.tiles.epoch.wrapping_add(1);
    }
}
