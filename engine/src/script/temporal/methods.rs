// what the Temporal types do: turning values and property bags into Temporal values, the
// getters, and each type's methods, after the abstract operations of the specification

use super::iso::{
    self, Date, DateDuration, DateTime, Disambiguation, Fields, Internal, Overflow, RoundingMode,
    Time, Unit,
};
use super::*;

// relativeTo for duration rounding and totals
enum Relative {
    Plain(Date),
    Zoned(i128, TimeZone),
}

// how an offset written with a date and time is used to find the instant
#[derive(Clone, Copy, PartialEq, Eq)]
enum OffsetUse {
    Use,
    Ignore,
    Prefer,
    Reject,
}

// the fields a property bag gave
#[derive(Default)]
struct Bag {
    day: Option<i64>,
    era: Option<String>,
    era_year: Option<i64>,
    time: [Option<i64>; 6],
    month: Option<i64>,
    month_code: Option<i64>,
    offset: Option<i128>,
    year: Option<i64>,
    zone: Option<TimeZone>,
    any: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Date,
    Time,
    DateTime,
    Zoned,
    YearMonth,
    MonthDay,
}

impl Want {
    fn date(self) -> bool {
        !matches!(self, Want::Time)
    }

    fn time(self) -> bool {
        matches!(self, Want::Time | Want::DateTime | Want::Zoned)
    }
}

// the settings until and since read
struct Settings {
    largest: Unit,
    smallest: Unit,
    increment: i64,
    mode: RoundingMode,
}

fn duration_largest(fields: &Fields) -> Unit {
    fields
        .iter()
        .position(|value| *value != 0.0)
        .map_or(Unit::Nanosecond, |index| iso::UNITS[index].0)
}

fn negate_fields(fields: &Fields) -> Fields {
    fields.map(|value| if value == 0.0 { 0.0 } else { -value })
}

fn month_code_text(month: u8) -> String {
    format!("M{month:02}")
}

fn start_of_day(zone: &TimeZone, date: Date) -> Option<i128> {
    let midnight = DateTime {
        date,
        time: Time::default(),
    };
    if let Some(&first) = iso::Zone::possible(zone, midnight).first() {
        return Some(first);
    }
    let TimeZone::Named(_, found) = zone else {
        return None;
    };
    let before = (midnight.utc_epoch_ns() - iso::NS_PER_DAY).div_euclid(iso::NS_PER_SECOND);
    found
        .transition(before as i64, true)
        .map(|seconds| i128::from(seconds) * iso::NS_PER_SECOND)
}

// a time rounded to an increment of a unit, wrapping past midnight, with the days carried
fn round_time(time: Time, increment: i64, unit: Unit, mode: RoundingMode) -> (i64, Time) {
    let size = i128::from(increment) * unit.nanoseconds();
    Time::balance(iso::round_to_increment(time.nanoseconds(), size, mode))
}

// RoundTimeDuration
fn round_time_duration(time: i128, increment: i64, unit: Unit, mode: RoundingMode) -> i128 {
    iso::round_to_increment(time, i128::from(increment) * unit.nanoseconds(), mode)
}

fn round_date_time(
    value: DateTime,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> Option<DateTime> {
    let (days, time) = round_time(value.time, increment, unit, mode);
    Some(DateTime {
        date: Date::from_epoch_days(value.date.epoch_days() + days)?,
        time,
    })
}

impl ScriptRuntime {
    // ToTemporalInstant
    fn temporal_instant(&mut self, value: Value) -> Result<i128, ScriptError> {
        if let Some(found) = Self::temporal_of(&value) {
            match &*found {
                Temporal::Instant(nanoseconds) | Temporal::Zoned(nanoseconds, ..) => {
                    return Ok(*nanoseconds);
                }
                _ => {}
            }
        }
        let value = if is_primitive(&value) {
            value
        } else {
            self.coerce_primitive(value, Hint::String)?
        };
        let Value::String(text) = value else {
            return Err(self.type_error("An instant must be a string"));
        };
        let parsed = parse::date_time(&text).filter(|parsed| parsed.time.is_some());
        let Some(parsed) = parsed.filter(|parsed| parsed.utc || parsed.offset.is_some()) else {
            return Err(self.range_error(format!("{text} is not a valid instant")));
        };
        let local = self.parsed_date_time(&parsed)?;
        let offset = parsed.offset.map_or(0, |(offset, _)| offset);
        self.checked_instant(Some(local.utc_epoch_ns() - offset))
    }

    fn parsed_date(&mut self, parsed: &parse::Parsed) -> Result<Date, ScriptError> {
        let date = match (parsed.year, parsed.month, parsed.day) {
            (Some(year), Some(month), Some(day)) => {
                iso::regulate_date(year, month, day, Overflow::Reject)
            }
            _ => None,
        };
        self.checked_date(date)
    }

    fn parsed_date_time(&mut self, parsed: &parse::Parsed) -> Result<DateTime, ScriptError> {
        let date = self.parsed_date(parsed)?;
        let time = parsed
            .time
            .and_then(|fields| iso::regulate_time(fields, Overflow::Reject))
            .unwrap_or_default();
        self.checked_date_time(Some(DateTime { date, time }))
    }

    // the calendar a property bag names, ISO when it names none
    fn bag_calendar(&mut self, object: &Value) -> Result<Calendar, ScriptError> {
        match self.get_property(object, "calendar")? {
            Value::Undefined => Ok(Calendar::Iso),
            other => self.temporal_calendar(other),
        }
    }

    // PrepareCalendarFields: the fields a type reads, in alphabetical order
    fn read_bag(
        &mut self,
        object: &Value,
        calendar: Calendar,
        want: Want,
    ) -> Result<Bag, ScriptError> {
        let mut bag = Bag::default();
        let mut names = Vec::new();
        if want.date() {
            names.extend(["day", "month", "monthCode", "year"]);
            if calendar == Calendar::Gregory {
                names.extend(["era", "eraYear"]);
            }
        }
        if want.time() {
            names.extend([
                "hour",
                "microsecond",
                "millisecond",
                "minute",
                "nanosecond",
                "second",
            ]);
        }
        if want == Want::Zoned {
            names.extend(["offset", "timeZone"]);
        }
        names.sort_unstable();
        for name in names {
            let value = self.get_property(object, name)?;
            if matches!(value, Value::Undefined) {
                continue;
            }
            bag.any = true;
            match name {
                "day" | "month" => {
                    let number = self.integer_field(value)?;
                    if number < 1 {
                        return Err(self.range_error(format!("{name} must be a positive integer")));
                    }
                    if name == "day" {
                        bag.day = Some(number);
                    } else {
                        bag.month = Some(number);
                    }
                }
                "year" => bag.year = Some(self.integer_field(value)?),
                "eraYear" => bag.era_year = Some(self.integer_field(value)?),
                "era" => bag.era = Some(self.coerce_string(value)?),
                "monthCode" => {
                    let value = self.coerce_primitive(value, Hint::String)?;
                    let Value::String(code) = value else {
                        return Err(self.type_error("monthCode must be a string"));
                    };
                    let month = code
                        .strip_prefix('M')
                        .filter(|digits| {
                            digits.len() == 2 && digits.chars().all(|c| c.is_ascii_digit())
                        })
                        .and_then(|digits| digits.parse::<i64>().ok())
                        .filter(|month| (1..=12).contains(month));
                    match month {
                        Some(month) => bag.month_code = Some(month),
                        None => {
                            return Err(
                                self.range_error(format!("{code} is not a valid month code"))
                            );
                        }
                    }
                }
                "offset" => {
                    let value = self.coerce_primitive(value, Hint::String)?;
                    let Value::String(text) = value else {
                        return Err(self.type_error("offset must be a string"));
                    };
                    match parse::offset(&text) {
                        Some((offset, _)) => bag.offset = Some(offset),
                        None => {
                            return Err(self.range_error(format!("{text} is not a valid offset")));
                        }
                    }
                }
                "timeZone" => bag.zone = Some(self.temporal_zone(value)?),
                time_name => {
                    let index = [
                        "hour",
                        "minute",
                        "second",
                        "millisecond",
                        "microsecond",
                        "nanosecond",
                    ]
                    .iter()
                    .position(|found| *found == time_name)
                    .expect("a time field");
                    bag.time[index] = Some(self.integer_field(value)?);
                }
            }
        }
        Ok(bag)
    }

    fn bag_year(&mut self, bag: &Bag, calendar: Calendar) -> Result<Option<i64>, ScriptError> {
        if calendar == Calendar::Gregory
            && let (Some(era), Some(era_year)) = (&bag.era, bag.era_year)
        {
            let year = match era.as_str() {
                "ce" | "ad" | "gregory" => era_year,
                "bce" | "bc" | "gregory-inverse" => 1 - era_year,
                _ => return Err(self.range_error(format!("{era} is not a valid era"))),
            };
            if bag.year.is_some_and(|given| given != year) {
                return Err(self.temporal_range("year and eraYear disagree"));
            }
            return Ok(Some(year));
        }
        if bag.era.is_some() != bag.era_year.is_some() {
            return Err(self.type_error("era and eraYear go together"));
        }
        Ok(bag.year)
    }

    fn bag_month(&mut self, bag: &Bag) -> Result<Option<i64>, ScriptError> {
        match (bag.month, bag.month_code) {
            (Some(month), Some(code)) if month != code => {
                Err(self.temporal_range("month and monthCode disagree"))
            }
            (month, code) => Ok(month.or(code)),
        }
    }

    // CalendarDateFromFields
    fn bag_date(
        &mut self,
        bag: &Bag,
        calendar: Calendar,
        overflow: Overflow,
    ) -> Result<Date, ScriptError> {
        let year = self.bag_year(bag, calendar)?;
        let month = self.bag_month(bag)?;
        let (Some(year), Some(month), Some(day)) = (year, month, bag.day) else {
            return Err(self.type_error("A date needs a year, a month or monthCode and a day"));
        };
        let date = iso::regulate_date(year, month, day, overflow);
        if date.is_none() {
            return Err(self.temporal_range("The date is not valid"));
        }
        self.checked_date(date)
    }

    fn bag_time(&mut self, bag: &Bag, overflow: Overflow) -> Result<Time, ScriptError> {
        let fields = bag.time.map(|field| field.unwrap_or(0));
        iso::regulate_time(fields, overflow)
            .ok_or_else(|| self.temporal_range("The time is not valid"))
    }

    // ToTemporalDate
    fn temporal_date(
        &mut self,
        value: Value,
        options: Value,
    ) -> Result<(Date, Calendar), ScriptError> {
        if let Some(found) = Self::temporal_of(&value) {
            let found = match &*found {
                Temporal::Date(date, calendar) => Some((*date, *calendar)),
                Temporal::DateTime(value, calendar) => Some((value.date, *calendar)),
                Temporal::Zoned(nanoseconds, zone, calendar) => {
                    Some((iso::date_time_for(zone, *nanoseconds).date, *calendar))
                }
                _ => None,
            };
            if let Some(found) = found {
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                return Ok(found);
            }
        }
        match value {
            Value::String(text) => {
                let parsed = parse::date_time(&text).filter(|parsed| !parsed.utc);
                let Some(parsed) = parsed else {
                    return Err(self.range_error(format!("{text} is not a valid date")));
                };
                let calendar = self.calendar_of(parsed.calendar.as_deref())?;
                let date = self.parsed_date(&parsed)?;
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                Ok((date, calendar))
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::Date)?;
                let options = self.options_object(options)?;
                let overflow = self.overflow_option(&options)?;
                Ok((self.bag_date(&bag, calendar, overflow)?, calendar))
            }
            _ => Err(self.type_error("A date must be a string or an object")),
        }
    }

    // ToTemporalTime
    fn temporal_time(&mut self, value: Value, options: Value) -> Result<Time, ScriptError> {
        if let Some(found) = Self::temporal_of(&value) {
            let found = match &*found {
                Temporal::Time(time) => Some(*time),
                Temporal::DateTime(value, _) => Some(value.time),
                Temporal::Zoned(nanoseconds, zone, _) => {
                    Some(iso::date_time_for(zone, *nanoseconds).time)
                }
                _ => None,
            };
            if let Some(found) = found {
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                return Ok(found);
            }
        }
        match value {
            Value::String(text) => {
                let parsed =
                    parse::time(&text).filter(|parsed| !parsed.utc && parsed.time.is_some());
                let Some(parsed) = parsed else {
                    return Err(self.range_error(format!("{text} is not a valid time")));
                };
                self.calendar_of(parsed.calendar.as_deref())?;
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                Ok(
                    iso::regulate_time(parsed.time.expect("checked above"), Overflow::Reject)
                        .unwrap_or_default(),
                )
            }
            value if !is_primitive(&value) => {
                let bag = self.read_bag(&value, Calendar::Iso, Want::Time)?;
                if !bag.any {
                    return Err(self.type_error("A time needs at least one time field"));
                }
                let options = self.options_object(options)?;
                let overflow = self.overflow_option(&options)?;
                self.bag_time(&bag, overflow)
            }
            _ => Err(self.type_error("A time must be a string or an object")),
        }
    }

    // ToTemporalDateTime
    fn temporal_date_time(
        &mut self,
        value: Value,
        options: Value,
    ) -> Result<(DateTime, Calendar), ScriptError> {
        if let Some(found) = Self::temporal_of(&value) {
            let found = match &*found {
                Temporal::DateTime(value, calendar) => Some((*value, *calendar)),
                Temporal::Date(date, calendar) => Some((
                    DateTime {
                        date: *date,
                        time: Time::default(),
                    },
                    *calendar,
                )),
                Temporal::Zoned(nanoseconds, zone, calendar) => {
                    Some((iso::date_time_for(zone, *nanoseconds), *calendar))
                }
                _ => None,
            };
            if let Some(found) = found {
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                return Ok(found);
            }
        }
        match value {
            Value::String(text) => {
                let parsed = parse::date_time(&text).filter(|parsed| !parsed.utc);
                let Some(parsed) = parsed else {
                    return Err(self.range_error(format!("{text} is not a valid date and time")));
                };
                let calendar = self.calendar_of(parsed.calendar.as_deref())?;
                let value = self.parsed_date_time(&parsed)?;
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                Ok((value, calendar))
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::DateTime)?;
                let options = self.options_object(options)?;
                let overflow = self.overflow_option(&options)?;
                let date = self.bag_date(&bag, calendar, overflow)?;
                let time = self.bag_time(&bag, overflow)?;
                Ok((
                    self.checked_date_time(Some(DateTime { date, time }))?,
                    calendar,
                ))
            }
            _ => Err(self.type_error("A date and time must be a string or an object")),
        }
    }

    // InterpretISODateTimeOffset
    #[allow(clippy::too_many_arguments)]
    fn interpret_offset(
        &mut self,
        local: DateTime,
        start_of_day_only: bool,
        offset: Option<i128>,
        exact: bool,
        zone: &TimeZone,
        disambiguation: Disambiguation,
        offset_use: OffsetUse,
        match_minutes: bool,
    ) -> Result<i128, ScriptError> {
        if start_of_day_only {
            let found = start_of_day(zone, local.date);
            return self.checked_instant(found);
        }
        let disambiguated = |runtime: &mut Self| {
            let found = iso::epoch_ns_for(zone, local, disambiguation);
            if found.is_none() {
                return Err(runtime
                    .temporal_range("The wall clock time is not unambiguous in the time zone"));
            }
            runtime.checked_instant(found)
        };
        let Some(offset) = offset else {
            return disambiguated(self);
        };
        if offset_use == OffsetUse::Ignore {
            return disambiguated(self);
        }
        if exact || offset_use == OffsetUse::Use {
            return self.checked_instant(Some(local.utc_epoch_ns() - offset));
        }
        let utc = local.utc_epoch_ns();
        for candidate in iso::Zone::possible(zone, local) {
            let candidate_offset = utc - candidate;
            if candidate_offset == offset
                || (match_minutes
                    && iso::round_to_increment(
                        candidate_offset,
                        60 * iso::NS_PER_SECOND,
                        RoundingMode::HalfExpand,
                    ) == offset)
            {
                return Ok(candidate);
            }
        }
        if offset_use == OffsetUse::Reject {
            return Err(self.temporal_range("The offset does not match the time zone"));
        }
        disambiguated(self)
    }

    fn offset_use_option(
        &mut self,
        options: &Options,
        fallback: &'static str,
    ) -> Result<OffsetUse, ScriptError> {
        Ok(
            match self
                .option_choice(
                    options,
                    "offset",
                    &["prefer", "use", "ignore", "reject"],
                    fallback,
                )?
                .as_str()
            {
                "use" => OffsetUse::Use,
                "ignore" => OffsetUse::Ignore,
                "prefer" => OffsetUse::Prefer,
                _ => OffsetUse::Reject,
            },
        )
    }

    // ToTemporalZonedDateTime
    fn temporal_zoned(
        &mut self,
        value: Value,
        options: Value,
    ) -> Result<(i128, TimeZone, Calendar), ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Temporal::Zoned(nanoseconds, zone, calendar) = &*found
        {
            let options = self.options_object(options)?;
            self.disambiguation_option(&options)?;
            self.offset_use_option(&options, "reject")?;
            self.overflow_option(&options)?;
            return Ok((*nanoseconds, zone.clone(), *calendar));
        }
        match value {
            Value::String(text) => {
                let parsed = parse::date_time(&text).filter(|parsed| parsed.zone.is_some());
                let Some(parsed) = parsed else {
                    return Err(
                        self.range_error(format!("{text} is not a valid zoned date and time"))
                    );
                };
                let annotation = parsed.zone.clone().expect("checked above");
                let Some(zone) = zone_identifier(&annotation) else {
                    return Err(self.range_error(format!("{annotation} is not a valid time zone")));
                };
                let calendar = self.calendar_of(parsed.calendar.as_deref())?;
                let options = self.options_object(options)?;
                let disambiguation = self.disambiguation_option(&options)?;
                let offset_use = self.offset_use_option(&options, "reject")?;
                self.overflow_option(&options)?;
                let local = self.parsed_date_time(&parsed)?;
                let offset = parsed.offset.map(|(offset, _)| offset);
                let match_minutes = parsed.offset.is_some_and(|(_, minutes)| minutes);
                let nanoseconds = self.interpret_offset(
                    local,
                    parsed.time.is_none(),
                    if parsed.utc { Some(0) } else { offset },
                    parsed.utc,
                    &zone,
                    disambiguation,
                    offset_use,
                    match_minutes,
                )?;
                Ok((nanoseconds, zone, calendar))
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::Zoned)?;
                let Some(zone) = bag.zone.clone() else {
                    return Err(self.type_error("A zoned date and time needs a timeZone"));
                };
                let options = self.options_object(options)?;
                let disambiguation = self.disambiguation_option(&options)?;
                let offset_use = self.offset_use_option(&options, "reject")?;
                let overflow = self.overflow_option(&options)?;
                let date = self.bag_date(&bag, calendar, overflow)?;
                let time = self.bag_time(&bag, overflow)?;
                let nanoseconds = self.interpret_offset(
                    DateTime { date, time },
                    false,
                    bag.offset,
                    false,
                    &zone,
                    disambiguation,
                    offset_use,
                    false,
                )?;
                Ok((nanoseconds, zone, calendar))
            }
            _ => Err(self.type_error("A zoned date and time must be a string or an object")),
        }
    }

    // ToTemporalYearMonth; the ISO calendar keeps the first of the month
    fn temporal_year_month(
        &mut self,
        value: Value,
        options: Value,
    ) -> Result<(Date, Calendar), ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Temporal::YearMonth(date, calendar) = &*found
        {
            let options = self.options_object(options)?;
            self.overflow_option(&options)?;
            return Ok((*date, *calendar));
        }
        let (year, month, calendar) = match value {
            Value::String(text) => {
                let parsed = parse::year_month(&text).filter(|parsed| !parsed.utc);
                let Some(parsed) = parsed else {
                    return Err(self.range_error(format!("{text} is not a valid year and month")));
                };
                let calendar = self.calendar_of(parsed.calendar.as_deref())?;
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                (
                    parsed.year.unwrap_or(0),
                    parsed.month.unwrap_or(1),
                    calendar,
                )
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::YearMonth)?;
                let options = self.options_object(options)?;
                let overflow = self.overflow_option(&options)?;
                let year = self.bag_year(&bag, calendar)?;
                let month = self.bag_month(&bag)?;
                let (Some(year), Some(month)) = (year, month) else {
                    return Err(
                        self.type_error("A year and month needs a year and a month or monthCode")
                    );
                };
                if overflow == Overflow::Reject && !(1..=12).contains(&month) {
                    return Err(self.temporal_range("The month is not valid"));
                }
                (year, month.clamp(1, 12), calendar)
            }
            _ => return Err(self.type_error("A year and month must be a string or an object")),
        };
        if !iso::year_month_in_range(year, month) {
            return Err(self.temporal_range("The year and month are out of range"));
        }
        Ok((
            Date {
                year: year as i32,
                month: month as u8,
                day: 1,
            },
            calendar,
        ))
    }

    // ToTemporalMonthDay; the ISO calendar keeps 1972, a leap year
    fn temporal_month_day(
        &mut self,
        value: Value,
        options: Value,
    ) -> Result<(Date, Calendar), ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Temporal::MonthDay(date, calendar) = &*found
        {
            let options = self.options_object(options)?;
            self.overflow_option(&options)?;
            return Ok((*date, *calendar));
        }
        match value {
            Value::String(text) => {
                let parsed = parse::month_day(&text).filter(|parsed| !parsed.utc);
                let Some(parsed) = parsed else {
                    return Err(self.range_error(format!("{text} is not a valid month and day")));
                };
                let calendar = self.calendar_of(parsed.calendar.as_deref())?;
                let options = self.options_object(options)?;
                self.overflow_option(&options)?;
                let date = iso::regulate_date(
                    1972,
                    parsed.month.unwrap_or(1),
                    parsed.day.unwrap_or(1),
                    Overflow::Reject,
                );
                Ok((self.checked_date(date)?, calendar))
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::MonthDay)?;
                let options = self.options_object(options)?;
                let overflow = self.overflow_option(&options)?;
                self.bag_month_day(&bag, calendar, overflow)
                    .map(|date| (date, calendar))
            }
            _ => Err(self.type_error("A month and day must be a string or an object")),
        }
    }

    fn bag_month_day(
        &mut self,
        bag: &Bag,
        calendar: Calendar,
        overflow: Overflow,
    ) -> Result<Date, ScriptError> {
        let year = self.bag_year(bag, calendar)?;
        let month = self.bag_month(bag)?;
        if calendar != Calendar::Iso
            && bag.month.is_some()
            && bag.month_code.is_none()
            && year.is_none()
        {
            return Err(self.type_error("A month and day given by month needs a year"));
        }
        let (Some(month), Some(day)) = (month, bag.day) else {
            return Err(self.type_error("A month and day needs a month or monthCode and a day"));
        };
        let checked = iso::regulate_date(year.unwrap_or(1972), month, day, overflow);
        let Some(checked) = checked else {
            return Err(self.temporal_range("The month and day are not valid"));
        };
        let date = iso::regulate_date(
            1972,
            i64::from(checked.month),
            i64::from(checked.day),
            Overflow::Constrain,
        );
        self.checked_date(date)
    }

    fn relative_to(&mut self, options: &Options) -> Result<Option<Relative>, ScriptError> {
        let Some(object) = &options.0 else {
            return Ok(None);
        };
        let value = self.get_property(object, "relativeTo")?;
        if matches!(value, Value::Undefined) {
            return Ok(None);
        }
        if let Some(found) = Self::temporal_of(&value) {
            match &*found {
                Temporal::Zoned(nanoseconds, zone, _) => {
                    return Ok(Some(Relative::Zoned(*nanoseconds, zone.clone())));
                }
                Temporal::Date(date, _) => return Ok(Some(Relative::Plain(*date))),
                Temporal::DateTime(value, _) => return Ok(Some(Relative::Plain(value.date))),
                _ => {}
            }
        }
        match value {
            Value::String(text) => {
                let parsed = parse::date_time(&text);
                let Some(parsed) = parsed.filter(|parsed| !parsed.utc || parsed.zone.is_some())
                else {
                    return Err(self.range_error(format!("{text} is not a valid relativeTo")));
                };
                self.calendar_of(parsed.calendar.as_deref())?;
                let local = self.parsed_date_time(&parsed)?;
                match &parsed.zone {
                    None => Ok(Some(Relative::Plain(local.date))),
                    Some(annotation) => {
                        let Some(zone) = zone_identifier(annotation) else {
                            return Err(
                                self.range_error(format!("{annotation} is not a valid time zone"))
                            );
                        };
                        let match_minutes = parsed.offset.is_some_and(|(_, minutes)| minutes);
                        let nanoseconds = self.interpret_offset(
                            local,
                            parsed.time.is_none(),
                            if parsed.utc {
                                Some(0)
                            } else {
                                parsed.offset.map(|(offset, _)| offset)
                            },
                            parsed.utc,
                            &zone,
                            Disambiguation::Compatible,
                            OffsetUse::Reject,
                            match_minutes,
                        )?;
                        Ok(Some(Relative::Zoned(nanoseconds, zone)))
                    }
                }
            }
            value if !is_primitive(&value) => {
                let calendar = self.bag_calendar(&value)?;
                let bag = self.read_bag(&value, calendar, Want::Zoned)?;
                let date = self.bag_date(&bag, calendar, Overflow::Constrain)?;
                match bag.zone.clone() {
                    None => Ok(Some(Relative::Plain(date))),
                    Some(zone) => {
                        let time = self.bag_time(&bag, Overflow::Constrain)?;
                        let nanoseconds = self.interpret_offset(
                            DateTime { date, time },
                            false,
                            bag.offset,
                            false,
                            &zone,
                            Disambiguation::Compatible,
                            OffsetUse::Reject,
                            false,
                        )?;
                        Ok(Some(Relative::Zoned(nanoseconds, zone)))
                    }
                }
            }
            _ => Err(self.type_error("relativeTo must be a string or an object")),
        }
    }

    fn validate_increment(
        &mut self,
        increment: i64,
        maximum: Option<i128>,
        inclusive: bool,
    ) -> Result<(), ScriptError> {
        let Some(maximum) = maximum else {
            return Ok(());
        };
        let limit = if inclusive { maximum } else { maximum - 1 };
        if i128::from(increment) > limit || maximum % i128::from(increment) != 0 {
            return Err(
                self.temporal_range("roundingIncrement does not divide the next larger unit")
            );
        }
        Ok(())
    }

    // GetDifferenceSettings
    #[allow(clippy::too_many_arguments)]
    fn difference_settings(
        &mut self,
        since: bool,
        options: Value,
        group_date: bool,
        group_time: bool,
        disallowed: &[Unit],
        fallback_smallest: Unit,
        default_largest: Unit,
    ) -> Result<Settings, ScriptError> {
        let options = self.options_object(options)?;
        let largest = self.unit_option(&options, "largestUnit", true)?;
        let increment = self.increment_option(&options)?;
        let mode = self.rounding_mode_option(&options, RoundingMode::Trunc)?;
        let smallest = self.unit_option(&options, "smallestUnit", false)?;
        let allowed = |unit: Unit| {
            (if unit.is_date() {
                group_date
            } else {
                group_time
            }) && !disallowed.contains(&unit)
        };
        if largest.is_some_and(|unit| !allowed(unit)) || smallest.is_some_and(|unit| !allowed(unit))
        {
            return Err(self.temporal_range("That unit is not allowed here"));
        }
        let smallest = smallest.unwrap_or(fallback_smallest);
        let largest = largest.unwrap_or_else(|| iso::larger(default_largest, smallest));
        if iso::larger(largest, smallest) != largest {
            return Err(self.temporal_range("smallestUnit is larger than largestUnit"));
        }
        if !smallest.is_date() {
            self.validate_increment(increment, smallest.maximum_increment(), false)?;
        }
        let mode = if since { mode.negated() } else { mode };
        Ok(Settings {
            largest,
            smallest,
            increment,
            mode,
        })
    }

    fn duration_value(
        &mut self,
        internal: Internal,
        largest: Unit,
        since: bool,
    ) -> Result<Value, ScriptError> {
        let Some(fields) = iso::fields_of(internal, largest) else {
            return Err(self.temporal_range("The duration is out of range"));
        };
        let fields = if since {
            negate_fields(&fields)
        } else {
            fields
        };
        Ok(self.wrap_temporal(Temporal::Duration(fields)))
    }

    // DifferencePlainDateTimeWithRounding
    fn date_time_difference_rounded(
        &mut self,
        one: DateTime,
        two: DateTime,
        settings: &Settings,
    ) -> Result<Internal, ScriptError> {
        if one == two {
            return Ok(Internal::default());
        }
        let difference = iso::date_time_difference(one, two, settings.largest);
        if settings.smallest == Unit::Nanosecond && settings.increment == 1 {
            return Ok(difference);
        }
        iso::round_relative(
            difference,
            two.utc_epoch_ns(),
            one,
            None,
            settings.largest,
            settings.increment,
            settings.smallest,
            settings.mode,
        )
        .ok_or_else(|| self.temporal_range("The rounded duration is out of range"))
    }

    // DifferenceZonedDateTimeWithRounding
    fn zoned_difference_rounded(
        &mut self,
        zone: &TimeZone,
        ns1: i128,
        ns2: i128,
        settings: &Settings,
    ) -> Result<Internal, ScriptError> {
        if !settings.largest.is_date() {
            let time = round_time_duration(
                ns2 - ns1,
                settings.increment,
                settings.smallest,
                settings.mode,
            );
            return Ok(Internal {
                date: DateDuration::default(),
                time,
            });
        }
        let difference = iso::zoned_difference(zone, ns1, ns2, settings.largest)
            .ok_or_else(|| self.temporal_range("The difference is out of range"))?;
        if settings.smallest == Unit::Nanosecond && settings.increment == 1 {
            return Ok(difference);
        }
        let origin = iso::date_time_for(zone, ns1);
        iso::round_relative(
            difference,
            ns2,
            origin,
            Some(zone),
            settings.largest,
            settings.increment,
            settings.smallest,
            settings.mode,
        )
        .ok_or_else(|| self.temporal_range("The rounded duration is out of range"))
    }

    // AddZonedDateTime
    fn add_zoned(
        &mut self,
        nanoseconds: i128,
        zone: &TimeZone,
        internal: Internal,
        overflow: Overflow,
    ) -> Result<i128, ScriptError> {
        if internal.date == DateDuration::default() {
            return self.checked_instant(Some(nanoseconds + internal.time));
        }
        let local = iso::date_time_for(zone, nanoseconds);
        let Some(date) = iso::add_date(local.date, internal.date, overflow) else {
            return Err(self.temporal_range("The date is out of range"));
        };
        let intermediate = self.checked_date_time(Some(DateTime {
            date,
            time: local.time,
        }))?;
        let Some(start) = iso::epoch_ns_for(zone, intermediate, Disambiguation::Compatible) else {
            return Err(self.temporal_range("The date is out of range"));
        };
        self.checked_instant(Some(start + internal.time))
    }

    pub(super) fn temporal_from(
        &mut self,
        kind: Kind,
        item: Value,
        options: Value,
    ) -> Result<Value, ScriptError> {
        let value = match kind {
            Kind::Duration => Temporal::Duration(self.temporal_duration(item)?),
            Kind::Instant => Temporal::Instant(self.temporal_instant(item)?),
            Kind::Date => {
                let (date, calendar) = self.temporal_date(item, options)?;
                Temporal::Date(date, calendar)
            }
            Kind::Time => Temporal::Time(self.temporal_time(item, options)?),
            Kind::DateTime => {
                let (value, calendar) = self.temporal_date_time(item, options)?;
                Temporal::DateTime(value, calendar)
            }
            Kind::Zoned => {
                let (nanoseconds, zone, calendar) = self.temporal_zoned(item, options)?;
                Temporal::Zoned(nanoseconds, zone, calendar)
            }
            Kind::YearMonth => {
                let (date, calendar) = self.temporal_year_month(item, options)?;
                Temporal::YearMonth(date, calendar)
            }
            Kind::MonthDay => {
                let (date, calendar) = self.temporal_month_day(item, options)?;
                Temporal::MonthDay(date, calendar)
            }
        };
        Ok(self.wrap_temporal(value))
    }

    pub(super) fn temporal_compare(
        &mut self,
        kind: Kind,
        one: Value,
        two: Value,
        options: Value,
    ) -> Result<Value, ScriptError> {
        let ordering = match kind {
            Kind::Duration => return self.compare_durations(one, two, options),
            Kind::Instant => self
                .temporal_instant(one)?
                .cmp(&self.temporal_instant(two)?),
            Kind::Date => self
                .temporal_date(one, Value::Undefined)?
                .0
                .cmp(&self.temporal_date(two, Value::Undefined)?.0),
            Kind::Time => self
                .temporal_time(one, Value::Undefined)?
                .cmp(&self.temporal_time(two, Value::Undefined)?),
            Kind::DateTime => self
                .temporal_date_time(one, Value::Undefined)?
                .0
                .cmp(&self.temporal_date_time(two, Value::Undefined)?.0),
            Kind::Zoned => self
                .temporal_zoned(one, Value::Undefined)?
                .0
                .cmp(&self.temporal_zoned(two, Value::Undefined)?.0),
            Kind::YearMonth => self
                .temporal_year_month(one, Value::Undefined)?
                .0
                .cmp(&self.temporal_year_month(two, Value::Undefined)?.0),
            Kind::MonthDay => return Err(self.type_error("PlainMonthDay has no compare")),
        };
        Ok(Value::Number(match ordering {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    }

    fn compare_durations(
        &mut self,
        one: Value,
        two: Value,
        options: Value,
    ) -> Result<Value, ScriptError> {
        let one = self.temporal_duration(one)?;
        let two = self.temporal_duration(two)?;
        let options = self.options_object(options)?;
        let relative = self.relative_to(&options)?;
        if one == two {
            return Ok(Value::Number(0.0));
        }
        let calendar_units = one[..3].iter().chain(&two[..3]).any(|value| *value != 0.0);
        let first = iso::internal_of(&one);
        let second = iso::internal_of(&two);
        let has_days = one[3] != 0.0 || two[3] != 0.0;
        if let Some(Relative::Zoned(nanoseconds, zone)) = &relative
            && (calendar_units || has_days)
        {
            let after_one = self.add_zoned(*nanoseconds, zone, first, Overflow::Constrain)?;
            let after_two = self.add_zoned(*nanoseconds, zone, second, Overflow::Constrain)?;
            return Ok(Value::Number(match after_one.cmp(&after_two) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            }));
        }
        let days = |runtime: &mut Self, internal: &Internal| -> Result<i128, ScriptError> {
            if !calendar_units {
                return Ok(i128::from(internal.date.days));
            }
            match &relative {
                Some(Relative::Plain(date)) => {
                    let Some(later) = iso::add_date(*date, internal.date, Overflow::Constrain)
                    else {
                        return Err(runtime.temporal_range("The date is out of range"));
                    };
                    Ok(i128::from(later.epoch_days() - date.epoch_days()))
                }
                _ => Err(runtime.temporal_range("Comparing calendar units needs relativeTo")),
            }
        };
        let total_one = days(self, &first)? * iso::NS_PER_DAY + first.time;
        let total_two = days(self, &second)? * iso::NS_PER_DAY + second.time;
        Ok(Value::Number(match total_one.cmp(&total_two) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    }

    pub(super) fn temporal_field(
        &mut self,
        value: &Temporal,
        field: Field,
    ) -> Result<Value, ScriptError> {
        let number = |value: i64| Value::Number(value as f64);
        let (date, calendar) = match value {
            Temporal::Duration(fields) => {
                return Ok(match field {
                    Field::Duration(index) => Value::Number(fields[usize::from(index)]),
                    Field::Sign => number(iso::fields_sign(fields)),
                    _ => Value::Bool(iso::fields_sign(fields) == 0),
                });
            }
            Temporal::Instant(nanoseconds) => {
                return Ok(match field {
                    Field::EpochMilliseconds => number(nanoseconds.div_euclid(1_000_000) as i64),
                    _ => Value::BigInt(Rc::new(super::super::bigint::BigInt::from_i128(
                        *nanoseconds,
                    ))),
                });
            }
            Temporal::Time(time) => {
                let Field::Time(index) = field else {
                    unreachable!("a time has only time fields")
                };
                return Ok(number(time.fields()[usize::from(index)]));
            }
            Temporal::Date(date, calendar)
            | Temporal::YearMonth(date, calendar)
            | Temporal::MonthDay(date, calendar) => (*date, *calendar),
            Temporal::DateTime(value, calendar) => {
                if let Field::Time(index) = field {
                    return Ok(number(value.time.fields()[usize::from(index)]));
                }
                (value.date, *calendar)
            }
            Temporal::Zoned(nanoseconds, zone, calendar) => {
                let offset = iso::Zone::offset_ns(zone, *nanoseconds);
                let local = iso::date_time_for(zone, *nanoseconds);
                match field {
                    Field::Time(index) => {
                        return Ok(number(local.time.fields()[usize::from(index)]));
                    }
                    Field::TimeZoneId => return Ok(Value::String(zone.id().into())),
                    Field::EpochMilliseconds => {
                        return Ok(number(nanoseconds.div_euclid(1_000_000) as i64));
                    }
                    Field::EpochNanoseconds => {
                        return Ok(Value::BigInt(Rc::new(
                            super::super::bigint::BigInt::from_i128(*nanoseconds),
                        )));
                    }
                    Field::OffsetNanoseconds => return Ok(Value::Number(offset as f64)),
                    Field::Offset => return Ok(Value::String(offset_text(offset, true).into())),
                    Field::HoursInDay => {
                        let today = start_of_day(zone, local.date);
                        let tomorrow = Date::from_epoch_days(local.date.epoch_days() + 1)
                            .and_then(|date| start_of_day(zone, date));
                        let (Some(today), Some(tomorrow)) = (today, tomorrow) else {
                            return Err(self.temporal_range("The day is out of range"));
                        };
                        return Ok(Value::Number(iso::fraction_to_f64(
                            tomorrow - today,
                            3_600_000_000_000,
                        )));
                    }
                    _ => {}
                }
                (local.date, *calendar)
            }
        };
        let year = i64::from(date.year);
        Ok(match field {
            Field::CalendarId => Value::String(calendar.id().into()),
            Field::Era | Field::EraYear if calendar == Calendar::Iso => Value::Undefined,
            Field::Era => Value::String(if year > 0 { "ce" } else { "bce" }.into()),
            Field::EraYear => number(if year > 0 { year } else { 1 - year }),
            Field::Year => number(year),
            Field::Month => number(i64::from(date.month)),
            Field::MonthCode => Value::String(month_code_text(date.month).into()),
            Field::Day => number(i64::from(date.day)),
            Field::DayOfWeek => number(date.day_of_week()),
            Field::DayOfYear => number(date.day_of_year()),
            Field::WeekOfYear => number(date.week_of_year().0),
            Field::YearOfWeek => number(date.week_of_year().1),
            Field::DaysInWeek => number(7),
            Field::DaysInMonth => number(iso::days_in_month(year, i64::from(date.month))),
            Field::DaysInYear => number(if iso::is_leap(year) { 366 } else { 365 }),
            Field::MonthsInYear => number(12),
            Field::InLeapYear => Value::Bool(iso::is_leap(year)),
            _ => Value::Undefined,
        })
    }

    // the partial fields of a with() argument, which may be no Temporal object and name no
    // calendar or time zone
    fn with_bag(
        &mut self,
        like: &Value,
        calendar: Calendar,
        want: Want,
    ) -> Result<Bag, ScriptError> {
        if is_primitive(like) {
            return Err(self.type_error("with() takes an object"));
        }
        if Self::temporal_of(like).is_some() {
            return Err(self.type_error("with() does not take a Temporal object"));
        }
        for name in ["calendar", "timeZone"] {
            if !matches!(self.get_property(like, name)?, Value::Undefined) {
                return Err(self.type_error(format!("with() does not take {name}")));
            }
        }
        let bag = self.read_bag(like, calendar, want)?;
        if !bag.any {
            return Err(self.type_error("with() needs at least one field"));
        }
        Ok(bag)
    }

    // the existing fields overlaid by the partial ones, with month and monthCode replacing each other
    fn merged(
        &mut self,
        date: Date,
        time: Time,
        partial: Bag,
        calendar: Calendar,
    ) -> Result<Bag, ScriptError> {
        let mut merged = Bag {
            year: Some(i64::from(date.year)),
            month: Some(i64::from(date.month)),
            month_code: Some(i64::from(date.month)),
            day: Some(i64::from(date.day)),
            time: time.fields().map(Some),
            any: true,
            ..Bag::default()
        };
        if partial.month.is_some() || partial.month_code.is_some() {
            merged.month = partial.month;
            merged.month_code = partial.month_code;
        }
        if calendar == Calendar::Gregory
            && (partial.era.is_some() || partial.era_year.is_some() || partial.year.is_some())
        {
            merged.year = partial.year;
            merged.era = partial.era;
            merged.era_year = partial.era_year;
            if merged.year.is_none() && merged.era.is_none() {
                merged.year = Some(i64::from(date.year));
            }
        } else if partial.year.is_some() {
            merged.year = partial.year;
        }
        if partial.day.is_some() {
            merged.day = partial.day;
        }
        for (slot, given) in merged.time.iter_mut().zip(partial.time) {
            if given.is_some() {
                *slot = given;
            }
        }
        merged.offset = partial.offset;
        Ok(merged)
    }

    pub(super) fn temporal_method(
        &mut self,
        value: Rc<Temporal>,
        method: Method,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::ValueOf => {
                return Err(self.type_error(format!(
                    "Temporal.{}.prototype.valueOf cannot be used; use compare() or equals()",
                    kind_name(value.kind())
                )));
            }
            Method::ToJson => return self.temporal_string(&value, Value::Undefined),
            Method::ToString => return self.temporal_string(&value, argument(0)),
            Method::ToLocaleString => {
                return self.temporal_locale_string(&value, argument(0), argument(1));
            }
            _ => {}
        }
        match &*value {
            Temporal::Duration(fields) => self.duration_method(*fields, method, &arguments),
            Temporal::Instant(nanoseconds) => self.instant_method(*nanoseconds, method, &arguments),
            Temporal::Date(date, calendar) => {
                self.date_method(*date, *calendar, method, &arguments)
            }
            Temporal::Time(time) => self.time_method(*time, method, &arguments),
            Temporal::DateTime(date_time, calendar) => {
                self.date_time_method(*date_time, *calendar, method, &arguments)
            }
            Temporal::Zoned(nanoseconds, zone, calendar) => {
                self.zoned_method(*nanoseconds, zone.clone(), *calendar, method, &arguments)
            }
            Temporal::YearMonth(date, calendar) => {
                self.year_month_method(*date, *calendar, method, &arguments)
            }
            Temporal::MonthDay(date, calendar) => {
                self.month_day_method(*date, *calendar, method, &arguments)
            }
        }
    }

    fn duration_method(
        &mut self,
        fields: Fields,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::With => {
                if is_primitive(&argument(0)) {
                    return Err(self.type_error("with() takes an object"));
                }
                let merged = self.duration_like(&argument(0), Some(&fields))?;
                let merged = self.checked_fields(merged)?;
                Ok(self.wrap_temporal(Temporal::Duration(merged)))
            }
            Method::Negated => Ok(self.wrap_temporal(Temporal::Duration(negate_fields(&fields)))),
            Method::Abs => Ok(self.wrap_temporal(Temporal::Duration(fields.map(f64::abs)))),
            Method::Add | Method::Subtract => {
                let mut other = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    other = negate_fields(&other);
                }
                let largest = iso::larger(duration_largest(&fields), duration_largest(&other));
                if largest.is_calendar() {
                    return Err(self.temporal_range(
                        "Adding durations with calendar units needs a date to add them to",
                    ));
                }
                let time = iso::internal_with_24_hour_days(&fields).time
                    + iso::internal_with_24_hour_days(&other).time;
                self.duration_value(
                    Internal {
                        date: DateDuration::default(),
                        time,
                    },
                    largest,
                    false,
                )
            }
            Method::Round => self.duration_round(fields, argument(0)),
            Method::Total => self.duration_total(fields, argument(0)),
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn duration_round(&mut self, fields: Fields, round_to: Value) -> Result<Value, ScriptError> {
        let options = match round_to {
            Value::Undefined => return Err(self.type_error("round() needs options")),
            Value::String(unit) => Options(Some(
                self.object_from_entries(vec![("smallestUnit".to_string(), Value::String(unit))]),
            )),
            other => self.options_object(other)?,
        };
        let largest_option = self.option_string(&options, "largestUnit")?;
        let relative = self.relative_to(&options)?;
        let increment = self.increment_option(&options)?;
        let mode = self.rounding_mode_option(&options, RoundingMode::HalfExpand)?;
        let smallest = self.unit_option(&options, "smallestUnit", false)?;
        let largest = match largest_option.as_deref() {
            None | Some("auto") => None,
            Some(name) => match Unit::named(name) {
                Some(unit) => Some(unit),
                None => {
                    return Err(
                        self.range_error(format!("{name} is not a valid value for largestUnit"))
                    );
                }
            },
        };
        if smallest.is_none() && largest_option.is_none() {
            return Err(self.temporal_range("round() needs smallestUnit or largestUnit"));
        }
        let smallest = smallest.unwrap_or(Unit::Nanosecond);
        let default_largest = iso::larger(duration_largest(&fields), smallest);
        let largest = largest.unwrap_or(default_largest);
        if iso::larger(largest, smallest) != largest {
            return Err(self.temporal_range("smallestUnit is larger than largestUnit"));
        }
        if !smallest.is_date() {
            self.validate_increment(increment, smallest.maximum_increment(), false)?;
        }
        if increment > 1 && largest != smallest && smallest.is_date() {
            return Err(self.temporal_range(
                "roundingIncrement must be 1 when rounding to a date unit across units",
            ));
        }
        let settings = Settings {
            largest,
            smallest,
            increment,
            mode,
        };
        let internal = match relative {
            Some(Relative::Zoned(nanoseconds, zone)) => {
                let target = self.add_zoned(
                    nanoseconds,
                    &zone,
                    iso::internal_of(&fields),
                    Overflow::Constrain,
                )?;
                self.zoned_difference_rounded(&zone, nanoseconds, target, &settings)?
            }
            Some(Relative::Plain(date)) => {
                let internal = iso::internal_of(&fields);
                let (days, time) = Time::balance(internal.time);
                let duration = DateDuration {
                    days: internal.date.days + days,
                    ..internal.date
                };
                let Some(target) = iso::add_date(date, duration, Overflow::Constrain) else {
                    return Err(self.temporal_range("The date is out of range"));
                };
                let start = DateTime {
                    date,
                    time: Time::default(),
                };
                let end = self.checked_date_time(Some(DateTime { date: target, time }))?;
                self.date_time_difference_rounded(start, end, &settings)?
            }
            None => {
                if fields[..3].iter().any(|value| *value != 0.0) || largest.is_calendar() {
                    return Err(self.temporal_range("Rounding calendar units needs relativeTo"));
                }
                let internal = iso::internal_with_24_hour_days(&fields);
                if smallest == Unit::Day {
                    let days = iso::round_to_increment(
                        internal.time,
                        i128::from(increment) * iso::NS_PER_DAY,
                        mode,
                    ) / iso::NS_PER_DAY;
                    Internal {
                        date: DateDuration {
                            days: days as i64,
                            ..Default::default()
                        },
                        time: 0,
                    }
                } else {
                    Internal {
                        date: DateDuration::default(),
                        time: round_time_duration(internal.time, increment, smallest, mode),
                    }
                }
            }
        };
        self.duration_value(internal, largest, false)
    }

    fn duration_total(&mut self, fields: Fields, total_of: Value) -> Result<Value, ScriptError> {
        let options = match total_of {
            Value::Undefined => return Err(self.type_error("total() needs a unit")),
            Value::String(unit) => Options(Some(
                self.object_from_entries(vec![("unit".to_string(), Value::String(unit))]),
            )),
            other => self.options_object(other)?,
        };
        let relative = self.relative_to(&options)?;
        let Some(unit) = self.unit_option(&options, "unit", false)? else {
            return Err(self.temporal_range("total() needs a unit"));
        };
        let fraction = match relative {
            Some(Relative::Zoned(nanoseconds, zone)) => {
                let target = self.add_zoned(
                    nanoseconds,
                    &zone,
                    iso::internal_of(&fields),
                    Overflow::Constrain,
                )?;
                if target == nanoseconds {
                    (0, 1)
                } else if !unit.is_date() {
                    (target - nanoseconds, unit.nanoseconds())
                } else {
                    let difference = iso::zoned_difference(&zone, nanoseconds, target, unit)
                        .ok_or_else(|| self.temporal_range("The difference is out of range"))?;
                    let origin = iso::date_time_for(&zone, nanoseconds);
                    iso::total_relative(difference, target, origin, Some(&zone), unit)
                        .ok_or_else(|| self.temporal_range("The total is out of range"))?
                }
            }
            Some(Relative::Plain(date)) => {
                let internal = iso::internal_of(&fields);
                let (days, time) = Time::balance(internal.time);
                let duration = DateDuration {
                    days: internal.date.days + days,
                    ..internal.date
                };
                let Some(target) = iso::add_date(date, duration, Overflow::Constrain) else {
                    return Err(self.temporal_range("The date is out of range"));
                };
                let start = DateTime {
                    date,
                    time: Time::default(),
                };
                let end = self.checked_date_time(Some(DateTime { date: target, time }))?;
                if start == end {
                    (0, 1)
                } else {
                    let difference = iso::date_time_difference(start, end, unit);
                    iso::total_relative(difference, end.utc_epoch_ns(), start, None, unit)
                        .ok_or_else(|| self.temporal_range("The total is out of range"))?
                }
            }
            None => {
                if fields[..3].iter().any(|value| *value != 0.0) || unit.is_calendar() {
                    return Err(self.temporal_range("A total in calendar units needs relativeTo"));
                }
                (
                    iso::internal_with_24_hour_days(&fields).time,
                    unit.nanoseconds(),
                )
            }
        };
        Ok(Value::Number(iso::fraction_to_f64(fraction.0, fraction.1)))
    }

    fn instant_method(
        &mut self,
        nanoseconds: i128,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::Add | Method::Subtract => {
                let mut fields = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    fields = negate_fields(&fields);
                }
                if fields[..4].iter().any(|value| *value != 0.0) {
                    return Err(
                        self.temporal_range("An instant takes no years, months, weeks or days")
                    );
                }
                let time = iso::internal_of(&fields).time;
                let result = self.checked_instant(Some(nanoseconds + time))?;
                Ok(self.wrap_temporal(Temporal::Instant(result)))
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let other = self.temporal_instant(argument(0))?;
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    false,
                    true,
                    &[],
                    Unit::Nanosecond,
                    Unit::Second,
                )?;
                let time = round_time_duration(
                    other - nanoseconds,
                    settings.increment,
                    settings.smallest,
                    settings.mode,
                );
                self.duration_value(
                    Internal {
                        date: DateDuration::default(),
                        time,
                    },
                    settings.largest,
                    since,
                )
            }
            Method::Round => {
                let (smallest, increment, mode) = self.round_options(argument(0), false)?;
                let maximum = iso::NS_PER_DAY / smallest.nanoseconds();
                self.validate_increment(increment, Some(maximum), true)?;
                let rounded = iso::round_to_increment(
                    nanoseconds,
                    i128::from(increment) * smallest.nanoseconds(),
                    mode,
                );
                let rounded = self.checked_instant(Some(rounded))?;
                Ok(self.wrap_temporal(Temporal::Instant(rounded)))
            }
            Method::Equals => {
                let other = self.temporal_instant(argument(0))?;
                Ok(Value::Bool(other == nanoseconds))
            }
            Method::ToZonedDateTimeIso => {
                let zone = self.temporal_zone(argument(0))?;
                Ok(self.wrap_temporal(Temporal::Zoned(nanoseconds, zone, Calendar::Iso)))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    // the smallestUnit, increment and mode round() takes, as a unit name or an options bag
    fn round_options(
        &mut self,
        round_to: Value,
        allow_day: bool,
    ) -> Result<(Unit, i64, RoundingMode), ScriptError> {
        let options = match round_to {
            Value::Undefined => return Err(self.type_error("round() needs options")),
            Value::String(unit) => Options(Some(
                self.object_from_entries(vec![("smallestUnit".to_string(), Value::String(unit))]),
            )),
            other => self.options_object(other)?,
        };
        let increment = self.increment_option(&options)?;
        let mode = self.rounding_mode_option(&options, RoundingMode::HalfExpand)?;
        let Some(smallest) = self.unit_option(&options, "smallestUnit", false)? else {
            return Err(self.temporal_range("round() needs smallestUnit"));
        };
        if smallest.is_calendar() || (smallest == Unit::Day && !allow_day) {
            return Err(self.temporal_range("That smallestUnit is not allowed here"));
        }
        Ok((smallest, increment, mode))
    }

    fn add_to_date(
        &mut self,
        date: Date,
        method: Method,
        arguments: &[Value],
    ) -> Result<Date, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let mut fields = self.temporal_duration(argument(0))?;
        if method == Method::Subtract {
            fields = negate_fields(&fields);
        }
        let options = self.options_object(argument(1))?;
        let overflow = self.overflow_option(&options)?;
        let duration = iso::date_duration_without_time(&fields);
        let result = iso::add_date(date, duration, overflow);
        if result.is_none() {
            return Err(self.temporal_range("The date is out of range or not valid"));
        }
        self.checked_date(result)
    }

    fn date_method(
        &mut self,
        date: Date,
        calendar: Calendar,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::ToPlainYearMonth => {
                Ok(self.wrap_temporal(Temporal::YearMonth(Date { day: 1, ..date }, calendar)))
            }
            Method::ToPlainMonthDay => {
                let month_day = iso::regulate_date(
                    1972,
                    i64::from(date.month),
                    i64::from(date.day),
                    Overflow::Constrain,
                )
                .expect("a valid month and day");
                Ok(self.wrap_temporal(Temporal::MonthDay(month_day, calendar)))
            }
            Method::Add | Method::Subtract => {
                let result = self.add_to_date(date, method, arguments)?;
                Ok(self.wrap_temporal(Temporal::Date(result, calendar)))
            }
            Method::With => {
                let partial = self.with_bag(&argument(0), calendar, Want::Date)?;
                let merged = self.merged(date, Time::default(), partial, calendar)?;
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let result = self.bag_date(&merged, calendar, overflow)?;
                Ok(self.wrap_temporal(Temporal::Date(result, calendar)))
            }
            Method::WithCalendar => {
                let calendar = self.temporal_calendar(argument(0))?;
                Ok(self.wrap_temporal(Temporal::Date(date, calendar)))
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let (other, other_calendar) = self.temporal_date(argument(0), Value::Undefined)?;
                if other_calendar != calendar {
                    return Err(self.temporal_range("The dates are in different calendars"));
                }
                let time_units = [
                    Unit::Hour,
                    Unit::Minute,
                    Unit::Second,
                    Unit::Millisecond,
                    Unit::Microsecond,
                    Unit::Nanosecond,
                ];
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    true,
                    false,
                    &time_units,
                    Unit::Day,
                    Unit::Day,
                )?;
                if other == date {
                    return self.duration_value(Internal::default(), Unit::Day, since);
                }
                let mut internal = Internal {
                    date: iso::date_until(date, other, settings.largest),
                    time: 0,
                };
                if !(settings.smallest == Unit::Day && settings.increment == 1) {
                    let start = DateTime {
                        date,
                        time: Time::default(),
                    };
                    let end = DateTime {
                        date: other,
                        time: Time::default(),
                    };
                    internal = iso::round_relative(
                        internal,
                        end.utc_epoch_ns(),
                        start,
                        None,
                        settings.largest,
                        settings.increment,
                        settings.smallest,
                        settings.mode,
                    )
                    .ok_or_else(|| self.temporal_range("The rounded duration is out of range"))?;
                }
                self.duration_value(internal, Unit::Day, since)
            }
            Method::Equals => {
                let (other, other_calendar) = self.temporal_date(argument(0), Value::Undefined)?;
                Ok(Value::Bool(other == date && other_calendar == calendar))
            }
            Method::ToPlainDateTime => {
                let time = match argument(0) {
                    Value::Undefined => Time::default(),
                    other => self.temporal_time(other, Value::Undefined)?,
                };
                let value = self.checked_date_time(Some(DateTime { date, time }))?;
                Ok(self.wrap_temporal(Temporal::DateTime(value, calendar)))
            }
            Method::ToZonedDateTime => {
                let item = argument(0);
                let (zone, time) = if let Value::Object(_) = &item
                    && Self::temporal_of(&item).is_none()
                {
                    let zone_like = self.get_property(&item, "timeZone")?;
                    if matches!(zone_like, Value::Undefined) {
                        (self.temporal_zone(item)?, None)
                    } else {
                        let zone = self.temporal_zone(zone_like)?;
                        let time = match self.get_property(&item, "plainTime")? {
                            Value::Undefined => None,
                            other => Some(self.temporal_time(other, Value::Undefined)?),
                        };
                        (zone, time)
                    }
                } else {
                    (self.temporal_zone(item)?, None)
                };
                let nanoseconds = match time {
                    None => {
                        let found = start_of_day(&zone, date);
                        self.checked_instant(found)?
                    }
                    Some(time) => {
                        let local = self.checked_date_time(Some(DateTime { date, time }))?;
                        let found = iso::epoch_ns_for(&zone, local, Disambiguation::Compatible);
                        self.checked_instant(found)?
                    }
                };
                Ok(self.wrap_temporal(Temporal::Zoned(nanoseconds, zone, calendar)))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn time_method(
        &mut self,
        time: Time,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::Add | Method::Subtract => {
                let mut fields = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    fields = negate_fields(&fields);
                }
                let (_, result) =
                    Time::balance(time.nanoseconds() + iso::internal_of(&fields).time);
                Ok(self.wrap_temporal(Temporal::Time(result)))
            }
            Method::With => {
                let partial = self.with_bag(&argument(0), Calendar::Iso, Want::Time)?;
                let fields: [i64; 6] = std::array::from_fn(|index| {
                    partial.time[index].unwrap_or(time.fields()[index])
                });
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                match iso::regulate_time(fields, overflow) {
                    Some(result) => Ok(self.wrap_temporal(Temporal::Time(result))),
                    None => Err(self.temporal_range("The time is not valid")),
                }
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let other = self.temporal_time(argument(0), Value::Undefined)?;
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    false,
                    true,
                    &[],
                    Unit::Nanosecond,
                    Unit::Hour,
                )?;
                let difference = other.nanoseconds() - time.nanoseconds();
                let rounded = round_time_duration(
                    difference,
                    settings.increment,
                    settings.smallest,
                    settings.mode,
                );
                self.duration_value(
                    Internal {
                        date: DateDuration::default(),
                        time: rounded,
                    },
                    settings.largest,
                    since,
                )
            }
            Method::Round => {
                let (smallest, increment, mode) = self.round_options(argument(0), false)?;
                self.validate_increment(increment, smallest.maximum_increment(), false)?;
                let (_, result) = round_time(time, increment, smallest, mode);
                Ok(self.wrap_temporal(Temporal::Time(result)))
            }
            Method::Equals => {
                let other = self.temporal_time(argument(0), Value::Undefined)?;
                Ok(Value::Bool(other == time))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn date_time_method(
        &mut self,
        value: DateTime,
        calendar: Calendar,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::Add | Method::Subtract => {
                let mut fields = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    fields = negate_fields(&fields);
                }
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let internal = iso::internal_with_24_hour_days(&fields);
                let (days, time) = Time::balance(value.time.nanoseconds() + internal.time);
                let duration = DateDuration {
                    days,
                    ..internal.date
                };
                let date = iso::add_date(value.date, duration, overflow);
                if date.is_none() {
                    return Err(self.temporal_range("The date is out of range or not valid"));
                }
                let date = self.checked_date(date)?;
                let result = self.checked_date_time(Some(DateTime { date, time }))?;
                Ok(self.wrap_temporal(Temporal::DateTime(result, calendar)))
            }
            Method::With => {
                let partial = self.with_bag(&argument(0), calendar, Want::DateTime)?;
                let merged = self.merged(value.date, value.time, partial, calendar)?;
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let date = self.bag_date(&merged, calendar, overflow)?;
                let time = self.bag_time(&merged, overflow)?;
                let result = self.checked_date_time(Some(DateTime { date, time }))?;
                Ok(self.wrap_temporal(Temporal::DateTime(result, calendar)))
            }
            Method::WithPlainTime => {
                let time = match argument(0) {
                    Value::Undefined => Time::default(),
                    other => self.temporal_time(other, Value::Undefined)?,
                };
                Ok(self.wrap_temporal(Temporal::DateTime(
                    DateTime {
                        date: value.date,
                        time,
                    },
                    calendar,
                )))
            }
            Method::WithCalendar => {
                let calendar = self.temporal_calendar(argument(0))?;
                Ok(self.wrap_temporal(Temporal::DateTime(value, calendar)))
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let (other, other_calendar) =
                    self.temporal_date_time(argument(0), Value::Undefined)?;
                if other_calendar != calendar {
                    return Err(self.temporal_range("The dates are in different calendars"));
                }
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    true,
                    true,
                    &[],
                    Unit::Nanosecond,
                    Unit::Day,
                )?;
                let internal = self.date_time_difference_rounded(value, other, &settings)?;
                self.duration_value(internal, settings.largest, since)
            }
            Method::Round => {
                let (smallest, increment, mode) = self.round_options(argument(0), true)?;
                let (maximum, inclusive) = if smallest == Unit::Day {
                    (Some(1), true)
                } else {
                    (smallest.maximum_increment(), false)
                };
                self.validate_increment(increment, maximum, inclusive)?;
                let rounded = round_date_time(value, increment, smallest, mode);
                let rounded = self.checked_date_time(rounded)?;
                Ok(self.wrap_temporal(Temporal::DateTime(rounded, calendar)))
            }
            Method::Equals => {
                let (other, other_calendar) =
                    self.temporal_date_time(argument(0), Value::Undefined)?;
                Ok(Value::Bool(other == value && other_calendar == calendar))
            }
            Method::ToZonedDateTime => {
                let zone = self.temporal_zone(argument(0))?;
                let options = self.options_object(argument(1))?;
                let disambiguation = self.disambiguation_option(&options)?;
                let found = iso::epoch_ns_for(&zone, value, disambiguation);
                if found.is_none() {
                    return Err(self.temporal_range(
                        "The wall clock time is not unambiguous in the time zone",
                    ));
                }
                let nanoseconds = self.checked_instant(found)?;
                Ok(self.wrap_temporal(Temporal::Zoned(nanoseconds, zone, calendar)))
            }
            Method::ToPlainDate => Ok(self.wrap_temporal(Temporal::Date(value.date, calendar))),
            Method::ToPlainTime => Ok(self.wrap_temporal(Temporal::Time(value.time))),
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn zoned_method(
        &mut self,
        nanoseconds: i128,
        zone: TimeZone,
        calendar: Calendar,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let local = iso::date_time_for(&zone, nanoseconds);
        let zoned = |runtime: &Self, nanoseconds: i128, zone: TimeZone| {
            runtime.wrap_temporal(Temporal::Zoned(nanoseconds, zone, calendar))
        };
        match method {
            Method::Add | Method::Subtract => {
                let mut fields = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    fields = negate_fields(&fields);
                }
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let result =
                    self.add_zoned(nanoseconds, &zone, iso::internal_of(&fields), overflow)?;
                Ok(zoned(self, result, zone))
            }
            Method::With => {
                let partial = self.with_bag(&argument(0), calendar, Want::Zoned)?;
                let offset = iso::Zone::offset_ns(&zone, nanoseconds);
                let mut merged = self.merged(local.date, local.time, partial, calendar)?;
                if merged.offset.is_none() {
                    merged.offset = Some(offset);
                }
                let options = self.options_object(argument(1))?;
                let disambiguation = self.disambiguation_option(&options)?;
                let offset_use = self.offset_use_option(&options, "prefer")?;
                let overflow = self.overflow_option(&options)?;
                let date = self.bag_date(&merged, calendar, overflow)?;
                let time = self.bag_time(&merged, overflow)?;
                let result = self.interpret_offset(
                    DateTime { date, time },
                    false,
                    merged.offset,
                    false,
                    &zone,
                    disambiguation,
                    offset_use,
                    false,
                )?;
                Ok(zoned(self, result, zone))
            }
            Method::WithPlainTime => {
                let result = match argument(0) {
                    Value::Undefined => {
                        let found = start_of_day(&zone, local.date);
                        self.checked_instant(found)?
                    }
                    other => {
                        let time = self.temporal_time(other, Value::Undefined)?;
                        let found = iso::epoch_ns_for(
                            &zone,
                            DateTime {
                                date: local.date,
                                time,
                            },
                            Disambiguation::Compatible,
                        );
                        self.checked_instant(found)?
                    }
                };
                Ok(zoned(self, result, zone))
            }
            Method::WithTimeZone => {
                let other = self.temporal_zone(argument(0))?;
                Ok(zoned(self, nanoseconds, other))
            }
            Method::WithCalendar => {
                let calendar = self.temporal_calendar(argument(0))?;
                Ok(self.wrap_temporal(Temporal::Zoned(nanoseconds, zone, calendar)))
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let (other, other_zone, other_calendar) =
                    self.temporal_zoned(argument(0), Value::Undefined)?;
                if other_calendar != calendar {
                    return Err(self.temporal_range("The dates are in different calendars"));
                }
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    true,
                    true,
                    &[],
                    Unit::Nanosecond,
                    Unit::Hour,
                )?;
                if settings.largest.is_date() && !zone.same(&other_zone) {
                    return Err(self
                        .temporal_range("Differences in days or longer need the same time zone"));
                }
                if other == nanoseconds {
                    return self.duration_value(Internal::default(), settings.largest, since);
                }
                let internal =
                    self.zoned_difference_rounded(&zone, nanoseconds, other, &settings)?;
                let largest = if settings.largest.is_date() {
                    Unit::Hour
                } else {
                    settings.largest
                };
                self.duration_value(internal, largest, since)
            }
            Method::Round => {
                let (smallest, increment, mode) = self.round_options(argument(0), true)?;
                let (maximum, inclusive) = if smallest == Unit::Day {
                    (Some(1), true)
                } else {
                    (smallest.maximum_increment(), false)
                };
                self.validate_increment(increment, maximum, inclusive)?;
                let result = if smallest == Unit::Day {
                    let start = start_of_day(&zone, local.date);
                    let end = Date::from_epoch_days(local.date.epoch_days() + 1)
                        .and_then(|date| start_of_day(&zone, date));
                    let (Some(start), Some(end)) = (start, end) else {
                        return Err(self.temporal_range("The day is out of range"));
                    };
                    start + iso::round_to_increment(nanoseconds - start, end - start, mode)
                } else {
                    let rounded = round_date_time(local, increment, smallest, mode);
                    let rounded = self.checked_date_time(rounded)?;
                    let offset = iso::Zone::offset_ns(&zone, nanoseconds);
                    self.interpret_offset(
                        rounded,
                        false,
                        Some(offset),
                        false,
                        &zone,
                        Disambiguation::Compatible,
                        OffsetUse::Prefer,
                        false,
                    )?
                };
                let result = self.checked_instant(Some(result))?;
                Ok(zoned(self, result, zone))
            }
            Method::Equals => {
                let (other, other_zone, other_calendar) =
                    self.temporal_zoned(argument(0), Value::Undefined)?;
                Ok(Value::Bool(
                    other == nanoseconds && zone.same(&other_zone) && other_calendar == calendar,
                ))
            }
            Method::StartOfDay => {
                let found = start_of_day(&zone, local.date);
                let result = self.checked_instant(found)?;
                Ok(zoned(self, result, zone))
            }
            Method::GetTimeZoneTransition => {
                let direction = match argument(0) {
                    Value::Undefined => {
                        return Err(self.type_error("getTimeZoneTransition() needs a direction"));
                    }
                    Value::String(direction) => direction.to_string(),
                    other => {
                        let options = self.options_object(other)?;
                        match self.option_string(&options, "direction")? {
                            Some(direction) => direction,
                            None => {
                                return Err(self
                                    .temporal_range("getTimeZoneTransition() needs a direction"));
                            }
                        }
                    }
                };
                let next = match direction.as_str() {
                    "next" => true,
                    "previous" => false,
                    _ => {
                        return Err(
                            self.range_error(format!("{direction} is not a valid direction"))
                        );
                    }
                };
                let TimeZone::Named(_, found) = &zone else {
                    return Ok(Value::Null);
                };
                let seconds = if next {
                    nanoseconds.div_euclid(iso::NS_PER_SECOND)
                } else {
                    -((-nanoseconds).div_euclid(iso::NS_PER_SECOND))
                };
                let transition = found
                    .transition(seconds as i64, next)
                    .map(|seconds| i128::from(seconds) * iso::NS_PER_SECOND)
                    .filter(|found| iso::instant_in_range(*found));
                Ok(match transition {
                    Some(found) => zoned(self, found, zone.clone()),
                    None => Value::Null,
                })
            }
            Method::ToInstant => Ok(self.wrap_temporal(Temporal::Instant(nanoseconds))),
            Method::ToPlainDate => Ok(self.wrap_temporal(Temporal::Date(local.date, calendar))),
            Method::ToPlainTime => Ok(self.wrap_temporal(Temporal::Time(local.time))),
            Method::ToPlainDateTime => Ok(self.wrap_temporal(Temporal::DateTime(local, calendar))),
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn year_month_method(
        &mut self,
        date: Date,
        calendar: Calendar,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::With => {
                let partial = self.with_bag(&argument(0), calendar, Want::YearMonth)?;
                let mut merged = self.merged(date, Time::default(), partial, calendar)?;
                merged.day = Some(1);
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let result = self.bag_date(&merged, calendar, overflow)?;
                if !iso::year_month_in_range(i64::from(result.year), i64::from(result.month)) {
                    return Err(self.temporal_range("The year and month are out of range"));
                }
                Ok(self.wrap_temporal(Temporal::YearMonth(result, calendar)))
            }
            Method::Add | Method::Subtract => {
                let mut fields = self.temporal_duration(argument(0))?;
                if method == Method::Subtract {
                    fields = negate_fields(&fields);
                }
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let first = Date { day: 1, ..date };
                let start = if iso::fields_sign(&fields) < 0 {
                    let next = iso::add_date(
                        first,
                        DateDuration {
                            months: 1,
                            ..Default::default()
                        },
                        Overflow::Constrain,
                    );
                    next.and_then(|next| Date::from_epoch_days(next.epoch_days() - 1))
                } else {
                    Some(first)
                };
                let added = start.and_then(|start| {
                    iso::add_date(start, iso::date_duration_without_time(&fields), overflow)
                });
                match added.filter(|added| {
                    iso::year_month_in_range(i64::from(added.year), i64::from(added.month))
                }) {
                    Some(added) => {
                        Ok(self
                            .wrap_temporal(Temporal::YearMonth(Date { day: 1, ..added }, calendar)))
                    }
                    None => Err(self.temporal_range("The year and month are out of range")),
                }
            }
            Method::Until | Method::Since => {
                let since = method == Method::Since;
                let (other, other_calendar) =
                    self.temporal_year_month(argument(0), Value::Undefined)?;
                if other_calendar != calendar {
                    return Err(self.temporal_range("The dates are in different calendars"));
                }
                let disallowed = [
                    Unit::Week,
                    Unit::Day,
                    Unit::Hour,
                    Unit::Minute,
                    Unit::Second,
                    Unit::Millisecond,
                    Unit::Microsecond,
                    Unit::Nanosecond,
                ];
                let settings = self.difference_settings(
                    since,
                    argument(1),
                    true,
                    false,
                    &disallowed,
                    Unit::Month,
                    Unit::Year,
                )?;
                let (one, two) = (Date { day: 1, ..date }, Date { day: 1, ..other });
                if one == two {
                    return self.duration_value(Internal::default(), Unit::Day, since);
                }
                let mut difference = iso::date_until(one, two, settings.largest);
                difference.weeks = 0;
                difference.days = 0;
                let mut internal = Internal {
                    date: difference,
                    time: 0,
                };
                if !(settings.smallest == Unit::Month && settings.increment == 1) {
                    let start = DateTime {
                        date: one,
                        time: Time::default(),
                    };
                    let end = DateTime {
                        date: two,
                        time: Time::default(),
                    };
                    internal = iso::round_relative(
                        internal,
                        end.utc_epoch_ns(),
                        start,
                        None,
                        settings.largest,
                        settings.increment,
                        settings.smallest,
                        settings.mode,
                    )
                    .ok_or_else(|| self.temporal_range("The rounded duration is out of range"))?;
                }
                self.duration_value(internal, Unit::Day, since)
            }
            Method::Equals => {
                let (other, other_calendar) =
                    self.temporal_year_month(argument(0), Value::Undefined)?;
                Ok(Value::Bool(other == date && other_calendar == calendar))
            }
            Method::ToPlainDate => {
                let item = argument(0);
                if is_primitive(&item) {
                    return Err(self.type_error("toPlainDate() takes an object"));
                }
                let bag = self.read_bag(&item, calendar, Want::Date)?;
                let Some(day) = bag.day else {
                    return Err(self.type_error("toPlainDate() needs a day"));
                };
                let result = iso::regulate_date(
                    i64::from(date.year),
                    i64::from(date.month),
                    day,
                    Overflow::Constrain,
                );
                let result = self.checked_date(result)?;
                Ok(self.wrap_temporal(Temporal::Date(result, calendar)))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn month_day_method(
        &mut self,
        date: Date,
        calendar: Calendar,
        method: Method,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match method {
            Method::With => {
                let partial = self.with_bag(&argument(0), calendar, Want::MonthDay)?;
                let year_given = partial.year;
                let mut merged = self.merged(date, Time::default(), partial, calendar)?;
                merged.year = year_given;
                let options = self.options_object(argument(1))?;
                let overflow = self.overflow_option(&options)?;
                let result = self.bag_month_day(&merged, calendar, overflow)?;
                Ok(self.wrap_temporal(Temporal::MonthDay(result, calendar)))
            }
            Method::Equals => {
                let (other, other_calendar) =
                    self.temporal_month_day(argument(0), Value::Undefined)?;
                Ok(Value::Bool(other == date && other_calendar == calendar))
            }
            Method::ToPlainDate => {
                let item = argument(0);
                if is_primitive(&item) {
                    return Err(self.type_error("toPlainDate() takes an object"));
                }
                let bag = self.read_bag(&item, calendar, Want::YearMonth)?;
                let Some(year) = self.bag_year(&bag, calendar)? else {
                    return Err(self.type_error("toPlainDate() needs a year"));
                };
                let result = iso::regulate_date(
                    year,
                    i64::from(date.month),
                    i64::from(date.day),
                    Overflow::Constrain,
                );
                let result = self.checked_date(result)?;
                Ok(self.wrap_temporal(Temporal::Date(result, calendar)))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    fn temporal_string(&mut self, value: &Temporal, options: Value) -> Result<Value, ScriptError> {
        let options = self.options_object(options)?;
        let text = match value {
            Temporal::Duration(fields) => {
                let (precision, unit, increment, mode) = self.string_precision(&options, false)?;
                if precision == Precision::Auto {
                    duration_text(fields, precision)
                } else {
                    let largest = duration_largest(fields);
                    let internal = iso::internal_of(fields);
                    let time = round_time_duration(internal.time, increment, unit, mode);
                    let Some(rounded) = iso::fields_of(
                        Internal {
                            date: internal.date,
                            time,
                        },
                        iso::larger(largest, Unit::Second),
                    ) else {
                        return Err(self.temporal_range("The duration is out of range"));
                    };
                    duration_text(&rounded, precision)
                }
            }
            Temporal::Instant(nanoseconds) => {
                let (precision, unit, increment, mode) = self.string_precision(&options, true)?;
                let zone = match &options.0 {
                    Some(object) => match self.get_property(object, "timeZone")? {
                        Value::Undefined => None,
                        other => Some(self.temporal_zone(other)?),
                    },
                    None => None,
                };
                let rounded = iso::round_to_increment(
                    *nanoseconds,
                    i128::from(increment) * unit.nanoseconds(),
                    mode,
                );
                let rounded = self.checked_instant(Some(rounded))?;
                match zone {
                    None => {
                        let local =
                            iso::DateTime::from_epoch_ns(rounded).expect("an instant in range");
                        format!(
                            "{}T{}Z",
                            date_text(local.date),
                            time_text(local.time, precision)
                        )
                    }
                    Some(zone) => {
                        let local = iso::date_time_for(&zone, rounded);
                        let offset = iso::Zone::offset_ns(&zone, rounded);
                        let offset = iso::round_to_increment(
                            offset,
                            60 * iso::NS_PER_SECOND,
                            RoundingMode::HalfExpand,
                        );
                        format!(
                            "{}T{}{}",
                            date_text(local.date),
                            time_text(local.time, precision),
                            offset_text(offset, false)
                        )
                    }
                }
            }
            Temporal::Date(date, calendar) => {
                let show = self.calendar_name_option(&options)?;
                format!(
                    "{}{}",
                    date_text(*date),
                    calendar_annotation(*calendar, show)
                )
            }
            Temporal::Time(time) => {
                let (precision, unit, increment, mode) = self.string_precision(&options, true)?;
                let (_, rounded) = round_time(*time, increment, unit, mode);
                time_text(rounded, precision)
            }
            Temporal::DateTime(value, calendar) => {
                let show = self.calendar_name_option(&options)?;
                let (precision, unit, increment, mode) = self.string_precision(&options, true)?;
                let rounded = round_date_time(*value, increment, unit, mode);
                let rounded = self.checked_date_time(rounded)?;
                format!(
                    "{}T{}{}",
                    date_text(rounded.date),
                    time_text(rounded.time, precision),
                    calendar_annotation(*calendar, show)
                )
            }
            Temporal::Zoned(nanoseconds, zone, calendar) => {
                let show = self.calendar_name_option(&options)?;
                let (precision, unit, increment, mode) = self.string_precision(&options, true)?;
                let show_offset =
                    self.option_choice(&options, "offset", &["auto", "never"], "auto")?;
                let show_zone = self.option_choice(
                    &options,
                    "timeZoneName",
                    &["auto", "never", "critical"],
                    "auto",
                )?;
                let rounded = iso::round_to_increment(
                    *nanoseconds,
                    i128::from(increment) * unit.nanoseconds(),
                    mode,
                );
                let rounded = self.checked_instant(Some(rounded))?;
                let local = iso::date_time_for(zone, rounded);
                let mut text = format!(
                    "{}T{}",
                    date_text(local.date),
                    time_text(local.time, precision)
                );
                if show_offset == "auto" {
                    let offset = iso::Zone::offset_ns(zone, rounded);
                    let offset = iso::round_to_increment(
                        offset,
                        60 * iso::NS_PER_SECOND,
                        RoundingMode::HalfExpand,
                    );
                    text.push_str(&offset_text(offset, false));
                }
                match show_zone.as_str() {
                    "never" => {}
                    "critical" => text.push_str(&format!("[!{}]", zone.id())),
                    _ => text.push_str(&format!("[{}]", zone.id())),
                }
                text.push_str(&calendar_annotation(*calendar, show));
                text
            }
            Temporal::YearMonth(date, calendar) => {
                let show = self.calendar_name_option(&options)?;
                if *calendar == Calendar::Iso
                    && matches!(show, ShowCalendar::Auto | ShowCalendar::Never)
                {
                    format!("{}-{:02}", year_text(i64::from(date.year)), date.month)
                } else {
                    format!(
                        "{}{}",
                        date_text(*date),
                        calendar_annotation(*calendar, show)
                    )
                }
            }
            Temporal::MonthDay(date, calendar) => {
                let show = self.calendar_name_option(&options)?;
                if *calendar == Calendar::Iso
                    && matches!(show, ShowCalendar::Auto | ShowCalendar::Never)
                {
                    format!("{:02}-{:02}", date.month, date.day)
                } else {
                    format!(
                        "{}{}",
                        date_text(*date),
                        calendar_annotation(*calendar, show)
                    )
                }
            }
        };
        Ok(Value::String(text.into()))
    }

    // toLocaleString through Intl: dates and times by the date formatter, durations by the
    // duration formatter
    fn temporal_locale_string(
        &mut self,
        value: &Temporal,
        locales: Value,
        options: Value,
    ) -> Result<Value, ScriptError> {
        let text = |name: &str| Value::String(name.into());
        let numeric = || text("numeric");
        let (milliseconds, zone, fields): (f64, String, Vec<(&str, Value)>) = match value {
            Temporal::Duration(_) => {
                let intl = self.get_property(&Value::Window, "Intl")?;
                let constructor = self.get_property(&intl, "DurationFormat")?;
                let formatter = self.construct(constructor, vec![locales, options], None)?;
                let format = self.get_property(&formatter, "format")?;
                let this = self.wrap_temporal(Temporal::Duration(match value {
                    Temporal::Duration(fields) => *fields,
                    _ => unreachable!("matched above"),
                }));
                return self.call_function(format, formatter, vec![this]);
            }
            Temporal::Instant(nanoseconds) => (
                nanoseconds.div_euclid(1_000_000) as f64,
                self.host_zone().id(),
                vec![
                    ("year", numeric()),
                    ("month", numeric()),
                    ("day", numeric()),
                    ("hour", numeric()),
                    ("minute", text("2-digit")),
                    ("second", text("2-digit")),
                ],
            ),
            Temporal::Zoned(nanoseconds, zone, _) => (
                nanoseconds.div_euclid(1_000_000) as f64,
                zone.id(),
                vec![
                    ("year", numeric()),
                    ("month", numeric()),
                    ("day", numeric()),
                    ("hour", numeric()),
                    ("minute", text("2-digit")),
                    ("second", text("2-digit")),
                ],
            ),
            Temporal::Date(date, _) => (
                date.epoch_days() as f64 * 86_400_000.0,
                "UTC".to_string(),
                vec![
                    ("year", numeric()),
                    ("month", numeric()),
                    ("day", numeric()),
                ],
            ),
            Temporal::DateTime(value, _) => (
                (value.utc_epoch_ns().div_euclid(1_000_000)) as f64,
                "UTC".to_string(),
                vec![
                    ("year", numeric()),
                    ("month", numeric()),
                    ("day", numeric()),
                    ("hour", numeric()),
                    ("minute", text("2-digit")),
                    ("second", text("2-digit")),
                ],
            ),
            Temporal::Time(time) => (
                (time.nanoseconds() / 1_000_000) as f64,
                "UTC".to_string(),
                vec![
                    ("hour", numeric()),
                    ("minute", text("2-digit")),
                    ("second", text("2-digit")),
                ],
            ),
            Temporal::YearMonth(date, _) => (
                date.epoch_days() as f64 * 86_400_000.0,
                "UTC".to_string(),
                vec![("year", numeric()), ("month", numeric())],
            ),
            Temporal::MonthDay(date, _) => (
                date.epoch_days() as f64 * 86_400_000.0,
                "UTC".to_string(),
                vec![("month", numeric()), ("day", numeric())],
            ),
        };
        let given = !matches!(options, Value::Undefined);
        let mut entries: Vec<(String, Value)> = Vec::new();
        if given {
            for name in [
                "dateStyle",
                "timeStyle",
                "weekday",
                "year",
                "month",
                "day",
                "hour",
                "minute",
                "second",
                "hour12",
            ] {
                let found = self.get_property(&options, name)?;
                if !matches!(found, Value::Undefined) {
                    entries.push((name.to_string(), found));
                }
            }
        }
        if entries.is_empty() {
            entries.extend(
                fields
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value)),
            );
        }
        entries.push(("timeZone".to_string(), Value::String(zone.clone().into())));
        let object = self.object_from_entries(entries);
        let date_options = self.date_options(&locales, &object)?;
        let mut formatted = self.format_date_with(&date_options, &Value::Number(milliseconds))?;
        if let Temporal::Zoned(nanoseconds, found_zone, _) = value {
            let offset = iso::Zone::offset_ns(found_zone, *nanoseconds);
            formatted.push(' ');
            formatted.push_str(&zone_short_name(&zone, offset));
        }
        Ok(Value::String(formatted.into()))
    }
}

// the short zone name English gives a time: the US abbreviations CLDR keeps, else GMT and
// the offset
fn zone_short_name(zone: &str, offset: i128) -> String {
    let primary = super::super::tz::primary(zone).unwrap_or("");
    let names: Option<(&str, &str)> = match primary {
        "America/New_York"
        | "America/Detroit"
        | "America/Indiana/Indianapolis"
        | "America/Kentucky/Louisville"
        | "America/Toronto" => Some(("EST", "EDT")),
        "America/Chicago" | "America/Indiana/Knox" | "America/Menominee" | "America/Winnipeg" => {
            Some(("CST", "CDT"))
        }
        "America/Denver" | "America/Boise" | "America/Edmonton" => Some(("MST", "MDT")),
        "America/Phoenix" => Some(("MST", "MST")),
        "America/Los_Angeles" | "America/Vancouver" => Some(("PST", "PDT")),
        "America/Anchorage" | "America/Juneau" => Some(("AKST", "AKDT")),
        "Pacific/Honolulu" => Some(("HST", "HST")),
        "Etc/UTC" | "Etc/GMT" => return "UTC".to_string(),
        _ => None,
    };
    if let Some((standard, daylight)) = names
        && let Some((_, found)) = super::super::tz::lookup(primary)
    {
        // the standard offset is the smaller of the year's two
        let january = found.offset(0);
        let july = found.offset(181 * 86_400);
        let standard_offset = i128::from(january.min(july)) * iso::NS_PER_SECOND;
        return if offset > standard_offset {
            daylight
        } else {
            standard
        }
        .to_string();
    }
    let minutes = offset / (60 * iso::NS_PER_SECOND);
    if minutes == 0 {
        return "GMT".to_string();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let (hours, rest) = (minutes.abs() / 60, minutes.abs() % 60);
    if rest == 0 {
        format!("GMT{sign}{hours}")
    } else {
        format!("GMT{sign}{hours}:{rest:02}")
    }
}
