// the ISO 8601 calendar arithmetic, time durations and rounding the Temporal types share, as
// the Temporal specification's abstract operations give them: dates and times are plain
// fields, durations a date part and a time part in nanoseconds, and every quantity that can
// outgrow 64 bits is an i128

use std::cmp::Ordering;

pub(crate) const NS_PER_DAY: i128 = 86_400_000_000_000;
pub(crate) const NS_PER_SECOND: i128 = 1_000_000_000;
// an instant may lie 10^8 days either side of the epoch
pub(crate) const NS_MAX_INSTANT: i128 = 8_640_000_000_000_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Date {
    pub(crate) year: i32,
    pub(crate) month: u8,
    pub(crate) day: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Time {
    pub(crate) hour: u8,
    pub(crate) minute: u8,
    pub(crate) second: u8,
    pub(crate) millisecond: u16,
    pub(crate) microsecond: u16,
    pub(crate) nanosecond: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DateTime {
    pub(crate) date: Date,
    pub(crate) time: Time,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

pub(crate) const UNITS: [(Unit, &str, &str); 10] = [
    (Unit::Year, "year", "years"),
    (Unit::Month, "month", "months"),
    (Unit::Week, "week", "weeks"),
    (Unit::Day, "day", "days"),
    (Unit::Hour, "hour", "hours"),
    (Unit::Minute, "minute", "minutes"),
    (Unit::Second, "second", "seconds"),
    (Unit::Millisecond, "millisecond", "milliseconds"),
    (Unit::Microsecond, "microsecond", "microseconds"),
    (Unit::Nanosecond, "nanosecond", "nanoseconds"),
];

impl Unit {
    pub(crate) fn named(name: &str) -> Option<Unit> {
        UNITS
            .iter()
            .find(|(_, singular, plural)| *singular == name || *plural == name)
            .map(|(unit, ..)| *unit)
    }

    pub(crate) fn is_calendar(self) -> bool {
        self < Unit::Day
    }

    pub(crate) fn is_date(self) -> bool {
        self <= Unit::Day
    }

    // the unit's length in nanoseconds, for day and the time units
    pub(crate) fn nanoseconds(self) -> i128 {
        match self {
            Unit::Day => NS_PER_DAY,
            Unit::Hour => 3_600_000_000_000,
            Unit::Minute => 60_000_000_000,
            Unit::Second => NS_PER_SECOND,
            Unit::Millisecond => 1_000_000,
            Unit::Microsecond => 1_000,
            Unit::Nanosecond => 1,
            _ => unreachable!("calendar units have no fixed length"),
        }
    }

    // how many of this unit make the next larger one, which an increment must divide
    pub(crate) fn maximum_increment(self) -> Option<i128> {
        match self {
            Unit::Hour => Some(24),
            Unit::Minute | Unit::Second => Some(60),
            Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond => Some(1000),
            _ => None,
        }
    }
}

pub(crate) fn larger(a: Unit, b: Unit) -> Unit {
    a.min(b)
}

pub(crate) fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub(crate) fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

impl Date {
    pub(crate) fn epoch_days(self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        )
    }

    // a day count from the epoch as a date, when its year fits the type at all
    pub(crate) fn from_epoch_days(days: i64) -> Option<Date> {
        let (year, month, day) = civil_from_days(days);
        Some(Date {
            year: i32::try_from(year).ok()?,
            month: month as u8,
            day: day as u8,
        })
    }

    pub(crate) fn day_of_week(self) -> i64 {
        // 1970-01-01 was a Thursday, day 4 counting Monday as 1
        (self.epoch_days() + 3).rem_euclid(7) + 1
    }

    pub(crate) fn day_of_year(self) -> i64 {
        self.epoch_days() - days_from_civil(i64::from(self.year), 1, 1) + 1
    }

    // the ISO week and the year it belongs to
    pub(crate) fn week_of_year(self) -> (i64, i64) {
        let year = i64::from(self.year);
        let week = (self.day_of_year() - self.day_of_week() + 10).div_euclid(7);
        let weeks_in = |year: i64| {
            let jan1 = Date {
                year: year as i32,
                month: 1,
                day: 1,
            }
            .day_of_week();
            if jan1 == 4 || (jan1 == 3 && is_leap(year)) {
                53
            } else {
                52
            }
        };
        if week < 1 {
            (weeks_in(year - 1), year - 1)
        } else if week > weeks_in(year) {
            (1, year + 1)
        } else {
            (week, year)
        }
    }
}

pub(crate) fn balance_year_month(year: i64, month: i64) -> (i64, i64) {
    let month = month - 1;
    (year + month.div_euclid(12), month.rem_euclid(12) + 1)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Overflow {
    Constrain,
    Reject,
}

// a date from fields, kept in range or refused as the overflow option says
pub(crate) fn regulate_date(year: i64, month: i64, day: i64, overflow: Overflow) -> Option<Date> {
    let fits =
        (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month.clamp(1, 12));
    if overflow == Overflow::Reject && !fits {
        return None;
    }
    let month = month.clamp(1, 12);
    let day = day.clamp(1, days_in_month(year, month));
    Some(Date {
        year: i32::try_from(year).ok()?,
        month: month as u8,
        day: day as u8,
    })
}

pub(crate) fn regulate_time(fields: [i64; 6], overflow: Overflow) -> Option<Time> {
    let limits = [23, 59, 59, 999, 999, 999];
    if overflow == Overflow::Reject
        && fields
            .iter()
            .zip(limits)
            .any(|(&value, limit)| !(0..=limit).contains(&value))
    {
        return None;
    }
    let [hour, minute, second, millisecond, microsecond, nanosecond] =
        std::array::from_fn(|index| fields[index].clamp(0, limits[index]));
    Some(Time {
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        millisecond: millisecond as u16,
        microsecond: microsecond as u16,
        nanosecond: nanosecond as u16,
    })
}

impl Time {
    pub(crate) fn nanoseconds(self) -> i128 {
        ((i128::from(self.hour) * 60 + i128::from(self.minute)) * 60 + i128::from(self.second))
            * NS_PER_SECOND
            + i128::from(self.millisecond) * 1_000_000
            + i128::from(self.microsecond) * 1000
            + i128::from(self.nanosecond)
    }

    // a time from nanoseconds that may run past a day either way, with the days it crossed
    pub(crate) fn balance(nanoseconds: i128) -> (i64, Time) {
        let days = nanoseconds.div_euclid(NS_PER_DAY);
        let mut rest = nanoseconds.rem_euclid(NS_PER_DAY);
        let mut take = |size: i128| {
            let part = rest / size;
            rest %= size;
            part
        };
        let hour = take(3_600_000_000_000) as u8;
        let minute = take(60_000_000_000) as u8;
        let second = take(NS_PER_SECOND) as u8;
        let millisecond = take(1_000_000) as u16;
        let microsecond = take(1000) as u16;
        let nanosecond = rest as u16;
        (
            days as i64,
            Time {
                hour,
                minute,
                second,
                millisecond,
                microsecond,
                nanosecond,
            },
        )
    }

    pub(crate) fn fields(self) -> [i64; 6] {
        [
            i64::from(self.hour),
            i64::from(self.minute),
            i64::from(self.second),
            i64::from(self.millisecond),
            i64::from(self.microsecond),
            i64::from(self.nanosecond),
        ]
    }
}

impl DateTime {
    pub(crate) fn utc_epoch_ns(self) -> i128 {
        i128::from(self.date.epoch_days()) * NS_PER_DAY + self.time.nanoseconds()
    }

    pub(crate) fn from_epoch_ns(nanoseconds: i128) -> Option<DateTime> {
        let (days, time) = Time::balance(nanoseconds);
        Some(DateTime {
            date: Date::from_epoch_days(days)?,
            time,
        })
    }

    // a date and time shifted by nanoseconds, the days carried into the date
    pub(crate) fn add_nanoseconds(self, nanoseconds: i128) -> Option<DateTime> {
        let (days, time) = Time::balance(self.time.nanoseconds() + nanoseconds);
        Some(DateTime {
            date: Date::from_epoch_days(self.date.epoch_days().checked_add(days)?)?,
            time,
        })
    }
}

pub(crate) fn instant_in_range(nanoseconds: i128) -> bool {
    (-NS_MAX_INSTANT..=NS_MAX_INSTANT).contains(&nanoseconds)
}

// a date's noon must lie within a day of the instants Temporal allows
pub(crate) fn date_in_range(date: Date) -> bool {
    let noon = i128::from(date.epoch_days()) * NS_PER_DAY + NS_PER_DAY / 2;
    noon.abs() < NS_MAX_INSTANT + NS_PER_DAY
}

pub(crate) fn date_time_in_range(date_time: DateTime) -> bool {
    date_time.utc_epoch_ns().abs() < NS_MAX_INSTANT + NS_PER_DAY
}

pub(crate) fn year_month_in_range(year: i64, month: i64) -> bool {
    (-271_821 * 12 + 4..=275_760 * 12 + 9).contains(&(year * 12 + month))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

pub(crate) const ROUNDING_MODES: [(RoundingMode, &str); 9] = [
    (RoundingMode::Ceil, "ceil"),
    (RoundingMode::Floor, "floor"),
    (RoundingMode::Expand, "expand"),
    (RoundingMode::Trunc, "trunc"),
    (RoundingMode::HalfCeil, "halfCeil"),
    (RoundingMode::HalfFloor, "halfFloor"),
    (RoundingMode::HalfExpand, "halfExpand"),
    (RoundingMode::HalfTrunc, "halfTrunc"),
    (RoundingMode::HalfEven, "halfEven"),
];

impl RoundingMode {
    // since runs until backwards, so its rounding turns the other way
    pub(crate) fn negated(self) -> RoundingMode {
        match self {
            RoundingMode::Ceil => RoundingMode::Floor,
            RoundingMode::Floor => RoundingMode::Ceil,
            RoundingMode::HalfCeil => RoundingMode::HalfFloor,
            RoundingMode::HalfFloor => RoundingMode::HalfCeil,
            other => other,
        }
    }

    // which way a magnitude rounds: toward zero, away, or at halves one of those or even
    fn unsigned(self, negative: bool) -> (bool, Option<bool>) {
        // (away from zero when not at a half, Some(away) at a half, None for half-even)
        match (self, negative) {
            (RoundingMode::Ceil, false)
            | (RoundingMode::Floor, true)
            | (RoundingMode::Expand, _) => (true, Some(true)),
            (RoundingMode::Ceil, true)
            | (RoundingMode::Floor, false)
            | (RoundingMode::Trunc, _) => (false, Some(false)),
            (RoundingMode::HalfCeil, false)
            | (RoundingMode::HalfFloor, true)
            | (RoundingMode::HalfExpand, _) => (false, Some(true)),
            (RoundingMode::HalfCeil, true)
            | (RoundingMode::HalfFloor, false)
            | (RoundingMode::HalfTrunc, _) => (false, Some(false)),
            (RoundingMode::HalfEven, _) => (false, None),
        }
    }

    fn is_half(self) -> bool {
        matches!(
            self,
            RoundingMode::HalfCeil
                | RoundingMode::HalfFloor
                | RoundingMode::HalfExpand
                | RoundingMode::HalfTrunc
                | RoundingMode::HalfEven
        )
    }

    // whether a magnitude between two multiples goes to the upper one; the distance to the
    // lower is part over whole, and lower says which multiple it is
    fn rounds_up(self, negative: bool, part: i128, whole: i128, lower: i128) -> bool {
        if part == 0 {
            return false;
        }
        let (away, at_half) = self.unsigned(negative);
        if !self.is_half() {
            return away;
        }
        match (part * 2).cmp(&whole) {
            Ordering::Less => false,
            Ordering::Greater => true,
            Ordering::Equal => at_half.unwrap_or(lower % 2 != 0),
        }
    }
}

// RoundNumberToIncrement on an integer quantity
pub(crate) fn round_to_increment(value: i128, increment: i128, mode: RoundingMode) -> i128 {
    let negative = value < 0;
    let magnitude = value.abs();
    let lower = magnitude / increment;
    let part = magnitude - lower * increment;
    let quotient = if mode.rounds_up(negative, part, increment, lower) {
        lower + 1
    } else {
        lower
    };
    let rounded = quotient * increment;
    if negative { -rounded } else { rounded }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DateDuration {
    pub(crate) years: i64,
    pub(crate) months: i64,
    pub(crate) weeks: i64,
    pub(crate) days: i64,
}

impl DateDuration {
    pub(crate) fn sign(&self) -> i64 {
        [self.years, self.months, self.weeks, self.days]
            .into_iter()
            .find(|&value| value != 0)
            .map_or(0, i64::signum)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Internal {
    pub(crate) date: DateDuration,
    pub(crate) time: i128,
}

impl Internal {
    pub(crate) fn sign(&self) -> i64 {
        match self.date.sign() {
            0 => self.time.signum() as i64,
            sign => sign,
        }
    }
}

// the ten fields of a Temporal.Duration
pub(crate) type Fields = [f64; 10];

pub(crate) fn fields_sign(fields: &Fields) -> i64 {
    fields
        .iter()
        .find(|&&value| value != 0.0)
        .map_or(0, |value| if *value < 0.0 { -1 } else { 1 })
}

// IsValidDuration: whole numbers of one sign, calendar fields under 2^32 and the rest under
// 2^53 seconds in all
pub(crate) fn valid_fields(fields: &Fields) -> bool {
    let sign = fields_sign(fields);
    if fields.iter().any(|value| {
        !value.is_finite()
            || value.fract() != 0.0
            || (value.signum() as i64 != sign && *value != 0.0)
    }) {
        return false;
    }
    if fields[..3]
        .iter()
        .any(|value| value.abs() >= 4_294_967_296.0)
    {
        return false;
    }
    time_of_fields(fields, true).is_some_and(|total| total.abs() < (1_i128 << 53) * NS_PER_SECOND)
}

// the days and time fields as nanoseconds, exactly; none past what i128 holds
fn time_of_fields(fields: &Fields, with_days: bool) -> Option<i128> {
    let scales = [
        NS_PER_DAY,
        3_600_000_000_000,
        60_000_000_000,
        NS_PER_SECOND,
        1_000_000,
        1000,
        1,
    ];
    let start = if with_days { 3 } else { 4 };
    fields[start..]
        .iter()
        .zip(&scales[start - 3..])
        .try_fold(0_i128, |total, (value, scale)| {
            if value.abs() > 1e30 {
                return None;
            }
            total.checked_add((*value as i128).checked_mul(*scale)?)
        })
}

pub(crate) fn internal_of(fields: &Fields) -> Internal {
    Internal {
        date: DateDuration {
            years: fields[0] as i64,
            months: fields[1] as i64,
            weeks: fields[2] as i64,
            days: fields[3] as i64,
        },
        time: time_of_fields(fields, false).expect("a valid duration"),
    }
}

pub(crate) fn internal_with_24_hour_days(fields: &Fields) -> Internal {
    Internal {
        date: DateDuration {
            days: 0,
            ..internal_of(fields).date
        },
        time: time_of_fields(fields, true).expect("a valid duration"),
    }
}

// a date duration with any time folded into whole days, as adding to a date takes it
pub(crate) fn date_duration_without_time(fields: &Fields) -> DateDuration {
    let internal = internal_with_24_hour_days(fields);
    DateDuration {
        days: (internal.time / NS_PER_DAY) as i64,
        ..internal.date
    }
}

// TemporalDurationFromInternal: the time part spread over days and time units down from the
// largest unit asked for
pub(crate) fn fields_of(internal: Internal, largest: Unit) -> Option<Fields> {
    let sign = internal.time.signum();
    let mut rest = internal.time.abs();
    let sizes = [
        NS_PER_DAY,
        3_600_000_000_000,
        60_000_000_000,
        NS_PER_SECOND,
        1_000_000,
        1000,
        1,
    ];
    // days take a share only when the largest unit is a date unit; otherwise the largest time
    // unit takes all that is above it
    let first = (largest as usize).saturating_sub(Unit::Day as usize);
    let mut parts = [0_i128; 7];
    for index in first..7 {
        parts[index] = rest / sizes[index];
        rest %= sizes[index];
    }
    let mut fields: Fields = [
        internal.date.years as f64,
        internal.date.months as f64,
        internal.date.weeks as f64,
        (i128::from(internal.date.days) + parts[0] * sign) as f64,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ];
    for index in 1..7 {
        fields[index + 3] = (parts[index] * sign) as f64;
    }
    let fields = fields.map(|value| if value == 0.0 { 0.0 } else { value });
    valid_fields(&fields).then_some(fields)
}

// how a zone answers, so the algorithms here can work in a zone or without one
pub(crate) trait Zone {
    fn offset_ns(&self, epoch_ns: i128) -> i128;
    fn possible(&self, local: DateTime) -> Vec<i128>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Disambiguation {
    Compatible,
    Earlier,
    Later,
    Reject,
}

pub(crate) fn date_time_for(zone: &dyn Zone, epoch_ns: i128) -> DateTime {
    DateTime::from_epoch_ns(epoch_ns + zone.offset_ns(epoch_ns)).expect("an instant in range")
}

// GetEpochNanosecondsFor: the instant a wall clock time names, choosing in a fold and
// stepping over a gap as the disambiguation says
pub(crate) fn epoch_ns_for(
    zone: &dyn Zone,
    local: DateTime,
    disambiguation: Disambiguation,
) -> Option<i128> {
    let possible = zone.possible(local);
    if possible.len() == 1 {
        return Some(possible[0]);
    }
    if !possible.is_empty() {
        return match disambiguation {
            Disambiguation::Compatible | Disambiguation::Earlier => Some(possible[0]),
            Disambiguation::Later => possible.last().copied(),
            Disambiguation::Reject => None,
        };
    }
    if disambiguation == Disambiguation::Reject {
        return None;
    }
    let utc = local.utc_epoch_ns();
    let before = zone.offset_ns(utc - NS_PER_DAY);
    let after = zone.offset_ns(utc + NS_PER_DAY);
    let gap = after - before;
    if disambiguation == Disambiguation::Earlier {
        let earlier = local.add_nanoseconds(-gap)?;
        zone.possible(earlier).first().copied()
    } else {
        let later = local.add_nanoseconds(gap)?;
        zone.possible(later).last().copied()
    }
}

// CalendarDateAdd in the ISO calendar
pub(crate) fn add_date(date: Date, duration: DateDuration, overflow: Overflow) -> Option<Date> {
    let (year, month) = balance_year_month(
        i64::from(date.year) + duration.years,
        i64::from(date.month) + duration.months,
    );
    let regulated = regulate_date(year, month, i64::from(date.day), overflow)?;
    let days = duration.days.checked_add(duration.weeks.checked_mul(7)?)?;
    let result = Date::from_epoch_days(regulated.epoch_days().checked_add(days)?)?;
    date_in_range(result).then_some(result)
}

// whether a date given by possibly overflowing fields lies past another, going one way
fn surpasses(sign: i64, year: i64, month: i64, day: i64, other: Date) -> bool {
    let compare = (year, month, day).cmp(&(
        i64::from(other.year),
        i64::from(other.month),
        i64::from(other.day),
    ));
    match compare {
        Ordering::Equal => false,
        Ordering::Greater => sign > 0,
        Ordering::Less => sign < 0,
    }
}

// CalendarDateUntil in the ISO calendar
pub(crate) fn date_until(one: Date, two: Date, largest: Unit) -> DateDuration {
    let sign = match two.cmp(&one) {
        Ordering::Equal => return DateDuration::default(),
        Ordering::Greater => 1,
        Ordering::Less => -1,
    };
    let (mut years, mut months) = (0, 0);
    let (y1, m1, d1) = (
        i64::from(one.year),
        i64::from(one.month),
        i64::from(one.day),
    );
    if largest <= Unit::Month {
        let mut candidate = i64::from(two.year) - y1;
        if candidate != 0 {
            candidate -= sign;
        }
        while !surpasses(sign, y1 + candidate, m1, d1, two) {
            years = candidate;
            candidate += sign;
        }
        let mut candidate_months = sign;
        let mut intermediate = balance_year_month(y1 + years, m1 + candidate_months);
        while !surpasses(sign, intermediate.0, intermediate.1, d1, two) {
            months = candidate_months;
            candidate_months += sign;
            intermediate = balance_year_month(intermediate.0, intermediate.1 + sign);
        }
        if largest == Unit::Month {
            months += years * 12;
            years = 0;
        }
    }
    let (year, month) = balance_year_month(y1 + years, m1 + months);
    let constrained =
        regulate_date(year, month, d1, Overflow::Constrain).expect("a month in range");
    let remaining = two.epoch_days() - constrained.epoch_days();
    let (weeks, days) = if largest == Unit::Week {
        (remaining / 7, remaining % 7)
    } else {
        (0, remaining)
    };
    DateDuration {
        years,
        months,
        weeks,
        days,
    }
}

// DifferenceISODateTime
pub(crate) fn date_time_difference(one: DateTime, two: DateTime, largest: Unit) -> Internal {
    let mut time = two.time.nanoseconds() - one.time.nanoseconds();
    let time_sign = time.signum() as i64;
    // the dates compared first to second, so a time running the other way borrows a day
    let date_sign = match one.date.cmp(&two.date) {
        Ordering::Greater => 1,
        Ordering::Less => -1,
        Ordering::Equal => 0,
    };
    let mut adjusted = two.date;
    if time_sign == date_sign && time_sign != 0 {
        adjusted =
            Date::from_epoch_days(two.date.epoch_days() + time_sign).expect("a date in range");
        time -= i128::from(time_sign) * NS_PER_DAY;
    }
    let date_largest = larger(Unit::Day, largest);
    let mut date = date_until(one.date, adjusted, date_largest);
    if largest != date_largest {
        time += i128::from(date.days) * NS_PER_DAY;
        date.days = 0;
    }
    Internal { date, time }
}

// DifferenceZonedDateTime
pub(crate) fn zoned_difference(
    zone: &dyn Zone,
    ns1: i128,
    ns2: i128,
    largest: Unit,
) -> Option<Internal> {
    if ns1 == ns2 {
        return Some(Internal::default());
    }
    let start = date_time_for(zone, ns1);
    let end = date_time_for(zone, ns2);
    if start.date == end.date {
        return Some(Internal {
            date: DateDuration::default(),
            time: ns2 - ns1,
        });
    }
    let sign: i64 = if ns2 > ns1 { 1 } else { -1 };
    let maximum_correction = if sign == 1 { 2 } else { 1 };
    let mut correction = 0;
    let mut time = end.time.nanoseconds() - start.time.nanoseconds();
    if time.signum() as i64 == -sign {
        correction += 1;
    }
    while correction <= maximum_correction {
        let intermediate_date = Date::from_epoch_days(end.date.epoch_days() - correction * sign)?;
        let intermediate = DateTime {
            date: intermediate_date,
            time: start.time,
        };
        let intermediate_ns = epoch_ns_for(zone, intermediate, Disambiguation::Compatible)?;
        time = ns2 - intermediate_ns;
        if time.signum() as i64 != -sign {
            let date = date_until(start.date, intermediate_date, larger(largest, Unit::Day));
            return Some(Internal { date, time });
        }
        correction += 1;
    }
    None
}

// the epoch nanoseconds of a date and time, in a zone or as UTC without one
fn epoch_of(zone: Option<&dyn Zone>, date_time: DateTime) -> Option<i128> {
    match zone {
        Some(zone) => epoch_ns_for(zone, date_time, Disambiguation::Compatible),
        None => Some(date_time.utc_epoch_ns()),
    }
}

pub(crate) struct Nudge {
    pub(crate) duration: Internal,
    // the total of the unit rounded to, as a fraction over a denominator, for total()
    pub(crate) total: Option<(i128, i128, i128)>,
    epoch_ns: i128,
    expanded: bool,
}

// NudgeToCalendarUnit
#[allow(clippy::too_many_arguments)]
pub(crate) fn nudge_to_calendar_unit(
    sign: i64,
    duration: Internal,
    destination: i128,
    origin: DateTime,
    zone: Option<&dyn Zone>,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> Option<Nudge> {
    let date = duration.date;
    let truncated = |value: i64| {
        round_to_increment(
            i128::from(value),
            i128::from(increment),
            RoundingMode::Trunc,
        ) as i64
    };
    let step = increment * sign;
    let (r1, start, end) = match unit {
        Unit::Year => {
            let r1 = truncated(date.years);
            (
                r1,
                DateDuration {
                    years: r1,
                    ..Default::default()
                },
                DateDuration {
                    years: r1 + step,
                    ..Default::default()
                },
            )
        }
        Unit::Month => {
            let r1 = truncated(date.months);
            (
                r1,
                DateDuration {
                    years: date.years,
                    months: r1,
                    ..Default::default()
                },
                DateDuration {
                    years: date.years,
                    months: r1 + step,
                    ..Default::default()
                },
            )
        }
        Unit::Week => {
            let years_months = DateDuration {
                years: date.years,
                months: date.months,
                ..Default::default()
            };
            let weeks_start = add_date(origin.date, years_months, Overflow::Constrain)?;
            let weeks_end = Date::from_epoch_days(weeks_start.epoch_days() + date.days)?;
            let until = date_until(weeks_start, weeks_end, Unit::Week);
            let r1 = truncated(date.weeks + until.weeks);
            (
                r1,
                DateDuration {
                    years: date.years,
                    months: date.months,
                    weeks: r1,
                    days: 0,
                },
                DateDuration {
                    years: date.years,
                    months: date.months,
                    weeks: r1 + step,
                    days: 0,
                },
            )
        }
        _ => {
            let r1 = truncated(date.days);
            (
                r1,
                DateDuration { days: r1, ..date },
                DateDuration {
                    days: r1 + step,
                    ..date
                },
            )
        }
    };
    let start_date = add_date(origin.date, start, Overflow::Constrain)?;
    let end_date = add_date(origin.date, end, Overflow::Constrain)?;
    let start_ns = epoch_of(
        zone,
        DateTime {
            date: start_date,
            time: origin.time,
        },
    )?;
    let end_ns = epoch_of(
        zone,
        DateTime {
            date: end_date,
            time: origin.time,
        },
    )?;
    if end_ns == start_ns {
        return None;
    }
    let numerator = destination - start_ns;
    let denominator = end_ns - start_ns;
    // the distance past r1, in units of the step, is numerator / denominator of one step
    let (part, whole) = if denominator < 0 {
        (-numerator, -denominator)
    } else {
        (numerator, denominator)
    };
    let lower = i128::from(r1.abs() / increment);
    let rounds_up = part == whole || mode.rounds_up(sign < 0, part, whole, lower);
    let total = (
        i128::from(r1) * whole + i128::from(step) * part,
        whole,
        i128::from(sign),
    );
    let (duration, epoch_ns, expanded) = if rounds_up {
        (end, end_ns, true)
    } else {
        (start, start_ns, false)
    };
    Some(Nudge {
        duration: Internal {
            date: duration,
            time: 0,
        },
        total: Some(total),
        epoch_ns,
        expanded,
    })
}

// NudgeToZonedTime
#[allow(clippy::too_many_arguments)]
fn nudge_to_zoned_time(
    sign: i64,
    duration: Internal,
    origin: DateTime,
    zone: &dyn Zone,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> Option<Nudge> {
    let start = add_date(origin.date, duration.date, Overflow::Constrain)?;
    let end = Date::from_epoch_days(start.epoch_days() + sign)?;
    let start_ns = epoch_ns_for(
        zone,
        DateTime {
            date: start,
            time: origin.time,
        },
        Disambiguation::Compatible,
    )?;
    let end_ns = epoch_ns_for(
        zone,
        DateTime {
            date: end,
            time: origin.time,
        },
        Disambiguation::Compatible,
    )?;
    let day_span = end_ns - start_ns;
    let size = i128::from(increment) * unit.nanoseconds();
    let mut rounded = round_to_increment(duration.time, size, mode);
    let beyond = rounded - day_span;
    let (day_delta, epoch_ns, expanded) = if beyond.signum() as i64 != -sign {
        rounded = round_to_increment(beyond, size, mode);
        (sign, end_ns + rounded, true)
    } else {
        (0, start_ns + rounded, false)
    };
    Some(Nudge {
        duration: Internal {
            date: DateDuration {
                days: duration.date.days + day_delta,
                ..duration.date
            },
            time: rounded,
        },
        total: None,
        epoch_ns,
        expanded,
    })
}

// NudgeToDayOrTime
fn nudge_to_day_or_time(
    duration: Internal,
    destination: i128,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Nudge {
    let time = duration.time + i128::from(duration.date.days) * NS_PER_DAY;
    let rounded = round_to_increment(time, i128::from(increment) * smallest.nanoseconds(), mode);
    let difference = rounded - time;
    let whole_days = time / NS_PER_DAY;
    let rounded_days = rounded / NS_PER_DAY;
    let day_delta = rounded_days - whole_days;
    let expanded = day_delta.signum() == time.signum();
    let (days, remainder) = if larger(largest, Unit::Day) == largest {
        (rounded_days, rounded - rounded_days * NS_PER_DAY)
    } else {
        (0, rounded)
    };
    Nudge {
        duration: Internal {
            date: DateDuration {
                days: days as i64,
                ..duration.date
            },
            time: remainder,
        },
        total: None,
        epoch_ns: destination + difference,
        expanded,
    }
}

// BubbleRelativeDuration
fn bubble(
    sign: i64,
    mut duration: Internal,
    nudged: i128,
    origin: DateTime,
    zone: Option<&dyn Zone>,
    largest: Unit,
    smallest: Unit,
) -> Option<Internal> {
    if smallest == largest {
        return Some(duration);
    }
    let units = [Unit::Year, Unit::Month, Unit::Week, Unit::Day];
    for &unit in units.iter().rev() {
        if unit >= smallest || unit < largest {
            continue;
        }
        if unit == Unit::Week && largest != Unit::Week {
            continue;
        }
        let date = duration.date;
        let end = match unit {
            Unit::Year => DateDuration {
                years: date.years + sign,
                ..Default::default()
            },
            Unit::Month => DateDuration {
                years: date.years,
                months: date.months + sign,
                ..Default::default()
            },
            Unit::Week => DateDuration {
                years: date.years,
                months: date.months,
                weeks: date.weeks + sign,
                days: 0,
            },
            _ => DateDuration {
                days: date.days + sign,
                ..date
            },
        };
        let end_date = add_date(origin.date, end, Overflow::Constrain)?;
        let end_ns = epoch_of(
            zone,
            DateTime {
                date: end_date,
                time: origin.time,
            },
        )?;
        let beyond = nudged - end_ns;
        if beyond.signum() as i64 != -sign {
            duration = Internal { date: end, time: 0 };
        } else {
            break;
        }
    }
    Some(duration)
}

// RoundRelativeDuration
#[allow(clippy::too_many_arguments)]
pub(crate) fn round_relative(
    duration: Internal,
    destination: i128,
    origin: DateTime,
    zone: Option<&dyn Zone>,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Option<Internal> {
    let irregular = smallest.is_calendar() || (zone.is_some() && smallest == Unit::Day);
    let sign = if duration.sign() < 0 { -1 } else { 1 };
    let nudge = if irregular {
        nudge_to_calendar_unit(
            sign,
            duration,
            destination,
            origin,
            zone,
            increment,
            smallest,
            mode,
        )?
    } else if let Some(zone) = zone {
        nudge_to_zoned_time(sign, duration, origin, zone, increment, smallest, mode)?
    } else {
        nudge_to_day_or_time(duration, destination, largest, increment, smallest, mode)
    };
    if nudge.expanded && smallest != Unit::Week {
        return bubble(
            sign,
            nudge.duration,
            nudge.epoch_ns,
            origin,
            zone,
            largest,
            larger(smallest, Unit::Day),
        );
    }
    Some(nudge.duration)
}

// TotalRelativeDuration, as a numerator and denominator
pub(crate) fn total_relative(
    duration: Internal,
    destination: i128,
    origin: DateTime,
    zone: Option<&dyn Zone>,
    unit: Unit,
) -> Option<(i128, i128)> {
    if unit.is_calendar() || (zone.is_some() && unit == Unit::Day) {
        let sign = if duration.sign() < 0 { -1 } else { 1 };
        let nudge = nudge_to_calendar_unit(
            sign,
            duration,
            destination,
            origin,
            zone,
            1,
            unit,
            RoundingMode::Trunc,
        )?;
        let (numerator, denominator, _) = nudge.total?;
        return Some((numerator, denominator));
    }
    let time = duration.time + i128::from(duration.date.days) * NS_PER_DAY;
    Some((time, unit.nanoseconds()))
}

// an exact fraction as the nearest double
pub(crate) fn fraction_to_f64(numerator: i128, denominator: i128) -> f64 {
    let whole = numerator / denominator;
    let rest = numerator % denominator;
    // splitting off the whole part keeps both halves within what a double holds exactly enough
    whole as f64 + rest as f64 / denominator as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn dates_balance_differ_and_round() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(date(2024, 12, 30).week_of_year(), (1, 2025));
        assert_eq!(date(2021, 1, 1).week_of_year(), (53, 2020));
        assert_eq!(date(2024, 12, 30).day_of_week(), 1);
        assert_eq!(
            date_until(date(2020, 2, 29), date(2024, 2, 28), Unit::Year),
            DateDuration {
                years: 3,
                months: 11,
                weeks: 0,
                days: 30
            }
        );
        assert_eq!(
            date_until(date(2024, 1, 31), date(2024, 3, 1), Unit::Month),
            DateDuration {
                years: 0,
                months: 1,
                weeks: 0,
                days: 1
            }
        );
        assert_eq!(
            add_date(
                date(2024, 1, 31),
                DateDuration {
                    months: 1,
                    ..Default::default()
                },
                Overflow::Constrain
            ),
            Some(date(2024, 2, 29))
        );
        assert_eq!(
            add_date(
                date(2024, 1, 31),
                DateDuration {
                    months: 1,
                    ..Default::default()
                },
                Overflow::Reject
            ),
            None
        );
        assert_eq!(round_to_increment(25, 10, RoundingMode::HalfEven), 20);
        assert_eq!(round_to_increment(35, 10, RoundingMode::HalfEven), 40);
        assert_eq!(round_to_increment(-25, 10, RoundingMode::HalfExpand), -30);
        assert_eq!(round_to_increment(-25, 10, RoundingMode::Floor), -30);
        assert_eq!(round_to_increment(-25, 10, RoundingMode::Ceil), -20);
        assert!(date_in_range(date(275_760, 9, 13)));
        assert!(!date_in_range(date(275_760, 9, 14)));
        assert!(date_in_range(date(-271_821, 4, 19)));
        assert!(!date_in_range(date(-271_821, 4, 18)));
    }
}
