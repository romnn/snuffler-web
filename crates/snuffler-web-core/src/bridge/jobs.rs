//! Snapshots and local results owned by bridge jobs.

use crate::activity::JobHandle;

use super::{ViewMeta, ViewSpan};

/// A view plus its out-of-band sample buffer.
#[derive(Clone, Debug, Default)]
pub struct ViewData {
    /// Track metadata and time bounds.
    pub meta: ViewMeta,
    /// Uniform samples or packed waterfall cells.
    pub ys: Vec<f32>,
}

impl ViewData {
    /// Raw samples for one protobuf span.
    #[must_use]
    pub fn samples<'a>(&'a self, span: &ViewSpan) -> &'a [f32] {
        let off = usize::try_from(span.offset).unwrap_or(usize::MAX);
        let cnt = usize::try_from(span.count).unwrap_or(usize::MAX);
        let end = off.saturating_add(cnt).min(self.ys.len());
        self.ys.get(off.min(self.ys.len())..end).unwrap_or(&[])
    }
}

/// A rendered pyrocko/matplotlib figure.
#[derive(Clone, Debug, Default)]
pub struct FigureData {
    /// Encoded PNG.
    pub png: Vec<u8>,
    /// Render failure text.
    pub error: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct FigureSlot {
    pub(super) data: FigureData,
    pub(super) generation: u64,
    pub(super) job: Option<JobHandle>,
}

pub(super) enum TileState {
    InFlight,
    Ready(u32, u32, Vec<u8>),
    Consumed,
    Failed,
}

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

pub(super) const TILE_BATCH: usize = 8;
pub(super) const TILE_BATCHES_IN_FLIGHT: usize = 2;

/// How a Python command line completed.
#[derive(Clone, Debug)]
pub enum CommandOutcome {
    /// The command and any requested view change succeeded.
    Succeeded,
    /// The command was rejected with user-facing detail.
    Failed(String),
}
