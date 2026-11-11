//! Snuffler's window-jump arithmetic, shared by `goto` and the `g`/`G` keys.

/// A time window, in epoch seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeWindow {
    /// Lower edge.
    pub tmin: f64,
    /// Upper edge.
    pub tmax: f64,
}

/// What a window jump aims at, and what it measures its minimum width against.
#[derive(Clone, Copy, Debug)]
pub struct TimeJump {
    /// The span to show; reversed bounds are swapped.
    pub target: TimeWindow,
    /// The window on screen, which caps the minimum width the way Qt caps it.
    pub view: TimeWindow,
    /// The finest sampling interval of the loaded pile, in seconds; 0.0 when empty.
    pub deltatmin: f64,
    /// Samples per trace the minimum width is measured in (see
    /// [`crate::state::VisibleLength::jump_samples`]).
    pub visible_length_samples: f64,
    /// Zoom tightly around the target instead of flooring the width.
    pub tight: bool,
}

/// Snuffler's `make_good_looking_time_range` (`pile_viewer.py:2760`): pad a target span
/// by 10 % on each side, flooring every jump (`goto`, `g`, `G`) at one trace-visibility
/// length, `deltatmin * visible_length * 0.95`.
///
/// The floor becomes one second when nothing is loaded.
#[must_use]
pub fn good_looking_time_range(jump: TimeJump) -> TimeWindow {
    let TimeWindow { mut tmin, mut tmax } = jump.target;
    if tmax < tmin {
        std::mem::swap(&mut tmin, &mut tmax);
    }
    let mut min_width = jump.deltatmin * jump.visible_length_samples * 0.95;
    if min_width == 0.0 || !min_width.is_finite() {
        min_width = 1.0;
    }

    if jump.tight {
        // Tight around a real span pads it; tight around a point zooms in to one
        // visibility length (pile_viewer.py:2770).
        if tmax > tmin {
            let width = tmax - tmin;
            return TimeWindow {
                tmin: tmin - width * 0.1,
                tmax: tmax + width * 0.1,
            };
        }
        let center = f64::midpoint(tmin, tmax);
        return TimeWindow {
            tmin: center - 0.5 * min_width,
            tmax: center + 0.5 * min_width,
        };
    }

    if tmax - tmin < min_width {
        // Widen a narrow target to the floor, capped at the current view's width
        let min_width = min_width.min(jump.view.tmax - jump.view.tmin);
        let center = f64::midpoint(tmin, tmax);
        let (target_tmin, target_tmax) = (tmin, tmax);
        tmin = target_tmin.min(center - 0.5 * min_width);
        tmax = target_tmax.max(center + 0.5 * min_width);
        let width = tmax - tmin;
        // The margin goes only to the sides the floor did not already widen.
        if (target_tmin - tmin).abs() == 0.0 {
            tmin -= width * 0.1;
        }
        if (target_tmax - tmax).abs() == 0.0 {
            tmax += width * 0.1;
        }
    } else {
        let width = tmax - tmin;
        tmin -= width * 0.1;
        tmax += width * 0.1;
    }
    TimeWindow { tmin, tmax }
}
