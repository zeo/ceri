// the Temporal namespace: Duration, Instant, PlainDate, PlainTime, PlainDateTime,
// ZonedDateTime, PlainYearMonth, PlainMonthDay and Temporal.Now, in the ISO 8601 and
// Gregorian calendars and every IANA time zone; iso.rs holds the arithmetic, parse.rs the
// string grammar and methods.rs what each type's methods do

mod iso;
mod methods;
mod parse;

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{Hint, is_primitive};
use super::realm::{Builder, Native};
use super::*;
use iso::{Date, DateTime, Disambiguation, Fields, Overflow, RoundingMode, Time, Unit};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Duration,
    Instant,
    Date,
    Time,
    DateTime,
    Zoned,
    YearMonth,
    MonthDay,
}

const KINDS: [(Kind, &str, u32); 8] = [
    (Kind::Duration, "Duration", 0),
    (Kind::Instant, "Instant", 1),
    (Kind::Date, "PlainDate", 3),
    (Kind::Time, "PlainTime", 0),
    (Kind::DateTime, "PlainDateTime", 3),
    (Kind::Zoned, "ZonedDateTime", 2),
    (Kind::YearMonth, "PlainYearMonth", 2),
    (Kind::MonthDay, "PlainMonthDay", 2),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Field {
    Duration(u8),
    Sign,
    Blank,
    EpochMilliseconds,
    EpochNanoseconds,
    CalendarId,
    TimeZoneId,
    Era,
    EraYear,
    Year,
    Month,
    MonthCode,
    Day,
    Time(u8),
    DayOfWeek,
    DayOfYear,
    WeekOfYear,
    YearOfWeek,
    DaysInWeek,
    DaysInMonth,
    DaysInYear,
    MonthsInYear,
    InLeapYear,
    HoursInDay,
    OffsetNanoseconds,
    Offset,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Method {
    Add,
    Subtract,
    With,
    Until,
    Since,
    Round,
    Total,
    Equals,
    ToString,
    ToJson,
    ToLocaleString,
    ValueOf,
    Negated,
    Abs,
    WithPlainTime,
    WithCalendar,
    WithTimeZone,
    ToPlainDate,
    ToPlainTime,
    ToPlainDateTime,
    ToZonedDateTime,
    ToZonedDateTimeIso,
    ToInstant,
    ToPlainYearMonth,
    ToPlainMonthDay,
    StartOfDay,
    GetTimeZoneTransition,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NowMethod {
    Instant,
    TimeZoneId,
    ZonedDateTime,
    PlainDateTime,
    PlainDate,
    PlainTime,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TemporalMember {
    Construct(Kind),
    From(Kind),
    Compare(Kind),
    FromEpoch(bool),
    Get(Kind, Field),
    Call(Kind, Method),
    Now(NowMethod),
    DateToInstant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Calendar {
    Iso,
    Gregory,
}

impl Calendar {
    fn id(self) -> &'static str {
        match self {
            Calendar::Iso => "iso8601",
            Calendar::Gregory => "gregory",
        }
    }

    fn named(name: &str) -> Option<Calendar> {
        match name.to_ascii_lowercase().as_str() {
            "iso8601" => Some(Calendar::Iso),
            "gregory" => Some(Calendar::Gregory),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(crate) enum TimeZone {
    Offset(i128),
    Named(&'static str, Rc<super::tz::Zone>),
}

impl TimeZone {
    fn id(&self) -> String {
        match self {
            TimeZone::Offset(offset) => offset_text(*offset, false),
            TimeZone::Named(name, _) => name.to_string(),
        }
    }

    // two zones are the same when their identifiers name one zone
    fn same(&self, other: &TimeZone) -> bool {
        match (self, other) {
            (TimeZone::Offset(a), TimeZone::Offset(b)) => a == b,
            (TimeZone::Named(a, _), TimeZone::Named(b, _)) => {
                a.eq_ignore_ascii_case(b) || super::tz::primary(a) == super::tz::primary(b)
            }
            _ => false,
        }
    }
}

impl iso::Zone for TimeZone {
    fn offset_ns(&self, epoch_ns: i128) -> i128 {
        match self {
            TimeZone::Offset(offset) => *offset,
            TimeZone::Named(_, zone) => {
                i128::from(zone.offset(epoch_ns.div_euclid(iso::NS_PER_SECOND) as i64))
                    * iso::NS_PER_SECOND
            }
        }
    }

    fn possible(&self, local: DateTime) -> Vec<i128> {
        let utc = local.utc_epoch_ns();
        let found: Vec<i128> = match self {
            TimeZone::Offset(offset) => vec![utc - offset],
            TimeZone::Named(_, zone) => {
                let seconds = utc.div_euclid(iso::NS_PER_SECOND);
                let fraction = utc.rem_euclid(iso::NS_PER_SECOND);
                zone.possible(seconds as i64)
                    .into_iter()
                    .map(|instant| i128::from(instant) * iso::NS_PER_SECOND + fraction)
                    .collect()
            }
        };
        found
            .into_iter()
            .filter(|&instant| iso::instant_in_range(instant))
            .collect()
    }
}

pub(crate) enum Temporal {
    Duration(Fields),
    Instant(i128),
    Date(Date, Calendar),
    Time(Time),
    DateTime(DateTime, Calendar),
    Zoned(i128, TimeZone, Calendar),
    YearMonth(Date, Calendar),
    MonthDay(Date, Calendar),
}

impl Temporal {
    fn kind(&self) -> Kind {
        match self {
            Temporal::Duration(_) => Kind::Duration,
            Temporal::Instant(_) => Kind::Instant,
            Temporal::Date(..) => Kind::Date,
            Temporal::Time(_) => Kind::Time,
            Temporal::DateTime(..) => Kind::DateTime,
            Temporal::Zoned(..) => Kind::Zoned,
            Temporal::YearMonth(..) => Kind::YearMonth,
            Temporal::MonthDay(..) => Kind::MonthDay,
        }
    }

    fn calendar(&self) -> Option<Calendar> {
        match self {
            Temporal::Date(_, calendar)
            | Temporal::DateTime(_, calendar)
            | Temporal::Zoned(_, _, calendar)
            | Temporal::YearMonth(_, calendar)
            | Temporal::MonthDay(_, calendar) => Some(*calendar),
            _ => None,
        }
    }
}

pub(super) struct TemporalInterfaces {
    prototypes: Vec<ObjectHandle>,
}

fn kind_name(kind: Kind) -> &'static str {
    KINDS
        .iter()
        .find(|(found, ..)| *found == kind)
        .expect("every kind is listed")
        .1
}

fn getters(kind: Kind) -> Vec<(&'static str, Field)> {
    let date = [
        ("calendarId", Field::CalendarId),
        ("era", Field::Era),
        ("eraYear", Field::EraYear),
        ("year", Field::Year),
        ("month", Field::Month),
        ("monthCode", Field::MonthCode),
        ("day", Field::Day),
    ];
    let time = [
        ("hour", Field::Time(0)),
        ("minute", Field::Time(1)),
        ("second", Field::Time(2)),
        ("millisecond", Field::Time(3)),
        ("microsecond", Field::Time(4)),
        ("nanosecond", Field::Time(5)),
    ];
    let calendar_facts = [
        ("dayOfWeek", Field::DayOfWeek),
        ("dayOfYear", Field::DayOfYear),
        ("weekOfYear", Field::WeekOfYear),
        ("yearOfWeek", Field::YearOfWeek),
        ("daysInWeek", Field::DaysInWeek),
        ("daysInMonth", Field::DaysInMonth),
        ("daysInYear", Field::DaysInYear),
        ("monthsInYear", Field::MonthsInYear),
        ("inLeapYear", Field::InLeapYear),
    ];
    match kind {
        Kind::Duration => {
            let mut list: Vec<(&str, Field)> = iso::UNITS
                .iter()
                .enumerate()
                .map(|(index, (_, _, plural))| (*plural, Field::Duration(index as u8)))
                .collect();
            list.push(("sign", Field::Sign));
            list.push(("blank", Field::Blank));
            list
        }
        Kind::Instant => vec![
            ("epochMilliseconds", Field::EpochMilliseconds),
            ("epochNanoseconds", Field::EpochNanoseconds),
        ],
        Kind::Date => date.into_iter().chain(calendar_facts).collect(),
        Kind::Time => time.to_vec(),
        Kind::DateTime => date.into_iter().chain(time).chain(calendar_facts).collect(),
        Kind::Zoned => {
            let mut list = vec![
                ("calendarId", Field::CalendarId),
                ("timeZoneId", Field::TimeZoneId),
            ];
            list.extend(date.into_iter().skip(1));
            list.extend(time);
            list.push(("epochMilliseconds", Field::EpochMilliseconds));
            list.push(("epochNanoseconds", Field::EpochNanoseconds));
            list.extend(calendar_facts.into_iter().take(4));
            list.push(("hoursInDay", Field::HoursInDay));
            list.extend(calendar_facts.into_iter().skip(4));
            list.push(("offsetNanoseconds", Field::OffsetNanoseconds));
            list.push(("offset", Field::Offset));
            list
        }
        Kind::YearMonth => {
            let mut list: Vec<(&str, Field)> = date.into_iter().take(6).collect();
            list.extend([
                ("daysInYear", Field::DaysInYear),
                ("daysInMonth", Field::DaysInMonth),
                ("monthsInYear", Field::MonthsInYear),
                ("inLeapYear", Field::InLeapYear),
            ]);
            list
        }
        Kind::MonthDay => vec![
            ("calendarId", Field::CalendarId),
            ("monthCode", Field::MonthCode),
            ("day", Field::Day),
        ],
    }
}

fn method_list(kind: Kind) -> Vec<(&'static str, Method, u32)> {
    use Method as M;
    let tail = [
        ("toString", M::ToString, 0),
        ("toLocaleString", M::ToLocaleString, 0),
        ("toJSON", M::ToJson, 0),
        ("valueOf", M::ValueOf, 0),
    ];
    let mut list = match kind {
        Kind::Duration => vec![
            ("with", M::With, 1),
            ("negated", M::Negated, 0),
            ("abs", M::Abs, 0),
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("round", M::Round, 1),
            ("total", M::Total, 1),
        ],
        Kind::Instant => vec![
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("round", M::Round, 1),
            ("equals", M::Equals, 1),
            ("toZonedDateTimeISO", M::ToZonedDateTimeIso, 1),
        ],
        Kind::Date => vec![
            ("toPlainYearMonth", M::ToPlainYearMonth, 0),
            ("toPlainMonthDay", M::ToPlainMonthDay, 0),
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("with", M::With, 1),
            ("withCalendar", M::WithCalendar, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("equals", M::Equals, 1),
            ("toPlainDateTime", M::ToPlainDateTime, 0),
            ("toZonedDateTime", M::ToZonedDateTime, 1),
        ],
        Kind::Time => vec![
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("with", M::With, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("round", M::Round, 1),
            ("equals", M::Equals, 1),
        ],
        Kind::DateTime => vec![
            ("with", M::With, 1),
            ("withPlainTime", M::WithPlainTime, 0),
            ("withCalendar", M::WithCalendar, 1),
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("round", M::Round, 1),
            ("equals", M::Equals, 1),
            ("toZonedDateTime", M::ToZonedDateTime, 1),
            ("toPlainDate", M::ToPlainDate, 0),
            ("toPlainTime", M::ToPlainTime, 0),
        ],
        Kind::Zoned => vec![
            ("with", M::With, 1),
            ("withPlainTime", M::WithPlainTime, 0),
            ("withTimeZone", M::WithTimeZone, 1),
            ("withCalendar", M::WithCalendar, 1),
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("round", M::Round, 1),
            ("equals", M::Equals, 1),
            ("startOfDay", M::StartOfDay, 0),
            ("getTimeZoneTransition", M::GetTimeZoneTransition, 1),
            ("toInstant", M::ToInstant, 0),
            ("toPlainDate", M::ToPlainDate, 0),
            ("toPlainTime", M::ToPlainTime, 0),
            ("toPlainDateTime", M::ToPlainDateTime, 0),
        ],
        Kind::YearMonth => vec![
            ("with", M::With, 1),
            ("add", M::Add, 1),
            ("subtract", M::Subtract, 1),
            ("until", M::Until, 1),
            ("since", M::Since, 1),
            ("equals", M::Equals, 1),
            ("toPlainDate", M::ToPlainDate, 1),
        ],
        Kind::MonthDay => vec![
            ("with", M::With, 1),
            ("equals", M::Equals, 1),
            ("toPlainDate", M::ToPlainDate, 1),
        ],
    };
    list.extend(tail);
    list
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    date_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> TemporalInterfaces {
    use TemporalMember as T;
    let plain = || {
        new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let method = |target: &ObjectHandle, name: &str, member: TemporalMember, length: u32| {
        let function = build.function(Native::Temporal(member), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    };
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
    };
    let temporal = plain();
    tag(&temporal, "Temporal");
    let mut prototypes = Vec::new();
    for (kind, name, length) in KINDS {
        let prototype = plain();
        for (getter, field) in getters(kind) {
            let get = build.function(
                Native::Temporal(T::Get(kind, field)),
                &format!("get {getter}"),
                0,
            );
            prototype.borrow_mut().define(
                getter.to_string(),
                Property {
                    slot: Slot::accessor(Some(Value::Object(get)), None),
                    enumerable: false,
                    writable: false,
                    configurable: true,
                },
            );
        }
        for (name, member, length) in method_list(kind) {
            method(&prototype, name, T::Call(kind, member), length);
        }
        tag(&prototype, &format!("Temporal.{name}"));
        let constructor = build.constructor(
            Native::Temporal(T::Construct(kind)),
            name,
            length,
            &prototype,
        );
        method(&constructor, "from", T::From(kind), 1);
        if kind != Kind::MonthDay {
            method(&constructor, "compare", T::Compare(kind), 2);
        }
        if kind == Kind::Instant {
            method(
                &constructor,
                "fromEpochMilliseconds",
                T::FromEpoch(false),
                1,
            );
            method(&constructor, "fromEpochNanoseconds", T::FromEpoch(true), 1);
        }
        temporal.borrow_mut().define(
            name.to_string(),
            Property::hidden(Value::Object(constructor)),
        );
        prototypes.push(prototype);
    }
    let now = plain();
    tag(&now, "Temporal.Now");
    for (name, member) in [
        ("instant", NowMethod::Instant),
        ("timeZoneId", NowMethod::TimeZoneId),
        ("zonedDateTimeISO", NowMethod::ZonedDateTime),
        ("plainDateTimeISO", NowMethod::PlainDateTime),
        ("plainDateISO", NowMethod::PlainDate),
        ("plainTimeISO", NowMethod::PlainTime),
    ] {
        method(&now, name, T::Now(member), 0);
    }
    temporal
        .borrow_mut()
        .define("Now".to_string(), Property::hidden(Value::Object(now)));
    method(date_prototype, "toTemporalInstant", T::DateToInstant, 0);
    globals.push(("Temporal", Value::Object(temporal)));
    TemporalInterfaces { prototypes }
}

// a year as ISO 8601 writes it: four digits, or a sign and six outside 0 to 9999
fn year_text(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    }
}

fn date_text(date: Date) -> String {
    format!(
        "{}-{:02}-{:02}",
        year_text(i64::from(date.year)),
        date.month,
        date.day
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Precision {
    Auto,
    Minute,
    Digits(u8),
}

fn fraction_text(nanoseconds: i64, precision: Precision) -> String {
    let digits = format!("{nanoseconds:09}");
    match precision {
        Precision::Auto => {
            let trimmed = digits.trim_end_matches('0');
            if trimmed.is_empty() {
                String::new()
            } else {
                format!(".{trimmed}")
            }
        }
        Precision::Digits(0) | Precision::Minute => String::new(),
        Precision::Digits(count) => format!(".{}", &digits[..usize::from(count)]),
    }
}

fn time_text(time: Time, precision: Precision) -> String {
    if precision == Precision::Minute {
        return format!("{:02}:{:02}", time.hour, time.minute);
    }
    let fraction = i64::from(time.millisecond) * 1_000_000
        + i64::from(time.microsecond) * 1000
        + i64::from(time.nanosecond);
    format!(
        "{:02}:{:02}:{:02}{}",
        time.hour,
        time.minute,
        time.second,
        fraction_text(fraction, precision)
    )
}

// an offset as ±HH:MM, with seconds and a fraction when it has them and they are wanted
fn offset_text(offset: i128, full: bool) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let magnitude = offset.abs();
    let seconds_total = magnitude / iso::NS_PER_SECOND;
    let fraction = (magnitude % iso::NS_PER_SECOND) as i64;
    let (hours, minutes, seconds) = (
        seconds_total / 3600,
        seconds_total / 60 % 60,
        seconds_total % 60,
    );
    if full && (seconds != 0 || fraction != 0) {
        return format!(
            "{sign}{hours:02}:{minutes:02}:{seconds:02}{}",
            fraction_text(fraction, Precision::Auto)
        );
    }
    format!("{sign}{hours:02}:{minutes:02}")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShowCalendar {
    Auto,
    Always,
    Never,
    Critical,
}

fn calendar_annotation(calendar: Calendar, show: ShowCalendar) -> String {
    match show {
        ShowCalendar::Never => String::new(),
        ShowCalendar::Auto if calendar == Calendar::Iso => String::new(),
        ShowCalendar::Critical => format!("[!u-ca={}]", calendar.id()),
        _ => format!("[u-ca={}]", calendar.id()),
    }
}

// a duration as ISO 8601 writes it
fn duration_text(fields: &Fields, precision: Precision) -> String {
    let sign = iso::fields_sign(fields);
    let mut date = String::new();
    for (index, letter) in ["Y", "M", "W", "D"].iter().enumerate() {
        if fields[index] != 0.0 {
            date.push_str(&format!("{}{letter}", integer_text(fields[index].abs())));
        }
    }
    let mut time = String::new();
    if fields[4] != 0.0 {
        time.push_str(&format!("{}H", integer_text(fields[4].abs())));
    }
    if fields[5] != 0.0 {
        time.push_str(&format!("{}M", integer_text(fields[5].abs())));
    }
    let seconds = fields[6].abs() as i128 * iso::NS_PER_SECOND
        + fields[7].abs() as i128 * 1_000_000
        + fields[8].abs() as i128 * 1000
        + fields[9].abs() as i128;
    let zero_above = fields[..6].iter().all(|value| *value == 0.0);
    if seconds != 0 || zero_above || precision != Precision::Auto {
        time.push_str(&format!(
            "{}{}S",
            seconds / iso::NS_PER_SECOND,
            fraction_text((seconds % iso::NS_PER_SECOND) as i64, precision)
        ));
    }
    let mut text = String::new();
    if sign < 0 {
        text.push('-');
    }
    text.push('P');
    text.push_str(&date);
    if !time.is_empty() {
        text.push('T');
        text.push_str(&time);
    }
    text
}

fn integer_text(value: f64) -> String {
    format!("{value:.0}")
}

// the options a method reads, as an object or nothing
pub(super) struct Options(Option<Value>);

impl ScriptRuntime {
    fn temporal_of(value: &Value) -> Option<Rc<Temporal>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Temporal(found) => Some(found.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn wrap_temporal(&self, value: Temporal) -> Value {
        let index = KINDS
            .iter()
            .position(|(kind, ..)| *kind == value.kind())
            .expect("every kind is listed");
        let prototype = self.intrinsics.temporal.prototypes[index].clone();
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Temporal(Rc::new(value)),
        )))
    }

    fn temporal_range(&mut self, message: &str) -> ScriptError {
        self.range_error(message.to_string())
    }

    fn options_object(&mut self, value: Value) -> Result<Options, ScriptError> {
        match value {
            Value::Undefined => Ok(Options(None)),
            value if !is_primitive(&value) => Ok(Options(Some(value))),
            _ => Err(self.type_error("Options must be an object or undefined")),
        }
    }

    fn option_string(
        &mut self,
        options: &Options,
        name: &str,
    ) -> Result<Option<String>, ScriptError> {
        let Some(object) = &options.0 else {
            return Ok(None);
        };
        match self.get_property(object, name)? {
            Value::Undefined => Ok(None),
            value => Ok(Some(self.coerce_string(value)?)),
        }
    }

    fn option_choice(
        &mut self,
        options: &Options,
        name: &str,
        allowed: &[&str],
        fallback: &'static str,
    ) -> Result<String, ScriptError> {
        match self.option_string(options, name)? {
            None => Ok(fallback.to_string()),
            Some(value) if allowed.contains(&value.as_str()) => Ok(value),
            Some(value) => {
                Err(self.range_error(format!("{value} is not a valid value for {name}")))
            }
        }
    }

    fn overflow_option(&mut self, options: &Options) -> Result<Overflow, ScriptError> {
        Ok(
            match self
                .option_choice(options, "overflow", &["constrain", "reject"], "constrain")?
                .as_str()
            {
                "reject" => Overflow::Reject,
                _ => Overflow::Constrain,
            },
        )
    }

    fn disambiguation_option(&mut self, options: &Options) -> Result<Disambiguation, ScriptError> {
        Ok(
            match self
                .option_choice(
                    options,
                    "disambiguation",
                    &["compatible", "earlier", "later", "reject"],
                    "compatible",
                )?
                .as_str()
            {
                "earlier" => Disambiguation::Earlier,
                "later" => Disambiguation::Later,
                "reject" => Disambiguation::Reject,
                _ => Disambiguation::Compatible,
            },
        )
    }

    fn rounding_mode_option(
        &mut self,
        options: &Options,
        fallback: RoundingMode,
    ) -> Result<RoundingMode, ScriptError> {
        let names: Vec<&str> = iso::ROUNDING_MODES.iter().map(|(_, name)| *name).collect();
        let fallback_name = iso::ROUNDING_MODES
            .iter()
            .find(|(mode, _)| *mode == fallback)
            .expect("a listed mode")
            .1;
        let chosen = self.option_choice(options, "roundingMode", &names, fallback_name)?;
        Ok(iso::ROUNDING_MODES
            .iter()
            .find(|(_, name)| *name == chosen)
            .expect("a listed mode")
            .0)
    }

    fn increment_option(&mut self, options: &Options) -> Result<i64, ScriptError> {
        let Some(object) = &options.0 else {
            return Ok(1);
        };
        let value = self.get_property(object, "roundingIncrement")?;
        if matches!(value, Value::Undefined) {
            return Ok(1);
        }
        let number = self.coerce_number(value)?;
        if !number.is_finite() {
            return Err(self.temporal_range("roundingIncrement must be finite"));
        }
        let increment = number.trunc();
        if !(1.0..=1e9).contains(&increment) {
            return Err(self.temporal_range("roundingIncrement is out of range"));
        }
        Ok(increment as i64)
    }

    // a unit option: none when absent, "auto" as none for largestUnit
    fn unit_option(
        &mut self,
        options: &Options,
        name: &str,
        auto: bool,
    ) -> Result<Option<Unit>, ScriptError> {
        match self.option_string(options, name)? {
            None => Ok(None),
            Some(value) if auto && value == "auto" => Ok(None),
            Some(value) => match Unit::named(&value) {
                Some(unit) => Ok(Some(unit)),
                None => Err(self.range_error(format!("{value} is not a valid value for {name}"))),
            },
        }
    }

    fn fractional_digits_option(&mut self, options: &Options) -> Result<Option<u8>, ScriptError> {
        let Some(object) = &options.0 else {
            return Ok(None);
        };
        let value = self.get_property(object, "fractionalSecondDigits")?;
        match value {
            Value::Undefined => Ok(None),
            Value::Number(number) => {
                let floored = number.floor();
                if !number.is_finite() || !(0.0..=9.0).contains(&floored) {
                    return Err(self.temporal_range("fractionalSecondDigits is out of range"));
                }
                Ok(Some(floored as u8))
            }
            other => {
                let text = self.coerce_string(other)?;
                if text == "auto" {
                    Ok(None)
                } else {
                    Err(self.range_error(format!(
                        "{text} is not a valid value for fractionalSecondDigits"
                    )))
                }
            }
        }
    }

    fn calendar_name_option(&mut self, options: &Options) -> Result<ShowCalendar, ScriptError> {
        Ok(
            match self
                .option_choice(
                    options,
                    "calendarName",
                    &["auto", "always", "never", "critical"],
                    "auto",
                )?
                .as_str()
            {
                "always" => ShowCalendar::Always,
                "never" => ShowCalendar::Never,
                "critical" => ShowCalendar::Critical,
                _ => ShowCalendar::Auto,
            },
        )
    }

    // ToSecondsStringPrecisionRecord with the rounding toString applies
    fn string_precision(
        &mut self,
        options: &Options,
        allow_minute: bool,
    ) -> Result<(Precision, Unit, i64, RoundingMode), ScriptError> {
        let digits = self.fractional_digits_option(options)?;
        let mode = self.rounding_mode_option(options, RoundingMode::Trunc)?;
        let smallest = self.unit_option(options, "smallestUnit", false)?;
        match smallest {
            Some(Unit::Minute) if allow_minute => Ok((Precision::Minute, Unit::Minute, 1, mode)),
            Some(Unit::Second) => Ok((Precision::Digits(0), Unit::Second, 1, mode)),
            Some(Unit::Millisecond) => Ok((Precision::Digits(3), Unit::Millisecond, 1, mode)),
            Some(Unit::Microsecond) => Ok((Precision::Digits(6), Unit::Microsecond, 1, mode)),
            Some(Unit::Nanosecond) => Ok((Precision::Digits(9), Unit::Nanosecond, 1, mode)),
            Some(_) => Err(self.temporal_range("smallestUnit is not allowed here")),
            None => Ok(match digits {
                None => (Precision::Auto, Unit::Nanosecond, 1, mode),
                Some(0) => (Precision::Digits(0), Unit::Second, 1, mode),
                Some(count) => {
                    let unit = match count {
                        1..=3 => Unit::Millisecond,
                        4..=6 => Unit::Microsecond,
                        _ => Unit::Nanosecond,
                    };
                    let increment = 10_i64.pow(u32::from((3 - count % 3) % 3));
                    (Precision::Digits(count), unit, increment, mode)
                }
            }),
        }
    }

    // ToTemporalTimeZoneIdentifier
    fn temporal_zone(&mut self, value: Value) -> Result<TimeZone, ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Temporal::Zoned(_, zone, _) = &*found
        {
            return Ok(zone.clone());
        }
        let Value::String(text) = value else {
            return Err(self.type_error("A time zone must be a string"));
        };
        let text = text.to_string();
        if let Some(zone) = zone_identifier(&text) {
            return Ok(zone);
        }
        if let Some(parsed) = parse::date_time(&text).or_else(|| parse::time(&text)) {
            if let Some(annotation) = parsed.zone {
                if let Some(zone) = zone_identifier(&annotation) {
                    return Ok(zone);
                }
            } else if parsed.utc {
                return Ok(zone_identifier("UTC").expect("UTC is in the database"));
            } else if let Some((offset, _)) = parsed.offset {
                return Ok(TimeZone::Offset(offset));
            }
        }
        Err(self.range_error(format!("{text} is not a valid time zone")))
    }

    // ToTemporalCalendarIdentifier
    fn temporal_calendar(&mut self, value: Value) -> Result<Calendar, ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Some(calendar) = found.calendar()
        {
            return Ok(calendar);
        }
        let Value::String(text) = value else {
            return Err(self.type_error("A calendar must be a string"));
        };
        let text = text.to_string();
        if let Some(calendar) = Calendar::named(&text) {
            return Ok(calendar);
        }
        let parsed = parse::date_time(&text)
            .or_else(|| parse::time(&text))
            .or_else(|| parse::year_month(&text))
            .or_else(|| parse::month_day(&text));
        match parsed {
            Some(parsed) => self.calendar_of(parsed.calendar.as_deref()),
            None => Err(self.range_error(format!("{text} is not a valid calendar"))),
        }
    }

    fn calendar_of(&mut self, name: Option<&str>) -> Result<Calendar, ScriptError> {
        match name {
            None => Ok(Calendar::Iso),
            Some(name) => Calendar::named(name)
                .ok_or_else(|| self.range_error(format!("{name} is not a supported calendar"))),
        }
    }

    // ToTemporalDuration
    fn temporal_duration(&mut self, value: Value) -> Result<Fields, ScriptError> {
        if let Some(found) = Self::temporal_of(&value)
            && let Temporal::Duration(fields) = &*found
        {
            return Ok(*fields);
        }
        match value {
            Value::String(text) => {
                let fields = parse::duration(&text)
                    .ok_or_else(|| self.range_error(format!("{text} is not a valid duration")))?;
                self.checked_fields(fields)
            }
            value if !is_primitive(&value) => {
                let fields = self.duration_like(&value, None)?;
                self.checked_fields(fields)
            }
            _ => Err(self.type_error("A duration must be a string or an object")),
        }
    }

    fn checked_fields(&mut self, fields: Fields) -> Result<Fields, ScriptError> {
        if iso::valid_fields(&fields) {
            Ok(fields.map(|value| if value == 0.0 { 0.0 } else { value }))
        } else {
            Err(self.temporal_range("The duration is not valid"))
        }
    }

    // ToTemporalPartialDurationRecord, over a base when with() merges into one
    fn duration_like(
        &mut self,
        value: &Value,
        base: Option<&Fields>,
    ) -> Result<Fields, ScriptError> {
        let mut fields = base.copied().unwrap_or([0.0; 10]);
        let mut any = false;
        let mut names: Vec<(usize, &str)> = iso::UNITS
            .iter()
            .enumerate()
            .map(|(index, (_, _, plural))| (index, *plural))
            .collect();
        names.sort_by_key(|(_, name)| *name);
        for (index, name) in names {
            let field = self.get_property(value, name)?;
            if matches!(field, Value::Undefined) {
                continue;
            }
            any = true;
            let number = self.coerce_number(field)?;
            if !number.is_finite() || number.fract() != 0.0 {
                return Err(self.temporal_range("Duration fields must be integers"));
            }
            fields[index] = number;
        }
        if !any {
            return Err(self.type_error("A duration needs at least one field"));
        }
        Ok(fields)
    }

    // ToIntegerWithTruncation
    fn integer_field(&mut self, value: Value) -> Result<i64, ScriptError> {
        let number = self.coerce_number(value)?;
        if !number.is_finite() {
            return Err(self.temporal_range("A field must be a finite number"));
        }
        let truncated = number.trunc();
        if truncated.abs() > 1e15 {
            return Err(self.temporal_range("A field is out of range"));
        }
        Ok(truncated as i64)
    }

    fn arguments_integer(&mut self, value: Value, fallback: i64) -> Result<i64, ScriptError> {
        match value {
            Value::Undefined => Ok(fallback),
            value => self.integer_field(value),
        }
    }

    fn checked_date(&mut self, date: Option<Date>) -> Result<Date, ScriptError> {
        match date.filter(|date| iso::date_in_range(*date)) {
            Some(date) => Ok(date),
            None => Err(self.temporal_range("The date is out of range")),
        }
    }

    fn checked_date_time(&mut self, value: Option<DateTime>) -> Result<DateTime, ScriptError> {
        match value.filter(|value| iso::date_time_in_range(*value)) {
            Some(value) => Ok(value),
            None => Err(self.temporal_range("The date and time are out of range")),
        }
    }

    fn checked_instant(&mut self, value: Option<i128>) -> Result<i128, ScriptError> {
        match value.filter(|value| iso::instant_in_range(*value)) {
            Some(value) => Ok(value),
            None => Err(self.temporal_range("The instant is out of range")),
        }
    }

    pub(super) fn construct_temporal(
        &mut self,
        kind: Kind,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if matches!(new_target, Value::Undefined) {
            return Err(self.type_error(format!(
                "Constructor Temporal.{} requires 'new'",
                kind_name(kind)
            )));
        }
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let value = match kind {
            Kind::Duration => {
                let mut fields = [0.0; 10];
                for (index, field) in fields.iter_mut().enumerate() {
                    *field = match argument(index) {
                        Value::Undefined => 0.0,
                        value => {
                            let number = self.coerce_number(value)?;
                            if !number.is_finite() || number.fract() != 0.0 {
                                return Err(self.temporal_range("Duration fields must be integers"));
                            }
                            number
                        }
                    };
                }
                Temporal::Duration(self.checked_fields(fields)?)
            }
            Kind::Instant => {
                let value = self.coerce_bigint(argument(0))?;
                let nanoseconds = value
                    .to_i128()
                    .filter(|value| iso::instant_in_range(*value));
                Temporal::Instant(self.checked_instant(nanoseconds)?)
            }
            Kind::Date => {
                let year = self.integer_field(argument(0))?;
                let month = self.integer_field(argument(1))?;
                let day = self.integer_field(argument(2))?;
                let calendar = self.constructor_calendar(argument(3))?;
                let date = iso::regulate_date(year, month, day, Overflow::Reject);
                Temporal::Date(self.checked_date(date)?, calendar)
            }
            Kind::Time => {
                let mut fields = [0; 6];
                for (index, field) in fields.iter_mut().enumerate() {
                    *field = self.arguments_integer(argument(index), 0)?;
                }
                match iso::regulate_time(fields, Overflow::Reject) {
                    Some(time) => Temporal::Time(time),
                    None => return Err(self.temporal_range("The time is out of range")),
                }
            }
            Kind::DateTime => {
                let year = self.integer_field(argument(0))?;
                let month = self.integer_field(argument(1))?;
                let day = self.integer_field(argument(2))?;
                let mut fields = [0; 6];
                for (index, field) in fields.iter_mut().enumerate() {
                    *field = self.arguments_integer(argument(index + 3), 0)?;
                }
                let calendar = self.constructor_calendar(argument(9))?;
                let date = iso::regulate_date(year, month, day, Overflow::Reject);
                let Some(time) = iso::regulate_time(fields, Overflow::Reject) else {
                    return Err(self.temporal_range("The time is out of range"));
                };
                let date = self.checked_date(date)?;
                let value = self.checked_date_time(Some(DateTime { date, time }))?;
                Temporal::DateTime(value, calendar)
            }
            Kind::Zoned => {
                let value = self.coerce_bigint(argument(0))?;
                let nanoseconds = value.to_i128();
                let nanoseconds = self.checked_instant(nanoseconds)?;
                let Value::String(zone) = argument(1) else {
                    return Err(self.type_error("A time zone must be a string"));
                };
                let Some(zone) = zone_identifier(&zone) else {
                    return Err(self.range_error(format!("{zone} is not a valid time zone")));
                };
                let calendar = self.constructor_calendar(argument(2))?;
                Temporal::Zoned(nanoseconds, zone, calendar)
            }
            Kind::YearMonth => {
                let year = self.integer_field(argument(0))?;
                let month = self.integer_field(argument(1))?;
                let calendar = self.constructor_calendar(argument(2))?;
                let day = self.arguments_integer(argument(3), 1)?;
                let date = iso::regulate_date(year, month, day, Overflow::Reject);
                match date.filter(|date| {
                    iso::year_month_in_range(i64::from(date.year), i64::from(date.month))
                }) {
                    Some(date) => Temporal::YearMonth(date, calendar),
                    None => return Err(self.temporal_range("The year and month are out of range")),
                }
            }
            Kind::MonthDay => {
                let month = self.integer_field(argument(0))?;
                let day = self.integer_field(argument(1))?;
                let calendar = self.constructor_calendar(argument(2))?;
                let year = self.arguments_integer(argument(3), 1972)?;
                let date = iso::regulate_date(year, month, day, Overflow::Reject);
                Temporal::MonthDay(self.checked_date(date)?, calendar)
            }
        };
        let object = self.wrap_temporal(value);
        let prototype = self.prototype_from_constructor(&new_target)?;
        if let Value::Object(object) = &object {
            object.borrow_mut().set_prototype(Some(prototype));
        }
        Ok(object)
    }

    fn constructor_calendar(&mut self, value: Value) -> Result<Calendar, ScriptError> {
        match value {
            Value::Undefined => Ok(Calendar::Iso),
            Value::String(text) => Calendar::named(&text)
                .ok_or_else(|| self.range_error(format!("{text} is not a supported calendar"))),
            _ => Err(self.type_error("A calendar must be a string")),
        }
    }

    pub(super) fn call_temporal(
        &mut self,
        member: TemporalMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match member {
            TemporalMember::Construct(kind) => {
                self.construct_temporal(kind, arguments, Value::Undefined)
            }
            TemporalMember::From(kind) => self.temporal_from(kind, argument(0), argument(1)),
            TemporalMember::Compare(kind) => {
                self.temporal_compare(kind, argument(0), argument(1), argument(2))
            }
            TemporalMember::FromEpoch(nanoseconds) => {
                let value = if nanoseconds {
                    self.coerce_bigint(argument(0))?.to_i128()
                } else {
                    let number = self.coerce_number(argument(0))?;
                    if !number.is_finite() || number.fract() != 0.0 {
                        return Err(self.temporal_range("epochMilliseconds must be an integer"));
                    }
                    Some(number as i128 * 1_000_000)
                };
                let value = self.checked_instant(value)?;
                Ok(self.wrap_temporal(Temporal::Instant(value)))
            }
            TemporalMember::Now(method) => self.temporal_now(method, argument(0)),
            TemporalMember::DateToInstant => {
                let time = match &this {
                    Value::Object(object) => match object.borrow().class {
                        ObjectClass::Date(time) => Some(time),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(time) = time else {
                    return Err(self.type_error("this is not a Date object."));
                };
                if time.is_nan() {
                    return Err(self.temporal_range("Invalid time value"));
                }
                Ok(self.wrap_temporal(Temporal::Instant(time as i128 * 1_000_000)))
            }
            TemporalMember::Get(kind, field) => {
                let value = self.this_temporal(&this, kind)?;
                self.temporal_field(&value, field)
            }
            TemporalMember::Call(kind, method) => {
                let value = self.this_temporal(&this, kind)?;
                self.temporal_method(value, method, arguments)
            }
        }
    }

    fn this_temporal(&mut self, this: &Value, kind: Kind) -> Result<Rc<Temporal>, ScriptError> {
        match Self::temporal_of(this) {
            Some(found) if found.kind() == kind => Ok(found),
            _ => Err(self.type_error(format!("this is not a Temporal.{}", kind_name(kind)))),
        }
    }

    fn host_zone(&self) -> TimeZone {
        let now = self.now_milliseconds();
        if let Some(name) = self.time_zone.name(now)
            && let Some(zone) = zone_identifier(&name)
        {
            return zone;
        }
        TimeZone::Offset(i128::from(self.time_zone.offset_minutes(now)) * 60 * iso::NS_PER_SECOND)
    }

    fn temporal_now(&mut self, method: NowMethod, zone: Value) -> Result<Value, ScriptError> {
        let now = (self.now_milliseconds() * 1_000_000.0) as i128;
        if method == NowMethod::Instant {
            return Ok(self.wrap_temporal(Temporal::Instant(now)));
        }
        if method == NowMethod::TimeZoneId {
            return Ok(Value::String(self.host_zone().id().into()));
        }
        let zone = match zone {
            Value::Undefined => self.host_zone(),
            other => self.temporal_zone(other)?,
        };
        let local = iso::date_time_for(&zone, now);
        Ok(self.wrap_temporal(match method {
            NowMethod::ZonedDateTime => Temporal::Zoned(now, zone, Calendar::Iso),
            NowMethod::PlainDateTime => Temporal::DateTime(local, Calendar::Iso),
            NowMethod::PlainDate => Temporal::Date(local.date, Calendar::Iso),
            _ => Temporal::Time(local.time),
        }))
    }
}

// a time zone identifier on its own: a ±HH:MM offset or a name in the database
fn zone_identifier(text: &str) -> Option<TimeZone> {
    if text.starts_with(['+', '-']) {
        let (offset, minutes_only) = parse::offset(text)?;
        return minutes_only.then_some(TimeZone::Offset(offset));
    }
    let (name, zone) = super::tz::lookup(text)?;
    Some(TimeZone::Named(name, zone))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> String {
        let document = Rc::new(RefCell::new(
            crate::dom::Document::parse(r#"<output id="out"></output>"#)
                .expect("document should parse"),
        ));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(&format!(
                "document.getElementById('out').textContent = (() => {{ {source} }})();"
            ))
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let out = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default()
    }

    #[test]
    fn plain_types_parse_add_differ_and_print() {
        assert_eq!(
            run(r#"
                const { Duration, PlainDate, PlainTime, PlainDateTime, PlainYearMonth, PlainMonthDay } = Temporal;
                return [
                    Duration.from("P1Y2M3W4DT5H6M7.008009010S"), Duration.from("PT1.5H"), Duration.from({ minutes: 130 }).round({ largestUnit: "hours" }),
                    Duration.from({ days: 45 }).round({ largestUnit: "months", relativeTo: "2024-01-15" }), Duration.from("P1M").total({ unit: "days", relativeTo: "2024-02-01" }),
                    PlainDate.from("2024-01-31").add({ months: 1 }), PlainDate.from("2020-02-29").until("2024-02-28", { largestUnit: "years" }),
                    PlainDate.from("2024-07-04").since("2023-01-01", { largestUnit: "years", smallestUnit: "months", roundingMode: "halfExpand" }),
                    PlainTime.from("23:59:59.9").round("second"), PlainTime.from("10:00").until("08:30"),
                    PlainDateTime.from("2024-01-31T23:00").add({ months: 1, hours: 2 }), PlainDateTime.from("2024-01-03T01:00").until("2024-01-01T23:00", { largestUnit: "days" }),
                    PlainYearMonth.from("2023-05").until("2024-07"), PlainMonthDay.from({ month: 2, day: 29 }), PlainDate.from("+275760-09-13"),
                    PlainDate.from("2024-12-30").weekOfYear + "/" + PlainDate.from("2024-12-30").yearOfWeek,
                ].map(String).join(" ");
            "#),
            "P1Y2M3W4DT5H6M7.00800901S PT1H30M PT2H10M P1M14D 29 2024-02-29 P3Y11M30D P1Y6M 00:00:00 -PT1H30M 2024-03-01T01:00:00 -P1DT2H P1Y2M 02-29 +275760-09-13 1/2025"
        );
    }

    #[test]
    fn zoned_times_follow_their_zones() {
        assert_eq!(
            run(r#"
                const { Instant, PlainDateTime, ZonedDateTime } = Temporal;
                const z = ZonedDateTime.from("2024-11-03T12:00[America/New_York]");
                const attempt = (make) => { try { return String(make()); } catch (error) { return error.name; } };
                return [
                    Instant.from("2024-07-04T12:30:00.5+02:00"), Instant.from("2024-07-04T12:30Z").toZonedDateTimeISO("America/New_York"),
                    z.offset, z.hoursInDay, PlainDateTime.from("2024-03-10T02:30").toZonedDateTime("America/New_York"),
                    PlainDateTime.from("2024-11-03T01:30").toZonedDateTime("America/New_York", { disambiguation: "later" }),
                    attempt(() => ZonedDateTime.from("2024-07-04T12:30-05:00[America/New_York]")),
                    ZonedDateTime.from("2024-07-04T12:30-05:00[America/New_York]", { offset: "prefer" }),
                    ZonedDateTime.from("2024-03-09T12:00[America/New_York]").add({ days: 1 }),
                    ZonedDateTime.from("2024-03-09T12:00[America/New_York]").until("2024-03-10T12:00[America/New_York]"),
                    ZonedDateTime.from("2024-07-04T12:00[America/New_York]").getTimeZoneTransition("next"),
                    ZonedDateTime.from("1880-01-01T00:00[America/New_York]").offset, ZonedDateTime.from("2024-07-04T12:00[Asia/Calcutta]").timeZoneId,
                    ZonedDateTime.from("2011-12-29T12:00[Pacific/Apia]").add({ days: 1 }),
                ].map(String).join(" ");
            "#),
            "2024-07-04T10:30:00.5Z 2024-07-04T08:30:00-04:00[America/New_York] -05:00 25 2024-03-10T03:30:00-04:00[America/New_York] 2024-11-03T01:30:00-05:00[America/New_York] RangeError 2024-07-04T12:30:00-04:00[America/New_York] 2024-03-10T12:00:00-04:00[America/New_York] PT23H 2024-11-03T01:00:00-05:00[America/New_York] -04:56:02 Asia/Calcutta 2011-12-31T12:00:00+14:00[Pacific/Apia]"
        );
    }
}
