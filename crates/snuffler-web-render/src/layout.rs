//! Toolkit-neutral waveform and track layout.

use emath::{Rect, pos2};
use snuffler_web_core::bridge::{ViewData, ViewTrack};
use snuffler_web_core::state::{AppState, DEFAULT_NTRACKS_SHOWN, MIN_TRACK_HEIGHT_PX};

/// Height reserved below the track rows for the shared time axis.
pub const AXIS_HEIGHT: f32 = 52.0;

/// Geometry of one waveform frame, derived from the view and canvas rectangle.
pub struct Layout<'a> {
    /// Full drawable canvas.
    pub rect: Rect,
    /// Canvas area occupied by waveform rows.
    pub track_area: Rect,
    /// Absolute time represented by the start of relative track data.
    pub data_tmin: f64,
    /// Left edge of the visible time window.
    pub view_tmin: f64,
    /// Right edge of the visible time window.
    pub view_tmax: f64,
    /// Duration of the visible time window in seconds, never zero.
    pub window_duration: f64,
    /// Tracks intersecting the visible vertical window.
    pub visible_tracks: &'a [ViewTrack],
    /// Index of the first visible track.
    pub first_row: usize,
    /// Number of rows drawn, which is the length of [`Self::visible_tracks`].
    pub rows_drawn: usize,
    /// Resolved height of one row in logical points.
    pub row_height: f32,
    /// Top edge of the first row, which may be above the clipped track area.
    pub first_row_top: f32,
}

impl Layout<'_> {
    /// Projects an absolute time onto the canvas's x axis.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "one pixel coordinate at the end of an f64 time projection"
    )]
    pub fn time_to_x(&self, time: f64) -> f32 {
        self.rect.left()
            + ((time - self.view_tmin) / self.window_duration) as f32 * self.rect.width()
    }

    /// The top edge of the visible row at `index`, counted from the first visible row.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a visible row index is a few hundred at most"
    )]
    pub fn row_top(&self, index: usize) -> f32 {
        self.first_row_top + index as f32 * self.row_height
    }
}

/// Vertical row geometry resolved from track-height, snapping, and scroll state.
pub struct TrackGeometry {
    /// Resolved height of one row in logical points.
    pub row_height: f32,
    /// Index of the first row to draw.
    pub first_row: usize,
    /// Number of rows needed to cover the track area.
    pub rows_drawn: usize,
    /// Rows a full page shows: `rows_drawn` before the clamp to the rows that
    /// remain past the first row.
    ///
    /// The view request and the per-track payload budget size themselves on this.
    /// A partial last page must not change the horizontal resolution, and with it
    /// the request identity, of a purely vertical scroll.
    pub rows_per_page: usize,
    /// Portion of the first row clipped above the track area.
    pub first_row_clipped: f32,
    /// Clamped fractional row offset.
    pub scroll: f32,
    /// Track height to retain, or zero while automatic fitting remains active.
    pub track_height: f32,
}

/// Resolves vertical track controls into concrete row geometry.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "bounded row counts and logical pixel sizes are converted between indices and f32 geometry"
)]
pub fn track_geometry(state: &AppState, track_count: usize, area_height: f32) -> TrackGeometry {
    let area = area_height.max(1.0);
    let track_count_f = track_count.max(1) as f32;
    let automatic = state.track_height <= 0.0;
    let mut height = if automatic {
        area / DEFAULT_NTRACKS_SHOWN as f32
    } else {
        state.track_height
    };
    height = height.clamp(MIN_TRACK_HEIGHT_PX, area.max(MIN_TRACK_HEIGHT_PX));

    if state.snap_tracks {
        // Snapped: a whole number of rows stretched to fill the area exactly
        let rows_per_page = (area / height).round().clamp(1.0, track_count_f) as usize;
        let row_height = area / rows_per_page as f32;
        let max_first_row = track_count.saturating_sub(rows_per_page);
        let scroll = state.track_scroll.clamp(0.0, max_first_row as f32);
        let first_row = scroll.round().clamp(0.0, max_first_row as f32) as usize;
        let rows_drawn = rows_per_page.min(track_count.saturating_sub(first_row));
        TrackGeometry {
            row_height,
            first_row,
            rows_drawn,
            rows_per_page,
            first_row_clipped: 0.0,
            scroll,
            track_height: if automatic { 0.0 } else { row_height },
        }
    } else {
        // Continuous: rows keep their height and the first one may be cut off
        let row_height = height;
        let rows_in_area = area / row_height;
        let max_scroll = (track_count_f - rows_in_area).max(0.0);
        let scroll = state.track_scroll.clamp(0.0, max_scroll);
        let first_row = scroll.floor() as usize;
        let first_row_clipped = (scroll - first_row as f32) * row_height;
        let rows_per_page = ((first_row_clipped + area) / row_height).ceil() as usize;
        let rows_drawn = rows_per_page.min(track_count.saturating_sub(first_row));
        TrackGeometry {
            row_height,
            first_row,
            rows_drawn,
            rows_per_page,
            first_row_clipped,
            scroll,
            track_height: if automatic { 0.0 } else { height },
        }
    }
}

/// Computes the horizontal time window and vertical track window for a frame.
#[must_use]
pub fn compute_layout<'a>(state: &AppState, view: &'a ViewData, rect: Rect) -> Layout<'a> {
    let meta = &view.meta;
    let track_area = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - AXIS_HEIGHT));
    let track_count = meta.tracks.len();
    let geometry = track_geometry(state, track_count, track_area.height());
    let visible_tracks = meta
        .tracks
        .get(geometry.first_row..(geometry.first_row + geometry.rows_drawn).min(track_count))
        .unwrap_or(&[]);
    Layout {
        rect,
        track_area,
        data_tmin: meta.data_tmin,
        view_tmin: state.tmin,
        view_tmax: state.tmax,
        window_duration: (state.tmax - state.tmin).max(1e-9),
        visible_tracks,
        first_row: geometry.first_row,
        rows_drawn: geometry.rows_drawn,
        row_height: geometry.row_height,
        first_row_top: track_area.top() - geometry.first_row_clipped,
    }
}

#[cfg(test)]
mod tests {
    use snuffler_web_core::state::{AppState, DEFAULT_NTRACKS_SHOWN};

    use super::track_geometry;

    #[test]
    fn snapped_rows_tile_the_available_height() {
        let state = AppState {
            track_height: 40.0,
            snap_tracks: true,
            ..AppState::default()
        };
        let geometry = track_geometry(&state, 20, 205.0);
        assert_eq!(geometry.rows_drawn, 5);
        assert!((geometry.row_height - 41.0).abs() < f32::EPSILON);
        assert!(geometry.first_row_clipped.abs() < f32::EPSILON);
    }

    #[test]
    fn continuous_scroll_keeps_a_partial_first_row() {
        let state = AppState {
            track_height: 40.0,
            snap_tracks: false,
            track_scroll: 2.25,
            ..AppState::default()
        };
        let geometry = track_geometry(&state, 20, 100.0);
        assert_eq!(geometry.first_row, 2);
        assert_eq!(geometry.rows_drawn, 3);
        assert!((geometry.first_row_clipped - 10.0).abs() < f32::EPSILON);
    }

    /// A transient one-track view cannot turn automatic fitting into a one-track preference.
    #[test]
    fn automatic_height_survives_a_transient_one_track_view() {
        let mut state = AppState::default();
        let transient = track_geometry(&state, 1, 480.0);
        state.track_height = transient.track_height;

        let loaded = track_geometry(&state, 100, 480.0);

        assert!(state.track_height.abs() < f32::EPSILON);
        assert_eq!(loaded.rows_drawn, DEFAULT_NTRACKS_SHOWN);
    }
}
