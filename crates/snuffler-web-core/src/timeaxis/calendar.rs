//! Civil UTC calendar arithmetic on epoch seconds.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "calendar math casts between f64 epoch seconds and integer calendar fields"
)]

pub(super) const SECONDS_PER_DAY: f64 = 86_400.0;
/// Snuffler's 30-day month, used only to pick a tick increment.
pub(super) const SECONDS_PER_MONTH: f64 = 2_592_000.0;
/// Snuffler's 365-day year, used only to pick a tick increment.
pub(super) const SECONDS_PER_YEAR: f64 = 31_536_000.0;

/// Howard Hinnant's `civil_from_days`: (year, month, day) from days since 1970-01-01.
///
/// The short names are the algorithm's own: era, day of era, year of era, day of
/// year, and the March-based month.
fn days_to_civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// Hinnant's `days_from_civil`, the inverse of [`days_to_civil`].
pub(super) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A civil UTC timestamp split into calendar fields.
#[derive(Clone, Copy)]
pub(super) struct CivilTime {
    pub(super) year: i64,
    /// 1-based month.
    pub(super) month: u32,
    /// 1-based day of the month.
    pub(super) day: u32,
    pub(super) hour: u32,
    pub(super) minute: u32,
    /// Whole seconds within the minute.
    pub(super) second: u32,
    /// Milliseconds within the second (may be fractional), snuffler's `ms`.
    pub(super) millisecond: f64,
    /// Day of week, 0 = Monday (matching `tm_wday`).
    pub(super) weekday: u32,
}

impl CivilTime {
    pub(super) fn from_epoch(time: f64) -> Self {
        let whole_seconds = time.floor();
        let days = (whole_seconds / SECONDS_PER_DAY).floor() as i64;
        let mut second_of_day = (whole_seconds - days as f64 * SECONDS_PER_DAY) as i64;
        if second_of_day < 0 {
            second_of_day += 86_400;
        }
        let (year, month, day) = days_to_civil(days);
        // 1970-01-01 was a Thursday (tm_wday 3, Monday-based).
        let weekday = (days + 3).rem_euclid(7) as u32;
        Self {
            year,
            month,
            day,
            hour: (second_of_day / 3600) as u32,
            minute: ((second_of_day % 3600) / 60) as u32,
            second: (second_of_day % 60) as u32,
            millisecond: (time - whole_seconds) * 1000.0,
            weekday,
        }
    }
}

pub(super) fn day_start(time: f64) -> f64 {
    (time / SECONDS_PER_DAY).floor() * SECONDS_PER_DAY
}

pub(super) fn month_start(time: f64) -> f64 {
    let civil = CivilTime::from_epoch(time);
    days_from_civil(civil.year, civil.month, 1) as f64 * SECONDS_PER_DAY
}

pub(super) fn year_start(time: f64) -> f64 {
    let civil = CivilTime::from_epoch(time);
    days_from_civil(civil.year, 1, 1) as f64 * SECONDS_PER_DAY
}
