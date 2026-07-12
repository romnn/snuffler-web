//! Toolkit-neutral waveform and track layout.

use emath::{Rect, pos2};
use snuffler_web_core::bridge::{ViewData, ViewTrack};
use snuffler_web_core::state::{AppState, DEFAULT_NTRACKS_SHOWN, MIN_TRACK_H};

/// Height reserved below the track rows for the shared time axis.
pub const AXIS_H: f32 = 52.0;

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
    /// Duration of the visible time window.
    pub window: f64,
    /// Tracks intersecting the visible vertical window.
    pub visible: &'a [ViewTrack],
    /// Index of the first visible track.
    pub off: usize,
    /// Number of tracks included in the layout.
    pub n_shown: usize,
    /// Resolved height of one row in logical points.
    pub row_h: f32,
    /// Top edge of the first row, which may be above the clipped track area.
    pub top0: f32,
}

/// Vertical row geometry resolved from track-height, snapping, and scroll state.
pub struct TrackGeom {
    /// Resolved height of one row in logical points.
    pub row_h: f32,
    /// Index of the first row to draw.
    pub off: usize,
    /// Number of rows needed to cover the track area.
    pub n_draw: usize,
    /// Portion of the first row clipped above the track area.
    pub top_shift: f32,
    /// Clamped fractional row offset.
    pub scroll: f32,
    /// Track height to retain in application state.
    pub height: f32,
}

/// Resolves vertical track controls into concrete row geometry.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "bounded row counts and logical pixel sizes are converted between indices and f32 geometry"
)]
pub fn track_geometry(state: &AppState, total: usize, area_h: f32) -> TrackGeom {
    let area = area_h.max(1.0);
    let total_f = total.max(1) as f32;
    let mut height = if state.track_height > 0.0 {
        state.track_height
    } else {
        area / DEFAULT_NTRACKS_SHOWN as f32
    };
    height = height.clamp(MIN_TRACK_H, area.max(MIN_TRACK_H));

    if state.snap_tracks {
        let n = (area / height).round().clamp(1.0, total_f) as usize;
        let row_h = area / n as f32;
        let max_off = total.saturating_sub(n);
        let scroll = state.track_scroll.clamp(0.0, max_off as f32);
        let off = scroll.round().clamp(0.0, max_off as f32) as usize;
        let n_draw = n.min(total.saturating_sub(off));
        TrackGeom {
            row_h,
            off,
            n_draw,
            top_shift: 0.0,
            scroll,
            height: row_h,
        }
    } else {
        let row_h = height;
        let visible_f = area / row_h;
        let max_scroll = (total_f - visible_f).max(0.0);
        let scroll = state.track_scroll.clamp(0.0, max_scroll);
        let off = scroll.floor() as usize;
        let top_shift = (scroll - off as f32) * row_h;
        let n_draw = ((top_shift + area) / row_h).ceil() as usize;
        let n_draw = n_draw.min(total.saturating_sub(off));
        TrackGeom {
            row_h,
            off,
            n_draw,
            top_shift,
            scroll,
            height,
        }
    }
}

/// Computes the horizontal time window and vertical track window for a frame.
#[must_use]
pub fn compute_layout<'a>(state: &AppState, view: &'a ViewData, rect: Rect) -> Layout<'a> {
    let meta = &view.meta;
    let track_area = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - AXIS_H));
    let window = (state.tmax - state.tmin).max(1e-9);
    let total = meta.tracks.len();
    let geometry = track_geometry(state, total, track_area.height());
    let visible = meta
        .tracks
        .get(geometry.off..(geometry.off + geometry.n_draw).min(total))
        .unwrap_or(&[]);
    Layout {
        rect,
        track_area,
        data_tmin: meta.view_tmin,
        view_tmin: state.tmin,
        view_tmax: state.tmax,
        window,
        visible,
        off: geometry.off,
        n_shown: geometry.n_draw,
        row_h: geometry.row_h,
        top0: track_area.top() - geometry.top_shift,
    }
}

#[cfg(test)]
mod tests {
    use snuffler_web_core::state::AppState;

    use super::track_geometry;

    #[test]
    fn snapped_rows_tile_the_available_height() {
        let state = AppState {
            track_height: 40.0,
            snap_tracks: true,
            ..AppState::default()
        };
        let geometry = track_geometry(&state, 20, 205.0);
        assert_eq!(geometry.n_draw, 5);
        assert!((geometry.row_h - 41.0).abs() < f32::EPSILON);
        assert!(geometry.top_shift.abs() < f32::EPSILON);
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
        assert_eq!(geometry.off, 2);
        assert_eq!(geometry.n_draw, 3);
        assert!((geometry.top_shift - 10.0).abs() < f32::EPSILON);
    }
}
