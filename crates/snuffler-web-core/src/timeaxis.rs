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
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "tick/label math casts between f64 epoch seconds and integer calendar fields"
)]

const SDAY: f64 = 86_400.0;
const SMONTH: f64 = 2_592_000.0;
const SYEAR: f64 = 31_536_000.0;

/// Snuffler's `acceptable_tincs` (`pile_viewer.py:253`), verbatim.
const ACCEPTABLE: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0, 300.0, 600.0, 1_200.0, 1_800.0, 3_600.0, 10_800.0,
    21_600.0, 43_200.0, SDAY, SMONTH, SYEAR,
];

/// The usable epoch range standing in for pyrocko's `working_system_time_range`:
/// year 1 to year 9999, wide enough that the clamp never bites on real data.
const WORK_TMIN: f64 = -62_135_596_800.0;
const WORK_TMAX: f64 = 253_402_300_800.0;

/// `pyrocko.plot.nice_value`: round to the nearest 1/2/5 decade step.
fn nice_value(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let (mut x, sign) = if x < 0.0 { (-x, -1.0) } else { (x, 1.0) };
    let mut exp = 1.0;
    while x >= 1.0 {
        x /= 10.0;
        exp *= 10.0;
    }
    while x < 0.1 {
        x *= 10.0;
        exp /= 10.0;
    }
    if x >= 0.75 {
        return sign * exp;
    }
    if x >= 0.35 {
        return sign * 0.5 * exp;
    }
    if x >= 0.15 {
        return sign * 0.2 * exp;
    }
    sign * 0.1 * exp
}

/// Snuffler's `time_nice_value` (`pile_viewer.py:359`): the *nearest* table entry,
/// with 1/2/5 rounding below the table and nice years above it.
#[must_use]
pub fn time_nice_value(inc0: f64) -> f64 {
    let (Some(&first), Some(&last)) = (ACCEPTABLE.first(), ACCEPTABLE.last()) else {
        return nice_value(inc0);
    };
    if inc0 < first {
        return nice_value(inc0);
    }
    if inc0 > last {
        return nice_value(inc0 / SYEAR) * SYEAR;
    }
    let mut best = first;
    let mut best_diff = f64::INFINITY;
    for &candidate in ACCEPTABLE {
        let diff = (candidate - inc0).abs();
        if diff < best_diff {
            best_diff = diff;
            best = candidate;
        }
    }
    best
}

/// Howard Hinnant's `civil_from_days`: civil date from days since 1970-01-01.
fn days_to_civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

/// Hinnant's `days_from_civil`, the inverse of [`days_to_civil`].
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A civil UTC timestamp split into calendar fields.
#[derive(Clone, Copy)]
struct Civil {
    year: i64,
    mon: u32,
    day: u32,
    hour: u32,
    min: u32,
    /// Whole seconds within the minute.
    sec: u32,
    /// Milliseconds within the second (may be fractional), snuffler's `ms`.
    ms: f64,
    /// Day of week, 0 = Monday (matching `tm_wday`).
    wday: u32,
}

fn civil(t: f64) -> Civil {
    let et = t.floor();
    let days = (et / SDAY).floor() as i64;
    let mut rem = (et - days as f64 * SDAY) as i64;
    if rem < 0 {
        rem += 86_400;
    }
    let (year, mon, day) = days_to_civil(days);
    // 1970-01-01 was a Thursday (tm_wday 3, Monday-based).
    let wday = (days + 3).rem_euclid(7) as u32;
    Civil {
        year,
        mon,
        day,
        hour: (rem / 3600) as u32,
        min: ((rem % 3600) / 60) as u32,
        sec: (rem % 60) as u32,
        ms: (t - et) * 1000.0,
        wday,
    }
}

fn day_start(t: f64) -> f64 {
    (t / SDAY).floor() * SDAY
}

fn month_start(t: f64) -> f64 {
    let c = civil(t);
    days_from_civil(c.year, c.mon, 1) as f64 * SDAY
}

fn year_start(t: f64) -> f64 {
    let c = civil(t);
    days_from_civil(c.year, 1, 1) as f64 * SDAY
}

/// Snuffler's `TimeScaler.make_scale` + `make_ticks` (`pile_viewer.py:374`, `:417`)
/// for the forward, `mode='min-max'`, `space=0` case the viewer uses: the tick
/// increment by [`time_nice_value`] of `span / approx_ticks` (7.0 in the
/// viewer), and tick times anchored to calendar boundaries — sub-day runs
/// restart at each day start, day-scale steps walk whole days from the first
/// day boundary, months from the 1st, years from Jan 1 in whole-`incy` years.
#[must_use]
pub fn make_ticks(tmin: f64, tmax: f64, approx_ticks: f64) -> (Vec<f64>, f64) {
    let mi = tmin.min(tmax).max(WORK_TMIN);
    let ma = tmin.max(tmax).min(WORK_TMAX);
    let mut inc = if approx_ticks > 0.0 {
        time_nice_value((ma - mi) / approx_ticks)
    } else {
        time_nice_value((ma - mi) * 10.0)
    };
    if inc == 0.0 {
        inc = 1.0;
    }

    let mut ticks = Vec::new();
    if inc < SDAY {
        let mi_day = day_start(mi.max(WORK_TMIN + SDAY * 1.5));
        let mut base = mi_day + ((mi - mi_day) / inc).ceil() * inc;
        let mut base_day = mi_day;
        let mut i: u64 = 0;
        // Bounded like every walk here: a pathological window cannot spin.
        for _ in 0..100_000 {
            let tick = base + i as f64 * inc;
            if tick > ma {
                break;
            }
            let tick_day = day_start(tick);
            if tick_day > base_day {
                // The run restarts at each day boundary, so ticks stay aligned
                // to the calendar day rather than drifting across it.
                base_day = tick_day;
                base = base_day;
                i = 0;
            } else {
                ticks.push(tick);
                i += 1;
            }
        }
    } else if inc < SMONTH {
        let mi_day = day_start(mi.max(WORK_TMIN + SDAY * 1.5));
        let delta = (inc / SDAY).round().max(1.0) * SDAY;
        let mut base = mi_day;
        if (mi_day - mi).abs() == 0.0 {
            base += delta;
        }
        for i in (0_u64..).take(100_000) {
            let tick = base + i as f64 * delta;
            if tick > ma {
                break;
            }
            ticks.push(tick);
        }
    } else if inc < SYEAR {
        let mi_month = month_start(mi.max(WORK_TMIN + SMONTH * 1.5));
        let c = civil(mi_month);
        let (mut y, mut m) = (c.year, c.mon);
        for _ in 0..100_000 {
            let tick = days_from_civil(y, m, 1) as f64 * SDAY;
            m += 1;
            if m > 12 {
                y += 1;
                m = 1;
            }
            if tick > ma {
                break;
            }
            if tick >= mi {
                ticks.push(tick);
            }
        }
    } else {
        let mi_year = year_start(mi.max(WORK_TMIN + SYEAR * 1.5));
        let incy = (inc / SYEAR).round().max(1.0) as i64;
        let mut y = ((civil(mi_year).year as f64) / incy as f64).ceil() as i64 * incy;
        for _ in 0..100_000 {
            let tick = days_from_civil(y, 1, 1) as f64 * SDAY;
            y += incy;
            if tick > ma {
                break;
            }
            if tick >= mi {
                ticks.push(tick);
            }
        }
    }
    (ticks, inc)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn month_name(mon: u32) -> &'static str {
    MONTHS
        .get(mon.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("???")
}

/// The label formats for one tick increment: snuffler's `fancy_time_ax_format`
/// (`pile_viewer.py:281`), as an enum over its five-plus zoom levels.
struct AxFormat {
    /// Fractional-second digits when the l0 label is a bare fraction; None for
    /// the calendar formats.
    frac_digits: Option<u32>,
    l0_kind: L0,
    l0_center: bool,
    l1_kind: L1,
    /// `l1_trig`: the calendar position that must be at its start for the
    /// context row to appear under this tick (6 = every whole second, 3 =
    /// midnight, 2 = month start, 1 = year start, 0 = never).
    l1_trig: u32,
    l2_trig: u32,
}

enum L0 {
    Frac,
    Hms,
    Hm,
    WdayDay,
    Month,
    Year,
}

enum L1 {
    /// `%H:%M:%S` — the seconds row under a sub-second axis.
    Hms,
    MonthDayYear,
    MonthYear,
    Year,
    None,
}

fn ax_format(inc: f64) -> AxFormat {
    if inc < 1e-6 {
        AxFormat {
            frac_digits: Some(9),
            l0_kind: L0::Frac,
            l0_center: false,
            l1_kind: L1::Hms,
            l1_trig: 6,
            l2_trig: 3,
        }
    } else if inc < 1e-3 {
        AxFormat {
            frac_digits: Some(6),
            l0_kind: L0::Frac,
            l0_center: false,
            l1_kind: L1::Hms,
            l1_trig: 6,
            l2_trig: 3,
        }
    } else if inc < 1.0 {
        AxFormat {
            frac_digits: Some(3),
            l0_kind: L0::Frac,
            l0_center: false,
            l1_kind: L1::Hms,
            l1_trig: 6,
            l2_trig: 3,
        }
    } else if inc < 60.0 {
        AxFormat {
            frac_digits: None,
            l0_kind: L0::Hms,
            l0_center: false,
            l1_kind: L1::MonthDayYear,
            l1_trig: 3,
            l2_trig: 0,
        }
    } else if inc < SDAY {
        AxFormat {
            frac_digits: None,
            l0_kind: L0::Hm,
            l0_center: false,
            l1_kind: L1::MonthDayYear,
            l1_trig: 3,
            l2_trig: 0,
        }
    } else if inc < SMONTH {
        AxFormat {
            frac_digits: None,
            l0_kind: L0::WdayDay,
            l0_center: true,
            l1_kind: L1::MonthYear,
            l1_trig: 2,
            l2_trig: 0,
        }
    } else if inc < SYEAR {
        AxFormat {
            frac_digits: None,
            l0_kind: L0::Month,
            l0_center: true,
            l1_kind: L1::Year,
            l1_trig: 1,
            l2_trig: 0,
        }
    } else {
        AxFormat {
            frac_digits: None,
            l0_kind: L0::Year,
            l0_center: false,
            l1_kind: L1::None,
            l1_trig: 0,
            l2_trig: 0,
        }
    }
}

/// `need_l1_tick` (`pile_viewer.py:505`): whether the calendar fields from `trig`
/// onward sit at their start values, i.e. the unit rolls over at this tick.
fn rolls_over(c: &Civil, trig: u32) -> bool {
    if trig == 0 {
        return false;
    }
    if c.ms.abs() > 0.0 {
        return false;
    }
    let fields = [
        0_i64,
        i64::from(c.mon),
        i64::from(c.day),
        i64::from(c.hour),
        i64::from(c.min),
        i64::from(c.sec),
    ];
    let starts = [0_i64, 1, 1, 0, 0, 0];
    fields
        .iter()
        .zip(&starts)
        .skip(trig as usize)
        .all(|(field, start)| field == start)
}

fn format_l0(c: &Civil, fmt: &AxFormat, brief: bool) -> String {
    match fmt.l0_kind {
        L0::Frac => {
            let digits = fmt.frac_digits.unwrap_or(3);
            // Snuffler's `.%r`/`.%u`/`.%n`: the fractional part alone.
            let scale = 10_f64.powi(digits.cast_signed()) / 1000.0;
            let value = (c.ms * scale).round() as u64;
            format!(".{value:0width$}", width = digits as usize)
        }
        L0::Hms => format!("{:02}:{:02}:{:02}", c.hour, c.min, c.sec),
        L0::Hm => format!("{:02}:{:02}", c.hour, c.min),
        L0::WdayDay => {
            if brief {
                format!("{:02}", c.day)
            } else {
                format!(
                    "{} {:02}",
                    WEEKDAYS.get(c.wday as usize).copied().unwrap_or("???"),
                    c.day
                )
            }
        }
        L0::Month => month_name(c.mon).to_owned(),
        L0::Year => format!("{}", c.year),
    }
}

fn format_l1(c: &Civil, kind: &L1) -> String {
    match kind {
        L1::Hms => format!("{:02}:{:02}:{:02}", c.hour, c.min, c.sec),
        L1::MonthDayYear => format!("{} {:02}, {}", month_name(c.mon), c.day, c.year),
        L1::MonthYear => format!("{}, {}", month_name(c.mon), c.year),
        L1::Year => format!("{}", c.year),
        L1::None => String::new(),
    }
}

/// One tick's labels, snuffler's `tick_to_labels` (`pile_viewer.py:509`): the main
/// label (with its brief form and centring flag) plus the context rows that
/// appear only under a tick where the respective unit rolls over.
pub struct TickLabels {
    /// The main label under the tick.
    pub l0: String,
    /// The abbreviated form the ladder falls back to when `l0` will not fit.
    pub l0_brief: String,
    /// Whether the label is centred *between* this tick and the next.
    pub l0_center: bool,
    /// Second row: date context, present only at the rollover tick.
    pub l1: Option<String>,
    /// Third row (sub-second zooms only): full date, at midnight ticks.
    pub l2: Option<String>,
}

/// Labels for one tick at increment `inc`.
#[must_use]
pub fn tick_to_labels(tick: f64, inc: f64) -> TickLabels {
    let fmt = ax_format(inc);
    let c = civil(tick);
    let l1 = rolls_over(&c, fmt.l1_trig).then(|| format_l1(&c, &fmt.l1_kind));
    let l2 = rolls_over(&c, fmt.l2_trig).then(|| format_l1(&c, &L1::MonthDayYear));
    TickLabels {
        l0: format_l0(&c, &fmt, false),
        l0_brief: if matches!(fmt.l0_kind, L0::WdayDay) {
            format_l0(&c, &fmt, true)
        } else {
            String::new()
        },
        l0_center: fmt.l0_center,
        l1,
        l2,
    }
}

/// The context labels a tick *would* carry, rollover or not — snuffler's
/// `l1_l2_tick`, used for the left-edge fallback row when no on-screen tick
/// triggered one.
#[must_use]
pub fn l1_l2_tick(tick: f64, inc: f64) -> (String, String) {
    let fmt = ax_format(inc);
    let c = civil(tick);
    let l2 = if fmt.l2_trig > 0 {
        format_l1(&c, &L1::MonthDayYear)
    } else {
        String::new()
    };
    (format_l1(&c, &fmt.l1_kind), l2)
}

/// Full UTC timestamp (millisecond precision) for marker lists / readouts.
#[must_use]
pub fn format_full(t: f64) -> String {
    let c = civil(t);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:06.3}",
        c.year,
        c.mon,
        c.day,
        c.hour,
        c.min,
        f64::from(c.sec) + c.ms / 1000.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2023-11-14 is inside every corpus window; spot values verified against
    // snuffler's own TimeScaler in CPython.
    const T0: f64 = 1_700_000_000.0; // 2023-11-14 22:13:20 UTC

    #[test]
    fn increment_is_the_nearest_table_entry() {
        // span 60 s / 7 ≈ 8.57 → nearest of {5, 10} is 10 (Qt gives 10 s ticks
        // on a 60 s window where a first-at-or-above rule gave 5).
        let (_, inc) = make_ticks(T0, T0 + 60.0, 7.0);
        assert!((inc - 10.0).abs() < 1e-9);
        // span 600 s / 7 ≈ 85.7 → nearest of {60, 300} is 60.
        let (_, inc) = make_ticks(T0, T0 + 600.0, 7.0);
        assert!((inc - 60.0).abs() < 1e-9);
    }

    #[test]
    fn subday_ticks_are_multiples_of_inc_from_day_start() {
        let (ticks, inc) = make_ticks(T0, T0 + 60.0, 7.0);
        assert!((inc - 10.0).abs() < 1e-9);
        for t in &ticks {
            let from_day = (t - day_start(*t)) % inc;
            assert!(from_day.abs() < 1e-6 || (inc - from_day).abs() < 1e-6);
        }
        assert_ne!(ticks, Vec::<f64>::new());
    }

    #[test]
    fn month_ticks_land_on_the_first() {
        let (ticks, inc) = make_ticks(T0, T0 + SYEAR * 0.5, 7.0);
        assert!((inc - SMONTH).abs() < 1e-6);
        for t in &ticks {
            let c = civil(*t);
            assert_eq!((c.day, c.hour, c.min, c.sec), (1, 0, 0, 0));
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
        let labels = tick_to_labels(1_699_920_000.0, SDAY);
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
