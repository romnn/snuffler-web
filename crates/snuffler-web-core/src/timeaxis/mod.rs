//! The time axis, transcribed from snuffler's `TimeScaler`/`TimeAx`
//! (`gui/snuffler/pile_viewer.py:253-670`): the same 18-value increment table
//! chosen by *nearest* entry to `span / approx_ticks`, the same calendar-anchored
//! tick walk (runs restart at each day start, months step from the 1st, years
//! from Jan 1), and the same five-level label ladder with context rows under the
//! ticks where a unit rolls over.
//!
//! One knowing simplification: snuffler switches to its `hpfloat` high-precision
//! time below millisecond increments; this stays `f64`, which drifts only below
//! the millisecond at large epochs.
//!
//! - this module: the tick increment and the tick walk;
//! - `labels`: what each tick says;
//! - `calendar`: civil UTC dates from epoch seconds.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the tick walk casts between f64 epoch seconds and integer years"
)]

mod calendar;
mod labels;

use calendar::{
    CivilTime, SECONDS_PER_DAY, SECONDS_PER_MONTH, SECONDS_PER_YEAR, day_start, days_from_civil,
    month_start, year_start,
};
pub use labels::{TickLabels, format_timestamp, l1_l2_tick, tick_to_labels};

/// Snuffler's `acceptable_tincs` (`pile_viewer.py:253`), verbatim, in seconds.
const ACCEPTABLE_INCREMENTS_S: &[f64] = &[
    1.0,
    2.0,
    5.0,
    10.0,
    20.0,
    30.0,
    60.0,
    300.0,
    600.0,
    1_200.0,
    1_800.0,
    3_600.0,
    10_800.0,
    21_600.0,
    43_200.0,
    SECONDS_PER_DAY,
    SECONDS_PER_MONTH,
    SECONDS_PER_YEAR,
];

/// The usable epoch range standing in for pyrocko's `working_system_time_range`:
/// year 1 to year 9999, wide enough that the clamp never bites on real data.
const WORKING_TMIN: f64 = -62_135_596_800.0;
const WORKING_TMAX: f64 = 253_402_300_800.0;

/// Every tick walk stops after this many steps, so a pathological window cannot spin.
const MAX_TICK_STEPS: usize = 100_000;

/// `pyrocko.plot.nice_value`: round to the nearest 1/2/5 decade step.
fn nice_value(value: f64) -> f64 {
    if value == 0.0 {
        return 0.0;
    }
    let (mut mantissa, sign) = if value < 0.0 {
        (-value, -1.0)
    } else {
        (value, 1.0)
    };
    // Normalize into 0.1..1 and remember the decade
    let mut decade = 1.0;
    while mantissa >= 1.0 {
        mantissa /= 10.0;
        decade *= 10.0;
    }
    while mantissa < 0.1 {
        mantissa *= 10.0;
        decade /= 10.0;
    }
    if mantissa >= 0.75 {
        return sign * decade;
    }
    if mantissa >= 0.35 {
        return sign * 0.5 * decade;
    }
    if mantissa >= 0.15 {
        return sign * 0.2 * decade;
    }
    sign * 0.1 * decade
}

/// Snuffler's `time_nice_value` (`pile_viewer.py:359`): the *nearest* table entry,
/// with 1/2/5 rounding below the table and nice years above it.
#[must_use]
pub fn time_nice_value(raw_increment: f64) -> f64 {
    let (Some(&first), Some(&last)) = (
        ACCEPTABLE_INCREMENTS_S.first(),
        ACCEPTABLE_INCREMENTS_S.last(),
    ) else {
        return nice_value(raw_increment);
    };
    if raw_increment < first {
        return nice_value(raw_increment);
    }
    if raw_increment > last {
        return nice_value(raw_increment / SECONDS_PER_YEAR) * SECONDS_PER_YEAR;
    }
    let mut nearest = first;
    let mut nearest_distance = f64::INFINITY;
    for &candidate in ACCEPTABLE_INCREMENTS_S {
        let distance = (candidate - raw_increment).abs();
        if distance < nearest_distance {
            nearest_distance = distance;
            nearest = candidate;
        }
    }
    nearest
}

/// The tick times of a window and the increment between them, in seconds.
pub struct Ticks {
    /// Tick times in epoch seconds, ascending.
    pub times: Vec<f64>,
    /// The increment the labels are formatted for.
    pub increment: f64,
}

/// Snuffler's `TimeScaler.make_scale` + `make_ticks` (`pile_viewer.py:374`, `:417`)
/// for the forward, `mode='min-max'`, `space=0` case the viewer uses.
///
/// The increment is [`time_nice_value`] of `span / approx_ticks` (7.0 in the viewer).
/// Tick times are anchored to calendar boundaries: sub-day runs restart at each day
/// start, day-scale steps walk whole days from the first day boundary, months from the
/// 1st, years from Jan 1 in whole multiples of the year increment.
#[must_use]
pub fn make_ticks(tmin: f64, tmax: f64, approx_ticks: f64) -> Ticks {
    let lower = tmin.min(tmax).max(WORKING_TMIN);
    let upper = tmin.max(tmax).min(WORKING_TMAX);
    let mut increment = if approx_ticks > 0.0 {
        time_nice_value((upper - lower) / approx_ticks)
    } else {
        time_nice_value((upper - lower) * 10.0)
    };
    if increment == 0.0 {
        increment = 1.0;
    }

    let times = if increment < SECONDS_PER_DAY {
        sub_day_ticks(lower, upper, increment)
    } else if increment < SECONDS_PER_MONTH {
        day_ticks(lower, upper, increment)
    } else if increment < SECONDS_PER_YEAR {
        month_ticks(lower, upper)
    } else {
        year_ticks(lower, upper, increment)
    };
    Ticks { times, increment }
}

/// Multiples of `increment` from each day start, so ticks stay aligned to the
/// calendar day rather than drifting across it.
fn sub_day_ticks(lower: f64, upper: f64, increment: f64) -> Vec<f64> {
    let first_day = day_start(lower.max(WORKING_TMIN + SECONDS_PER_DAY * 1.5));
    let mut run_start = first_day + ((lower - first_day) / increment).ceil() * increment;
    let mut run_day = first_day;
    let mut step: u64 = 0;
    let mut ticks = Vec::new();
    for _ in 0..MAX_TICK_STEPS {
        let tick = run_start + step as f64 * increment;
        if tick > upper {
            break;
        }
        let tick_day = day_start(tick);
        if tick_day > run_day {
            // The run restarts at each day boundary.
            run_day = tick_day;
            run_start = run_day;
            step = 0;
        } else {
            ticks.push(tick);
            step += 1;
        }
    }
    ticks
}

/// Whole days from the first day boundary at or after `lower`.
fn day_ticks(lower: f64, upper: f64, increment: f64) -> Vec<f64> {
    let first_day = day_start(lower.max(WORKING_TMIN + SECONDS_PER_DAY * 1.5));
    let step = (increment / SECONDS_PER_DAY).round().max(1.0) * SECONDS_PER_DAY;
    let mut start = first_day;
    if (first_day - lower).abs() == 0.0 {
        start += step;
    }
    (0_u64..)
        .take(MAX_TICK_STEPS)
        .map(|index| start + index as f64 * step)
        .take_while(|tick| *tick <= upper)
        .collect()
}

/// The 1st of every month.
fn month_ticks(lower: f64, upper: f64) -> Vec<f64> {
    let first_month = month_start(lower.max(WORKING_TMIN + SECONDS_PER_MONTH * 1.5));
    let civil = CivilTime::from_epoch(first_month);
    let (mut year, mut month) = (civil.year, civil.month);
    let mut ticks = Vec::new();
    for _ in 0..MAX_TICK_STEPS {
        let tick = days_from_civil(year, month, 1) as f64 * SECONDS_PER_DAY;
        month += 1;
        if month > 12 {
            year += 1;
            month = 1;
        }
        if tick > upper {
            break;
        }
        if tick >= lower {
            ticks.push(tick);
        }
    }
    ticks
}

/// Jan 1 of every year that is a whole multiple of the year increment.
fn year_ticks(lower: f64, upper: f64, increment: f64) -> Vec<f64> {
    let first_year = year_start(lower.max(WORKING_TMIN + SECONDS_PER_YEAR * 1.5));
    let years_per_tick = (increment / SECONDS_PER_YEAR).round().max(1.0) as i64;
    let mut year = ((CivilTime::from_epoch(first_year).year as f64) / years_per_tick as f64).ceil()
        as i64
        * years_per_tick;
    let mut ticks = Vec::new();
    for _ in 0..MAX_TICK_STEPS {
        let tick = days_from_civil(year, 1, 1) as f64 * SECONDS_PER_DAY;
        year += years_per_tick;
        if tick > upper {
            break;
        }
        if tick >= lower {
            ticks.push(tick);
        }
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::calendar::{
        CivilTime, SECONDS_PER_DAY, SECONDS_PER_MONTH, SECONDS_PER_YEAR, day_start,
    };
    use super::{l1_l2_tick, make_ticks, tick_to_labels};

    // 2023-11-14 is inside every corpus window; spot values verified against
    // snuffler's own TimeScaler in CPython.
    const T0: f64 = 1_700_000_000.0; // 2023-11-14 22:13:20 UTC

    #[test]
    fn increment_is_the_nearest_table_entry() {
        // span 60 s / 7 ≈ 8.57 → nearest of {5, 10} is 10 (Qt gives 10 s ticks
        // on a 60 s window where a first-at-or-above rule gave 5).
        let ticks = make_ticks(T0, T0 + 60.0, 7.0);
        assert!((ticks.increment - 10.0).abs() < 1e-9);
        // span 600 s / 7 ≈ 85.7 → nearest of {60, 300} is 60.
        let ticks = make_ticks(T0, T0 + 600.0, 7.0);
        assert!((ticks.increment - 60.0).abs() < 1e-9);
    }

    #[test]
    fn subday_ticks_are_multiples_of_inc_from_day_start() {
        let ticks = make_ticks(T0, T0 + 60.0, 7.0);
        let increment = ticks.increment;
        assert!((increment - 10.0).abs() < 1e-9);
        for tick in &ticks.times {
            let from_day = (tick - day_start(*tick)) % increment;
            assert!(from_day.abs() < 1e-6 || (increment - from_day).abs() < 1e-6);
        }
        assert_ne!(ticks.times, Vec::<f64>::new());
    }

    #[test]
    fn month_ticks_land_on_the_first() {
        let ticks = make_ticks(T0, T0 + SECONDS_PER_YEAR * 0.5, 7.0);
        assert!((ticks.increment - SECONDS_PER_MONTH).abs() < 1e-6);
        for tick in &ticks.times {
            let civil = CivilTime::from_epoch(*tick);
            assert_eq!(
                (civil.day, civil.hour, civil.minute, civil.second),
                (1, 0, 0, 0)
            );
        }
    }

    #[test]
    fn labels_follow_the_zoom_ladder() {
        // Minute-scale: HH:MM, with the date row only at midnight.
        let labels = tick_to_labels(1_699_920_000.0, 600.0); // 2023-11-14 00:00
        assert_eq!(labels.l0, "00:00");
        assert_eq!(labels.l1.as_deref(), Some("Nov 14, 2023"));
        let labels = tick_to_labels(1_699_920_000.0 + 600.0, 600.0);
        assert!(labels.l1.is_none());
        // Sub-second: the fraction alone, three digits at millisecond ticks.
        let labels = tick_to_labels(T0 + 0.25, 0.05);
        assert_eq!(labels.l0, ".250");
        // Day scale: weekday + day, brief form is the day alone, centred.
        let labels = tick_to_labels(1_699_920_000.0, SECONDS_PER_DAY);
        assert_eq!(labels.l0, "Tue 14");
        assert_eq!(labels.l0_brief, "14");
        assert!(labels.l0_center);
    }

    #[test]
    fn fallback_context_is_unconditional() {
        // The left-edge fallback rows carry the date whether or not the tick
        // rolls anything over; the third row exists only at sub-second zooms.
        let (l1, l2) = l1_l2_tick(T0, 10.0);
        assert_eq!(l1, "Nov 14, 2023");
        assert_eq!(l2, "");
        let (l1, l2) = l1_l2_tick(T0, 0.05);
        assert_eq!(l1, "22:13:20");
        assert_eq!(l2, "Nov 14, 2023");
    }
}
