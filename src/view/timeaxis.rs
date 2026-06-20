//! Adaptive time-axis ticks & labels, in the spirit of snuffler's `TimeAx`
//! (`gui/snuffler/pile_viewer.py`): pick a human-friendly tick increment for
//! the visible span and format labels at a precision that matches it.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "tick/label math casts between f64 epoch seconds and integer calendar fields"
)]

/// Tick increments snuffler considers "acceptable" (seconds), coarse subset.
const ACCEPTABLE: &[f64] = &[
    1e-3,
    2e-3,
    5e-3,
    1e-2,
    2e-2,
    5e-2,
    0.1,
    0.2,
    0.5,
    1.0,
    2.0,
    5.0,
    10.0,
    20.0,
    30.0,
    60.0,
    120.0,
    300.0,
    600.0,
    900.0,
    1800.0,
    3600.0,
    7200.0,
    10_800.0,
    21_600.0,
    43_200.0,
    86_400.0,
    172_800.0,
    432_000.0,
    864_000.0,
    2_592_000.0,
    7_776_000.0,
    31_536_000.0,
];

/// Choose a tick increment so there are roughly `target_ticks` ticks across `span`.
pub fn nice_tinc(span: f64, target_ticks: f64) -> f64 {
    let ideal = (span / target_ticks).max(1e-6);
    for &c in ACCEPTABLE {
        if c >= ideal {
            return c;
        }
    }
    ACCEPTABLE.last().copied().unwrap_or(ideal)
}

/// Iterate tick times (multiples of `tinc`) within `[t0, t1]`.
pub fn ticks(t0: f64, t1: f64, tinc: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if tinc <= 0.0 || !t0.is_finite() || !t1.is_finite() {
        return out;
    }
    let start = (t0 / tinc).ceil() * tinc;
    let mut t = start;
    let mut guard = 0;
    while t <= t1 && guard < 10_000 {
        out.push(t);
        t += tinc;
        guard += 1;
    }
    out
}

/// Civil date (year, month, day) from a day number relative to 1970-01-01.
/// Howard Hinnant's `civil_from_days`.
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

struct Civil {
    year: i64,
    mon: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: f64,
}

fn civil(t: f64) -> Civil {
    let days = (t / 86400.0).floor() as i64;
    let mut rem = t - days as f64 * 86400.0;
    if rem < 0.0 {
        rem += 86400.0;
    }
    let (year, mon, day) = days_to_civil(days);
    let hour = (rem / 3600.0).floor() as u32;
    let min = ((rem - f64::from(hour) * 3600.0) / 60.0).floor() as u32;
    let sec = rem - f64::from(hour) * 3600.0 - f64::from(min) * 60.0;
    Civil {
        year,
        mon,
        day,
        hour,
        min,
        sec,
    }
}

/// A short tick label whose precision matches the tick increment.
pub fn tick_label(t: f64, tinc: f64) -> String {
    let c = civil(t);
    if tinc < 1.0 {
        // sub-second
        format!("{:02}:{:02}:{:06.3}", c.hour, c.min, c.sec)
    } else if tinc < 60.0 {
        format!("{:02}:{:02}:{:02}", c.hour, c.min, c.sec as u32)
    } else if tinc < 86400.0 {
        format!("{:02}:{:02}", c.hour, c.min)
    } else {
        format!("{:04}-{:02}-{:02}", c.year, c.mon, c.day)
    }
}

/// Full UTC timestamp (millisecond precision) for marker lists / readouts.
pub fn format_full(t: f64) -> String {
    let c = civil(t);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:06.3}",
        c.year, c.mon, c.day, c.hour, c.min, c.sec
    )
}

/// Date context for the visible time axis, matching snuffler's `%b %d, %Y`.
pub fn context_label(t: f64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let c = civil(t);
    let month = MONTHS
        .get(c.mon.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("???");
    format!("{month} {:02}, {:04}", c.day, c.year)
}
