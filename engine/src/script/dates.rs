// Date: time values in milliseconds since the epoch, the spec's day and time arithmetic,
// ISO and RFC 2822 style parsing, and local time through an embedder time zone seam

use super::object::{JsObject, ObjectClass, new_object};
use super::*;

const MS_PER_DAY: f64 = 86_400_000.0;
const MAX_TIME: f64 = 8.64e15;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// the local time zone, supplied by the embedder; without one, local time is UTC
pub trait TimeZone {
    // minutes to add to UTC for local time at the given instant
    fn offset_minutes(&self, utc_milliseconds: f64) -> i32;
    // the zone's display name at the given instant, such as "Central European Standard Time"
    fn name(&self, _utc_milliseconds: f64) -> Option<String> {
        None
    }
}

pub(super) struct Utc;

impl TimeZone for Utc {
    fn offset_minutes(&self, _utc_milliseconds: f64) -> i32 {
        0
    }

    fn name(&self, _utc_milliseconds: f64) -> Option<String> {
        Some("Coordinated Universal Time".to_string())
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DateMethod {
    Constructor,
    Now,
    Parse,
    Utc,
    GetTime,
    Get(Field, bool),
    Set(Field, bool),
    GetDay(bool),
    GetTimezoneOffset,
    SetTime,
    GetYear,
    SetYear,
    ToIsoString,
    ToJson,
    ToString,
    ToDateString,
    ToTimeString,
    ToUtcString,
    ToLocaleString,
    ToLocaleDateString,
    ToLocaleTimeString,
    ToPrimitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    FullYear,
    Month,
    Date,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
}

pub(super) const DATE_METHODS: &[(&str, DateMethod, u32)] = &[
    ("getTime", DateMethod::GetTime, 0),
    ("valueOf", DateMethod::GetTime, 0),
    ("getFullYear", DateMethod::Get(Field::FullYear, false), 0),
    ("getUTCFullYear", DateMethod::Get(Field::FullYear, true), 0),
    ("getMonth", DateMethod::Get(Field::Month, false), 0),
    ("getUTCMonth", DateMethod::Get(Field::Month, true), 0),
    ("getDate", DateMethod::Get(Field::Date, false), 0),
    ("getUTCDate", DateMethod::Get(Field::Date, true), 0),
    ("getDay", DateMethod::GetDay(false), 0),
    ("getUTCDay", DateMethod::GetDay(true), 0),
    ("getHours", DateMethod::Get(Field::Hours, false), 0),
    ("getUTCHours", DateMethod::Get(Field::Hours, true), 0),
    ("getMinutes", DateMethod::Get(Field::Minutes, false), 0),
    ("getUTCMinutes", DateMethod::Get(Field::Minutes, true), 0),
    ("getSeconds", DateMethod::Get(Field::Seconds, false), 0),
    ("getUTCSeconds", DateMethod::Get(Field::Seconds, true), 0),
    (
        "getMilliseconds",
        DateMethod::Get(Field::Milliseconds, false),
        0,
    ),
    (
        "getUTCMilliseconds",
        DateMethod::Get(Field::Milliseconds, true),
        0,
    ),
    ("getTimezoneOffset", DateMethod::GetTimezoneOffset, 0),
    ("getYear", DateMethod::GetYear, 0),
    ("setTime", DateMethod::SetTime, 1),
    ("setFullYear", DateMethod::Set(Field::FullYear, false), 3),
    ("setUTCFullYear", DateMethod::Set(Field::FullYear, true), 3),
    ("setMonth", DateMethod::Set(Field::Month, false), 2),
    ("setUTCMonth", DateMethod::Set(Field::Month, true), 2),
    ("setDate", DateMethod::Set(Field::Date, false), 1),
    ("setUTCDate", DateMethod::Set(Field::Date, true), 1),
    ("setHours", DateMethod::Set(Field::Hours, false), 4),
    ("setUTCHours", DateMethod::Set(Field::Hours, true), 4),
    ("setMinutes", DateMethod::Set(Field::Minutes, false), 3),
    ("setUTCMinutes", DateMethod::Set(Field::Minutes, true), 3),
    ("setSeconds", DateMethod::Set(Field::Seconds, false), 2),
    ("setUTCSeconds", DateMethod::Set(Field::Seconds, true), 2),
    (
        "setMilliseconds",
        DateMethod::Set(Field::Milliseconds, false),
        1,
    ),
    (
        "setUTCMilliseconds",
        DateMethod::Set(Field::Milliseconds, true),
        1,
    ),
    ("setYear", DateMethod::SetYear, 1),
    ("toISOString", DateMethod::ToIsoString, 0),
    ("toJSON", DateMethod::ToJson, 1),
    ("toString", DateMethod::ToString, 0),
    ("toDateString", DateMethod::ToDateString, 0),
    ("toTimeString", DateMethod::ToTimeString, 0),
    ("toUTCString", DateMethod::ToUtcString, 0),
    ("toGMTString", DateMethod::ToUtcString, 0),
    ("toLocaleString", DateMethod::ToLocaleString, 0),
    ("toLocaleDateString", DateMethod::ToLocaleDateString, 0),
    ("toLocaleTimeString", DateMethod::ToLocaleTimeString, 0),
];

// ---- the spec's date arithmetic ----

fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}

fn time_within_day(t: f64) -> f64 {
    t.rem_euclid(MS_PER_DAY)
}

// days since the epoch for a proleptic Gregorian date, month zero based and unbounded
pub(super) fn days_from_civil(year: f64, month: f64, date: f64) -> f64 {
    let year = year + (month / 12.0).floor();
    let month = month.rem_euclid(12.0) + 1.0;
    let y = if month <= 2.0 { year - 1.0 } else { year };
    let era = (y / 400.0).floor();
    let yoe = y - era * 400.0;
    let mp = (month + 9.0) % 12.0;
    let doy = ((153.0 * mp + 2.0) / 5.0).floor();
    let doe = yoe * 365.0 + (yoe / 4.0).floor() - (yoe / 100.0).floor() + doy;
    era * 146_097.0 + doe - 719_468.0 + (date - 1.0)
}

// (year, zero-based month, date) for a day number since the epoch
pub(super) fn civil_from_days(days: f64) -> (f64, f64, f64) {
    let z = days + 719_468.0;
    let era = (z / 146_097.0).floor();
    let doe = z - era * 146_097.0;
    let yoe = ((doe - (doe / 1460.0).floor() + (doe / 36_524.0).floor()
        - (doe / 146_096.0).floor())
        / 365.0)
        .floor();
    let y = yoe + era * 400.0;
    let doy = doe - (365.0 * yoe + (yoe / 4.0).floor() - (yoe / 100.0).floor());
    let mp = ((5.0 * doy + 2.0) / 153.0).floor();
    let date = doy - ((153.0 * mp + 2.0) / 5.0).floor() + 1.0;
    let month = if mp < 10.0 { mp + 3.0 } else { mp - 9.0 };
    let year = if month <= 2.0 { y + 1.0 } else { y };
    (year, month - 1.0, date)
}

fn make_time(hour: f64, minute: f64, second: f64, ms: f64) -> f64 {
    if !(hour.is_finite() && minute.is_finite() && second.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    hour.trunc() * 3_600_000.0 + minute.trunc() * 60_000.0 + second.trunc() * 1000.0 + ms.trunc()
}

fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    days_from_civil(year.trunc(), month.trunc(), date.trunc())
}

fn make_date(day: f64, time: f64) -> f64 {
    if !(day.is_finite() && time.is_finite()) {
        return f64::NAN;
    }
    day * MS_PER_DAY + time
}

pub(super) fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > MAX_TIME {
        return f64::NAN;
    }
    time.trunc() + 0.0
}

struct Parts {
    year: f64,
    month: f64,
    date: f64,
    hours: f64,
    minutes: f64,
    seconds: f64,
    ms: f64,
    weekday: usize,
}

fn parts(t: f64) -> Parts {
    let (year, month, date) = civil_from_days(day(t));
    let within = time_within_day(t);
    Parts {
        year,
        month,
        date,
        hours: (within / 3_600_000.0).floor(),
        minutes: (within / 60_000.0).floor() % 60.0,
        seconds: (within / 1000.0).floor() % 60.0,
        ms: within % 1000.0,
        weekday: (day(t) + 4.0).rem_euclid(7.0) as usize,
    }
}

fn year_text(year: f64) -> String {
    if year >= 0.0 {
        format!("{:04}", year as i64)
    } else {
        format!("-{:06}", (-year) as i64)
    }
}

fn offset_text(offset_minutes: i32, colon: bool) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let absolute = offset_minutes.abs();
    if colon {
        format!("{sign}{:02}:{:02}", absolute / 60, absolute % 60)
    } else {
        format!("{sign}{:02}{:02}", absolute / 60, absolute % 60)
    }
}

pub(super) fn iso_string(t: f64) -> String {
    let p = parts(t);
    let year = if (0.0..=9999.0).contains(&p.year) {
        format!("{:04}", p.year as i64)
    } else if p.year < 0.0 {
        format!("-{:06}", (-p.year) as i64)
    } else {
        format!("+{:06}", p.year as i64)
    };
    format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        p.month as i64 + 1,
        p.date as i64,
        p.hours as i64,
        p.minutes as i64,
        p.seconds as i64,
        p.ms as i64
    )
}

fn utc_string(t: f64) -> String {
    let p = parts(t);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[p.weekday],
        p.date as i64,
        MONTHS[p.month as usize],
        year_text(p.year),
        p.hours as i64,
        p.minutes as i64,
        p.seconds as i64
    )
}

fn date_text(local: f64) -> String {
    let p = parts(local);
    format!(
        "{} {} {:02} {}",
        WEEKDAYS[p.weekday],
        MONTHS[p.month as usize],
        p.date as i64,
        year_text(p.year)
    )
}

fn time_text(local: f64, offset: i32, zone: Option<String>) -> String {
    let p = parts(local);
    let zone = zone.map(|name| format!(" ({name})")).unwrap_or_default();
    format!(
        "{:02}:{:02}:{:02} GMT{}{zone}",
        p.hours as i64,
        p.minutes as i64,
        p.seconds as i64,
        offset_text(offset, false)
    )
}

// the en-US formats toLocaleString uses: 1/15/2024, 3:04:05 PM
fn locale_date(local: f64) -> String {
    let p = parts(local);
    format!("{}/{}/{}", p.month as i64 + 1, p.date as i64, p.year as i64)
}

fn locale_time(local: f64) -> String {
    let p = parts(local);
    let hour = p.hours as i64 % 12;
    let hour = if hour == 0 { 12 } else { hour };
    let meridiem = if p.hours < 12.0 { "AM" } else { "PM" };
    format!(
        "{hour}:{:02}:{:02} {meridiem}",
        p.minutes as i64, p.seconds as i64
    )
}

// ---- parsing ----

fn digits(text: &[u8], position: &mut usize, count: usize) -> Option<f64> {
    let slice = text.get(*position..*position + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    *position += count;
    std::str::from_utf8(slice).ok()?.parse().ok()
}

// the ISO 8601 subset the spec defines; Ok(None) means the text is not in that format
fn parse_iso(text: &str, local_offset: &dyn Fn(f64) -> f64) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut position = 0;
    let year = match bytes.first()? {
        b'+' | b'-' => {
            let negative = bytes[0] == b'-';
            position = 1;
            let value = digits(bytes, &mut position, 6)?;
            if negative && value == 0.0 {
                return None;
            }
            if negative { -value } else { value }
        }
        _ => digits(bytes, &mut position, 4)?,
    };
    let mut month = 1.0;
    let mut date = 1.0;
    if bytes.get(position) == Some(&b'-') {
        position += 1;
        month = digits(bytes, &mut position, 2)?;
        if bytes.get(position) == Some(&b'-') {
            position += 1;
            date = digits(bytes, &mut position, 2)?;
        }
    }
    let (mut hours, mut minutes, mut seconds, mut ms) = (0.0, 0.0, 0.0, 0.0);
    let mut has_time = false;
    let mut offset: Option<f64> = None;
    if matches!(bytes.get(position), Some(b'T' | b't' | b' ')) && position + 1 < bytes.len() {
        position += 1;
        has_time = true;
        hours = digits(bytes, &mut position, 2)?;
        if bytes.get(position) != Some(&b':') {
            return None;
        }
        position += 1;
        minutes = digits(bytes, &mut position, 2)?;
        if bytes.get(position) == Some(&b':') {
            position += 1;
            seconds = digits(bytes, &mut position, 2)?;
            if matches!(bytes.get(position), Some(b'.' | b',')) {
                position += 1;
                let start = position;
                while bytes.get(position).is_some_and(u8::is_ascii_digit) {
                    position += 1;
                }
                if position == start {
                    return None;
                }
                let fraction = &text[start..position.min(start + 3)];
                ms = fraction.parse::<f64>().ok()? * 10f64.powi(3 - fraction.len() as i32);
            }
        }
        match bytes.get(position) {
            Some(b'Z' | b'z') => {
                offset = Some(0.0);
                position += 1;
            }
            Some(b'+' | b'-') => {
                let sign = if bytes[position] == b'-' { -1.0 } else { 1.0 };
                position += 1;
                let offset_hours = digits(bytes, &mut position, 2)?;
                if bytes.get(position) == Some(&b':') {
                    position += 1;
                }
                let offset_minutes = digits(bytes, &mut position, 2)?;
                offset = Some(sign * (offset_hours * 60.0 + offset_minutes));
            }
            _ => {}
        }
    }
    if position != bytes.len() {
        return None;
    }
    if !(1.0..=12.0).contains(&month)
        || !(1.0..=31.0).contains(&date)
        || hours > 24.0
        || minutes > 59.0
        || seconds > 59.0
    {
        return Some(f64::NAN);
    }
    if hours == 24.0 && (minutes != 0.0 || seconds != 0.0 || ms != 0.0) {
        return Some(f64::NAN);
    }
    let local = make_date(
        make_day(year, month - 1.0, date),
        make_time(hours, minutes, seconds, ms),
    );
    // date-only forms are UTC; date-time forms without an offset are local time
    let utc = match offset {
        Some(offset) => local - offset * 60_000.0,
        None if has_time => local - local_offset(local),
        None => local,
    };
    Some(time_clip(utc))
}

fn month_index(word: &str) -> Option<f64> {
    let lower = word.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|name| lower.starts_with(&name.to_ascii_lowercase()))
        .filter(|_| lower.len() >= 3)
        .map(|index| index as f64)
}

// the looser formats other engines accept: "Jan 15 2024 10:00:00 GMT+0100", "15 January
// 2024", "January 15, 2024 10:00 PM", "1/15/2024" and the toString and toUTCString outputs
fn parse_legacy(text: &str, local_offset: &dyn Fn(f64) -> f64) -> f64 {
    let mut year: Option<f64> = None;
    let mut month: Option<f64> = None;
    let mut date: Option<f64> = None;
    let mut time: Option<(f64, f64, f64)> = None;
    let mut offset: Option<f64> = None;
    let mut meridiem: Option<bool> = None;
    let cleaned: String = text
        .chars()
        .map(|character| if character == ',' { ' ' } else { character })
        .collect();
    // drop a parenthesized zone name such as "(Central European Standard Time)"
    let cleaned = match cleaned.find('(') {
        Some(start) => cleaned[..start].to_string(),
        None => cleaned,
    };
    for token in cleaned.split_whitespace() {
        let upper = token.to_ascii_uppercase();
        if upper == "AM" || upper == "PM" {
            meridiem = Some(upper == "PM");
        } else if upper == "Z" || upper == "UTC" || upper == "GMT" || upper == "UT" {
            offset = Some(0.0);
        } else if let Some(rest) = upper
            .strip_prefix("GMT")
            .or_else(|| upper.strip_prefix("UTC"))
        {
            let sign = if rest.starts_with('-') { -1.0 } else { 1.0 };
            let numbers: String = rest.chars().filter(char::is_ascii_digit).collect();
            if numbers.len() == 4 {
                let value: f64 = numbers.parse().unwrap_or(0.0);
                offset = Some(sign * ((value / 100.0).floor() * 60.0 + value % 100.0));
            } else if !numbers.is_empty() {
                offset = Some(sign * numbers.parse::<f64>().unwrap_or(0.0) * 60.0);
            }
        } else if (token.starts_with('+') || token.starts_with('-'))
            && token.len() == 5
            && token[1..].bytes().all(|byte| byte.is_ascii_digit())
        {
            let sign = if token.starts_with('-') { -1.0 } else { 1.0 };
            let value: f64 = token[1..].parse().unwrap_or(0.0);
            offset = Some(sign * ((value / 100.0).floor() * 60.0 + value % 100.0));
        } else if token.contains(':') {
            let mut fields = token.split(':').map(|field| field.parse::<f64>().ok());
            let hours = fields.next().flatten();
            let minutes = fields.next().flatten();
            let seconds = fields.next().flatten().unwrap_or(0.0);
            match (hours, minutes) {
                (Some(hours), Some(minutes)) => time = Some((hours, minutes, seconds)),
                _ => return f64::NAN,
            }
        } else if token.contains('/') {
            let fields: Vec<Option<f64>> = token
                .split('/')
                .map(|field| field.parse::<f64>().ok())
                .collect();
            match fields.as_slice() {
                [Some(m), Some(d), Some(y)] => {
                    month = Some(m - 1.0);
                    date = Some(*d);
                    year = Some(if *y < 100.0 { y + 1900.0 } else { *y });
                }
                [Some(m), Some(d)] => {
                    month = Some(m - 1.0);
                    date = Some(*d);
                }
                _ => return f64::NAN,
            }
        } else if let Some(index) = month_index(token) {
            month = Some(index);
        } else if token.bytes().all(|byte| byte.is_ascii_digit()) {
            let value: f64 = token.parse().unwrap_or(f64::NAN);
            if date.is_none() && value <= 31.0 && token.len() <= 2 {
                date = Some(value);
            } else if year.is_none() {
                year = Some(if token.len() <= 2 {
                    value + if value < 50.0 { 2000.0 } else { 1900.0 }
                } else {
                    value
                });
            } else {
                return f64::NAN;
            }
        } else if WEEKDAYS
            .iter()
            .any(|name| upper.starts_with(&name.to_ascii_uppercase()))
        {
            continue;
        } else {
            return f64::NAN;
        }
    }
    let (Some(year), Some(month)) = (year, month) else {
        return f64::NAN;
    };
    let (mut hours, minutes, seconds) = time.unwrap_or((0.0, 0.0, 0.0));
    match meridiem {
        Some(true) if hours < 12.0 => hours += 12.0,
        Some(false) if hours == 12.0 => hours = 0.0,
        _ => {}
    }
    let local = make_date(
        make_day(year, month, date.unwrap_or(1.0)),
        make_time(hours, minutes, seconds, 0.0),
    );
    let utc = match offset {
        Some(offset) => local - offset * 60_000.0,
        None => local - local_offset(local),
    };
    time_clip(utc)
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    // the current time: the wall clock at startup plus the page clock's progress
    pub(super) fn now_milliseconds(&self) -> f64 {
        self.time_origin + self.clock as f64
    }

    fn offset_ms(&self, utc: f64) -> f64 {
        f64::from(self.time_zone.offset_minutes(utc)) * 60_000.0
    }

    // LocalTime(t)
    fn local(&self, utc: f64) -> f64 {
        utc + self.offset_ms(utc)
    }

    // UTC(t) for a local time value; the offset is taken at the guessed instant
    fn to_utc(&self, local: f64) -> f64 {
        if !local.is_finite() {
            return f64::NAN;
        }
        let guess = local - self.offset_ms(local);
        local - self.offset_ms(guess)
    }

    pub(super) fn parse_date(&self, text: &str) -> f64 {
        let text = text.trim();
        let offset = |local: f64| {
            let guess = local - self.offset_ms(local);
            self.offset_ms(guess)
        };
        parse_iso(text, &offset).unwrap_or_else(|| parse_legacy(text, &offset))
    }

    pub(super) fn date_value(
        &mut self,
        time: f64,
        prototype: Option<object::ObjectHandle>,
    ) -> Value {
        let prototype = prototype.unwrap_or_else(|| self.intrinsics.date_prototype.clone());
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Date(time_clip(time)),
        )))
    }

    fn this_time(&mut self, this: &Value) -> Result<f64, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::Date(time) = object.borrow().class
        {
            return Ok(time);
        }
        Err(self.type_error("this is not a Date object."))
    }

    fn set_this_time(&mut self, this: &Value, time: f64) -> f64 {
        let time = time_clip(time);
        if let Value::Object(object) = this {
            object.borrow_mut().class = ObjectClass::Date(time);
        }
        time
    }

    fn date_string(&self, time: f64) -> String {
        if time.is_nan() {
            return "Invalid Date".to_string();
        }
        let offset = self.time_zone.offset_minutes(time);
        let local = self.local(time);
        format!(
            "{} {}",
            date_text(local),
            time_text(local, offset, self.time_zone.name(time))
        )
    }

    fn numbers(&mut self, arguments: &[Value], count: usize) -> Result<Vec<f64>, ScriptError> {
        let mut numbers = Vec::with_capacity(count);
        for value in arguments.iter().take(count) {
            numbers.push(self.coerce_number(value.clone())?);
        }
        Ok(numbers)
    }

    pub(super) fn call_date(
        &mut self,
        method: DateMethod,
        this: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use DateMethod as M;
        match method {
            M::Constructor => {
                let Some(new_target) = new_target else {
                    let now = self.now_milliseconds();
                    return Ok(Value::String(self.date_string(time_clip(now)).into()));
                };
                let prototype = match &new_target {
                    Value::Object(_) => Some(self.prototype_from_constructor(&new_target)?),
                    _ => None,
                };
                let time = match arguments.len() {
                    0 => self.now_milliseconds(),
                    1 => {
                        let value = arguments[0].clone();
                        let existing = match &value {
                            Value::Object(object) => match object.borrow().class {
                                ObjectClass::Date(time) => Some(time),
                                _ => None,
                            },
                            _ => None,
                        };
                        match existing {
                            Some(time) => time,
                            None => {
                                match self.coerce_primitive(value, operations::Hint::Default)? {
                                    Value::String(text) => self.parse_date(&text),
                                    other => self.coerce_number(other)?,
                                }
                            }
                        }
                    }
                    _ => {
                        let numbers = self.numbers(&arguments, 7)?;
                        let year = numbers[0];
                        let year = if (0.0..=99.0).contains(&year.trunc()) {
                            1900.0 + year.trunc()
                        } else {
                            year
                        };
                        let get = |index: usize, default: f64| {
                            numbers.get(index).copied().unwrap_or(default)
                        };
                        let local = make_date(
                            make_day(year, get(1, 0.0), get(2, 1.0)),
                            make_time(get(3, 0.0), get(4, 0.0), get(5, 0.0), get(6, 0.0)),
                        );
                        self.to_utc(local)
                    }
                };
                Ok(self.date_value(time, prototype))
            }
            M::Now => Ok(Value::Number(time_clip(self.now_milliseconds()))),
            M::Parse => {
                let text = self.coerce_string(argument(&arguments, 0))?;
                Ok(Value::Number(self.parse_date(&text)))
            }
            M::Utc => {
                let numbers = self.numbers(&arguments, 7)?;
                let year = numbers.first().copied().unwrap_or(f64::NAN);
                let year = if (0.0..=99.0).contains(&year.trunc()) {
                    1900.0 + year.trunc()
                } else {
                    year
                };
                let get =
                    |index: usize, default: f64| numbers.get(index).copied().unwrap_or(default);
                Ok(Value::Number(time_clip(make_date(
                    make_day(year, get(1, 0.0), get(2, 1.0)),
                    make_time(get(3, 0.0), get(4, 0.0), get(5, 0.0), get(6, 0.0)),
                ))))
            }
            M::GetTime => Ok(Value::Number(self.this_time(&this)?)),
            M::Get(field, utc) => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                let p = parts(if utc { time } else { self.local(time) });
                Ok(Value::Number(match field {
                    Field::FullYear => p.year,
                    Field::Month => p.month,
                    Field::Date => p.date,
                    Field::Hours => p.hours,
                    Field::Minutes => p.minutes,
                    Field::Seconds => p.seconds,
                    Field::Milliseconds => p.ms,
                }))
            }
            M::GetDay(utc) => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                Ok(Value::Number(
                    parts(if utc { time } else { self.local(time) }).weekday as f64,
                ))
            }
            M::GetYear => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                Ok(Value::Number(parts(self.local(time)).year - 1900.0))
            }
            M::GetTimezoneOffset => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                Ok(Value::Number(f64::from(
                    -self.time_zone.offset_minutes(time),
                )))
            }
            M::SetTime => {
                self.this_time(&this)?;
                let time = self.coerce_number(argument(&arguments, 0))?;
                Ok(Value::Number(self.set_this_time(&this, time)))
            }
            M::SetYear => {
                let time = self.this_time(&this)?;
                let year = self.coerce_number(argument(&arguments, 0))?;
                let year = if (0.0..=99.0).contains(&year.trunc()) {
                    1900.0 + year.trunc()
                } else {
                    year
                };
                let base = if time.is_nan() { 0.0 } else { self.local(time) };
                let p = parts(base);
                let local = make_date(make_day(year, p.month, p.date), time_within_day(base));
                let utc = self.to_utc(local);
                Ok(Value::Number(self.set_this_time(&this, utc)))
            }
            M::Set(field, utc) => {
                let time = self.this_time(&this)?;
                let numbers = self.numbers(&arguments, 4)?;
                let first = numbers.first().copied().unwrap_or(f64::NAN);
                // setFullYear on an invalid date starts from +0
                let base = if time.is_nan() {
                    if field == Field::FullYear {
                        0.0
                    } else {
                        return Ok(Value::Number(f64::NAN));
                    }
                } else if utc {
                    time
                } else {
                    self.local(time)
                };
                let p = parts(base);
                let get =
                    |index: usize, default: f64| numbers.get(index).copied().unwrap_or(default);
                let (year, month, date, hours, minutes, seconds, ms) = match field {
                    Field::FullYear => (
                        first,
                        get(1, p.month),
                        get(2, p.date),
                        p.hours,
                        p.minutes,
                        p.seconds,
                        p.ms,
                    ),
                    Field::Month => (
                        p.year,
                        first,
                        get(1, p.date),
                        p.hours,
                        p.minutes,
                        p.seconds,
                        p.ms,
                    ),
                    Field::Date => (p.year, p.month, first, p.hours, p.minutes, p.seconds, p.ms),
                    Field::Hours => (
                        p.year,
                        p.month,
                        p.date,
                        first,
                        get(1, p.minutes),
                        get(2, p.seconds),
                        get(3, p.ms),
                    ),
                    Field::Minutes => (
                        p.year,
                        p.month,
                        p.date,
                        p.hours,
                        first,
                        get(1, p.seconds),
                        get(2, p.ms),
                    ),
                    Field::Seconds => (
                        p.year,
                        p.month,
                        p.date,
                        p.hours,
                        p.minutes,
                        first,
                        get(1, p.ms),
                    ),
                    Field::Milliseconds => (
                        p.year, p.month, p.date, p.hours, p.minutes, p.seconds, first,
                    ),
                };
                let composed = make_date(
                    make_day(year, month, date),
                    make_time(hours, minutes, seconds, ms),
                );
                let new_time = if utc { composed } else { self.to_utc(composed) };
                Ok(Value::Number(self.set_this_time(&this, new_time)))
            }
            M::ToIsoString => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Err(self.range_error("Invalid time value"));
                }
                Ok(Value::String(iso_string(time).into()))
            }
            M::ToJson => {
                let primitive = self.coerce_primitive(this.clone(), operations::Hint::Number)?;
                if let Value::Number(number) = primitive
                    && !number.is_finite()
                {
                    return Ok(Value::Null);
                }
                let method = self.get_property(&this, "toISOString")?;
                self.call_function(method, this, Vec::new())
            }
            M::ToString => {
                let time = self.this_time(&this)?;
                Ok(Value::String(self.date_string(time).into()))
            }
            M::ToDateString
            | M::ToTimeString
            | M::ToUtcString
            | M::ToLocaleString
            | M::ToLocaleDateString
            | M::ToLocaleTimeString => {
                let time = self.this_time(&this)?;
                if time.is_nan() {
                    return Ok(Value::String("Invalid Date".into()));
                }
                if matches!(
                    method,
                    M::ToLocaleString | M::ToLocaleDateString | M::ToLocaleTimeString
                ) && !arguments.is_empty()
                {
                    let mut options =
                        self.date_options(&argument(&arguments, 0), &argument(&arguments, 1))?;
                    if options.is_empty() {
                        options.fill_defaults(
                            matches!(method, M::ToLocaleString | M::ToLocaleDateString),
                            matches!(method, M::ToLocaleString | M::ToLocaleTimeString),
                        );
                    }
                    return Ok(Value::String(
                        self.format_date_with(&options, &this)?.into(),
                    ));
                }
                let local = self.local(time);
                let text = match method {
                    M::ToDateString => date_text(local),
                    M::ToTimeString => time_text(
                        local,
                        self.time_zone.offset_minutes(time),
                        self.time_zone.name(time),
                    ),
                    M::ToUtcString => utc_string(time),
                    M::ToLocaleDateString => locale_date(local),
                    M::ToLocaleTimeString => locale_time(local),
                    _ => format!("{}, {}", locale_date(local), locale_time(local)),
                };
                Ok(Value::String(text.into()))
            }
            M::ToPrimitive => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("Date.prototype[Symbol.toPrimitive] called on non-object")
                    );
                }
                let hint = match argument(&arguments, 0) {
                    Value::String(hint) => hint.to_string(),
                    _ => return Err(self.type_error("Invalid hint")),
                };
                let order: [&str; 2] = match hint.as_str() {
                    "string" | "default" => ["toString", "valueOf"],
                    "number" => ["valueOf", "toString"],
                    _ => return Err(self.type_error(format!("Invalid hint: {hint}"))),
                };
                for name in order {
                    let method = self.get_property(&this, name)?;
                    if super::operations::is_callable(&method) {
                        let result = self.call_function(method, this.clone(), Vec::new())?;
                        if super::operations::is_primitive(&result) {
                            return Ok(result);
                        }
                    }
                }
                Err(self.type_error("Cannot convert object to primitive value"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip_across_eras() {
        for (year, month, date) in [
            (1970.0, 0.0, 1.0),
            (2000.0, 1.0, 29.0),
            (1900.0, 2.0, 1.0),
            (-1.0, 11.0, 31.0),
            (275_760.0, 8.0, 13.0),
        ] {
            let days = days_from_civil(year, month, date);
            assert_eq!(civil_from_days(days), (year, month, date));
        }
        assert_eq!(days_from_civil(1970.0, 0.0, 1.0), 0.0);
        assert_eq!(
            days_from_civil(2024.0, 12.0, 1.0),
            days_from_civil(2025.0, 0.0, 1.0)
        );
    }

    #[test]
    fn iso_and_legacy_formats_parse() {
        let utc = |_: f64| 0.0;
        assert_eq!(
            parse_iso("2024-01-15T10:30:00Z", &utc),
            Some(1_705_314_600_000.0)
        );
        assert_eq!(parse_iso("2024-01-15", &utc), Some(1_705_276_800_000.0));
        assert_eq!(
            parse_iso("2024-01-15T10:30:00.5+01:00", &utc),
            Some(1_705_311_000_500.0)
        );
        assert_eq!(
            parse_iso("+002024-01-15T00:00:00Z", &utc),
            Some(1_705_276_800_000.0)
        );
        assert!(parse_iso("2024-13-01", &utc).unwrap().is_nan());
        assert_eq!(parse_iso("Jan 15 2024", &utc), None);
        assert_eq!(
            parse_legacy(
                "Mon Jan 15 2024 10:30:00 GMT+0000 (Coordinated Universal Time)",
                &utc
            ),
            1_705_314_600_000.0
        );
        assert_eq!(
            parse_legacy("Mon, 15 Jan 2024 10:30:00 GMT", &utc),
            1_705_314_600_000.0
        );
        assert_eq!(
            parse_legacy("January 15, 2024 10:30 PM", &utc),
            1_705_357_800_000.0
        );
        assert_eq!(parse_legacy("1/15/2024", &utc), 1_705_276_800_000.0);
        assert!(parse_legacy("not a date", &utc).is_nan());
        assert_eq!(iso_string(1_705_314_600_123.0), "2024-01-15T10:30:00.123Z");
        assert_eq!(
            utc_string(1_705_314_600_000.0),
            "Mon, 15 Jan 2024 10:30:00 GMT"
        );
    }
}
