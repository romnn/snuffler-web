//! Tick labels: snuffler's five-level label ladder (`fancy_time_ax_format`,
//! `pile_viewer.py:281`), with context rows under the ticks where a unit rolls over.
//!
//! Snuffler's names are kept for the rows: `l0` is the main label under every tick,
//! `l1` the context row (the date under a time axis), `l2` the full date under a
//! sub-second axis.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "label math casts between f64 milliseconds and integer calendar fields"
)]

use super::calendar::{CivilTime, SECONDS_PER_DAY, SECONDS_PER_MONTH, SECONDS_PER_YEAR};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn month_name(month: u32) -> &'static str {
    MONTHS
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("???")
}

/// The calendar unit whose start a context row marks: the row appears under a tick
/// where a new one of these begins.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RolloverField {
    /// No row.
    Never,
    Year,
    Month,
    Day,
    Second,
}

impl RolloverField {
    /// Snuffler's numeric trigger: the index into (year, month, day, hour, minute, second).
    const fn index(self) -> usize {
        match self {
            Self::Never => 0,
            Self::Year => 1,
            Self::Month => 2,
            Self::Day => 3,
            Self::Second => 6,
        }
    }
}

/// What the main label under a tick shows.
#[derive(Clone, Copy)]
enum MainLabel {
    /// The fractional second alone, with this many digits.
    Fraction {
        digits: u32,
    },
    HourMinuteSecond,
    HourMinute,
    WeekdayDay,
    Month,
    Year,
}

/// What a context row shows.
#[derive(Clone, Copy)]
enum ContextLabel {
    /// `%H:%M:%S`, the seconds row under a sub-second axis.
    HourMinuteSecond,
    MonthDayYear,
    MonthYear,
    Year,
    None,
}

/// The label formats for one tick increment.
struct AxisLabelFormat {
    main: MainLabel,
    /// Whether the main label is centred between this tick and the next.
    main_centered: bool,
    context: ContextLabel,
    /// When the `l1` context row appears.
    context_rollover: RolloverField,
    /// When the `l2` full-date row appears (sub-second zooms only).
    date_rollover: RolloverField,
}

/// Snuffler's `fancy_time_ax_format`: the label formats for a tick increment.
fn axis_label_format(increment: f64) -> AxisLabelFormat {
    let sub_second = |digits| AxisLabelFormat {
        main: MainLabel::Fraction { digits },
        main_centered: false,
        context: ContextLabel::HourMinuteSecond,
        context_rollover: RolloverField::Second,
        date_rollover: RolloverField::Day,
    };
    if increment < 1e-6 {
        sub_second(9)
    } else if increment < 1e-3 {
        sub_second(6)
    } else if increment < 1.0 {
        sub_second(3)
    } else if increment < 60.0 {
        AxisLabelFormat {
            main: MainLabel::HourMinuteSecond,
            main_centered: false,
            context: ContextLabel::MonthDayYear,
            context_rollover: RolloverField::Day,
            date_rollover: RolloverField::Never,
        }
    } else if increment < SECONDS_PER_DAY {
        AxisLabelFormat {
            main: MainLabel::HourMinute,
            main_centered: false,
            context: ContextLabel::MonthDayYear,
            context_rollover: RolloverField::Day,
            date_rollover: RolloverField::Never,
        }
    } else if increment < SECONDS_PER_MONTH {
        AxisLabelFormat {
            main: MainLabel::WeekdayDay,
            main_centered: true,
            context: ContextLabel::MonthYear,
            context_rollover: RolloverField::Month,
            date_rollover: RolloverField::Never,
        }
    } else if increment < SECONDS_PER_YEAR {
        AxisLabelFormat {
            main: MainLabel::Month,
            main_centered: true,
            context: ContextLabel::Year,
            context_rollover: RolloverField::Year,
            date_rollover: RolloverField::Never,
        }
    } else {
        AxisLabelFormat {
            main: MainLabel::Year,
            main_centered: false,
            context: ContextLabel::None,
            context_rollover: RolloverField::Never,
            date_rollover: RolloverField::Never,
        }
    }
}

/// `need_l1_tick` (`pile_viewer.py:505`): whether the calendar fields from `field`
/// onward sit at their start values, i.e. the unit rolls over at this tick.
fn rolls_over(civil: &CivilTime, field: RolloverField) -> bool {
    if field == RolloverField::Never || civil.millisecond.abs() > 0.0 {
        return false;
    }
    let fields = [
        0_i64,
        i64::from(civil.month),
        i64::from(civil.day),
        i64::from(civil.hour),
        i64::from(civil.minute),
        i64::from(civil.second),
    ];
    let starts = [0_i64, 1, 1, 0, 0, 0];
    fields
        .iter()
        .zip(&starts)
        .skip(field.index())
        .all(|(value, start)| value == start)
}

fn format_main_label(civil: &CivilTime, label: MainLabel, brief: bool) -> String {
    match label {
        MainLabel::Fraction { digits } => {
            // Snuffler's `.%r`/`.%u`/`.%n`: the fractional part alone.
            let scale = 10_f64.powi(digits.cast_signed()) / 1000.0;
            let value = (civil.millisecond * scale).round() as u64;
            format!(".{value:0width$}", width = digits as usize)
        }
        MainLabel::HourMinuteSecond => {
            format!("{:02}:{:02}:{:02}", civil.hour, civil.minute, civil.second)
        }
        MainLabel::HourMinute => format!("{:02}:{:02}", civil.hour, civil.minute),
        MainLabel::WeekdayDay => {
            if brief {
                format!("{:02}", civil.day)
            } else {
                let weekday = WEEKDAYS
                    .get(civil.weekday as usize)
                    .copied()
                    .unwrap_or("???");
                format!("{weekday} {:02}", civil.day)
            }
        }
        MainLabel::Month => month_name(civil.month).to_owned(),
        MainLabel::Year => format!("{}", civil.year),
    }
}

fn format_context_label(civil: &CivilTime, label: ContextLabel) -> String {
    match label {
        ContextLabel::HourMinuteSecond => {
            format!("{:02}:{:02}:{:02}", civil.hour, civil.minute, civil.second)
        }
        ContextLabel::MonthDayYear => {
            format!(
                "{} {:02}, {}",
                month_name(civil.month),
                civil.day,
                civil.year
            )
        }
        ContextLabel::MonthYear => format!("{}, {}", month_name(civil.month), civil.year),
        ContextLabel::Year => format!("{}", civil.year),
        ContextLabel::None => String::new(),
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

/// Labels for one tick at the given increment, in seconds.
#[must_use]
pub fn tick_to_labels(tick: f64, increment: f64) -> TickLabels {
    let format = axis_label_format(increment);
    let civil = CivilTime::from_epoch(tick);
    let l0_brief = if matches!(format.main, MainLabel::WeekdayDay) {
        format_main_label(&civil, format.main, true)
    } else {
        String::new()
    };
    TickLabels {
        l0: format_main_label(&civil, format.main, false),
        l0_brief,
        l0_center: format.main_centered,
        l1: rolls_over(&civil, format.context_rollover)
            .then(|| format_context_label(&civil, format.context)),
        l2: rolls_over(&civil, format.date_rollover)
            .then(|| format_context_label(&civil, ContextLabel::MonthDayYear)),
    }
}

/// The context labels a tick *would* carry, rollover or not: snuffler's
/// `l1_l2_tick`, used for the left-edge fallback row when no on-screen tick
/// triggered one.
#[must_use]
pub fn l1_l2_tick(tick: f64, increment: f64) -> (String, String) {
    let format = axis_label_format(increment);
    let civil = CivilTime::from_epoch(tick);
    let l2 = if format.date_rollover == RolloverField::Never {
        String::new()
    } else {
        format_context_label(&civil, ContextLabel::MonthDayYear)
    };
    (format_context_label(&civil, format.context), l2)
}

/// Full UTC timestamp with millisecond precision, for marker lists and readouts.
#[must_use]
pub fn format_timestamp(time: f64) -> String {
    let civil = CivilTime::from_epoch(time);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:06.3}",
        civil.year,
        civil.month,
        civil.day,
        civil.hour,
        civil.minute,
        f64::from(civil.second) + civil.millisecond / 1000.0
    )
}
