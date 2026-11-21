// Intl.DateTimeFormat's patterns: the options become a CLDR skeleton the way V8 builds one,
// the skeleton's best pattern comes from the tables tools/date_patterns.py takes from ICU, and
// the pattern formats a wall clock time into typed parts

use super::DateOptions;
use super::date_patterns_data::{
    DATE_LOCALES, DATE_SKELETONS, PATTERN_ZONES, STRINGS, TIME_SKELETONS,
};

const WEEKDAY_STYLES: [&str; 4] = ["", "narrow", "short", "long"];
const YEAR_STYLES: [&str; 3] = ["", "2-digit", "numeric"];
const MONTH_STYLES: [&str; 6] = ["", "2-digit", "numeric", "narrow", "short", "long"];
const DAY_STYLES: [&str; 3] = ["", "2-digit", "numeric"];
const ZONE_STYLES: [&str; 7] = [
    "",
    "short",
    "long",
    "shortOffset",
    "longOffset",
    "shortGeneric",
    "longGeneric",
];

// where each kind of string starts in a locale's row
const TIMES: usize = DATE_SKELETONS;
const GLUE: usize = TIMES + TIME_SKELETONS;
const DEFAULT_HOUR: usize = GLUE + 4;
const DEFAULT_TIMES: usize = DEFAULT_HOUR + 1;
const DATE_STYLES: usize = DEFAULT_TIMES + 3;
const TIME_STYLES: usize = DATE_STYLES + 4;
const MONTHS: usize = TIME_STYLES + 4;
const WEEKDAYS: usize = MONTHS + 6 * 12;
const PERIODS: usize = WEEKDAYS + 8 * 7;
const ERAS: usize = PERIODS + 2;
const DIGITS: usize = ERAS + 4;
const GMT: usize = DIGITS + 1;
const ZONES: usize = GMT + 10;

pub(super) struct Part {
    pub(super) kind: &'static str,
    pub(super) value: String,
}

thread_local! {
    static SPLIT: Vec<&'static str> = STRINGS.split('\u{1f}').collect();
}

struct Locale {
    row: &'static [u16],
}

impl Locale {
    fn text(&self, index: usize) -> &'static str {
        SPLIT.with(|strings| strings[usize::from(self.row[index])])
    }
}

// the generated locale closest to a requested tag: the same language and region, then the
// language's first listed region, then American English
fn locale(requested: &str) -> Locale {
    let mut parts = requested.split(['-', '_']);
    let language = parts.next().unwrap_or("en").to_ascii_lowercase();
    let mut script = String::new();
    let mut region = String::new();
    for part in parts {
        if part.len() == 4 && part.chars().all(|c| c.is_ascii_alphabetic()) {
            script = part.to_ascii_lowercase();
        } else if part.len() == 2 || (part.len() == 3 && part.chars().all(|c| c.is_ascii_digit())) {
            region = part.to_ascii_uppercase();
        }
    }
    if language == "zh" && region.is_empty() && script == "hant" {
        region = "TW".to_string();
    }
    let wanted = format!("{language}-{region}");
    let found = DATE_LOCALES
        .iter()
        .find(|(id, _)| *id == wanted)
        .or_else(|| {
            DATE_LOCALES
                .iter()
                .find(|(id, _)| id.split('-').next() == Some(language.as_str()))
        })
        .unwrap_or(&DATE_LOCALES[0]);
    Locale { row: found.1 }
}

fn position<const N: usize>(styles: [&str; N], value: Option<&str>) -> usize {
    value
        .and_then(|value| styles.iter().position(|style| *style == value))
        .unwrap_or(0)
}

// the hour letter a request makes: hour12 picks h or H, hourCycle any of the four, and
// otherwise the locale's own from its j pattern
fn hour_letter(options: &DateOptions, locale: &Locale) -> char {
    let default = locale
        .text(DEFAULT_HOUR)
        .chars()
        .find(|c| matches!(c, 'h' | 'H' | 'K' | 'k'))
        .unwrap_or('h');
    match (options.hour12, options.hour_cycle.as_deref()) {
        (Some(true), _) => {
            if default == 'K' {
                'K'
            } else {
                'h'
            }
        }
        (Some(false), _) => 'H',
        (None, Some("h11")) => 'K',
        (None, Some("h12")) => 'h',
        (None, Some("h23")) => 'H',
        (None, Some("h24")) => 'k',
        _ => default,
    }
}

// the resolved hour cycle, for resolvedOptions
pub(super) fn hour_cycle(options: &DateOptions) -> &'static str {
    match hour_letter(options, &locale(&options.locale)) {
        'K' => "h11",
        'h' => "h12",
        'k' => "h24",
        _ => "h23",
    }
}

fn swap_hours(pattern: &str, letter: char) -> String {
    let mut quoted = false;
    pattern
        .chars()
        .map(|c| {
            if c == '\'' {
                quoted = !quoted;
            }
            if !quoted && matches!(c, 'h' | 'H' | 'K' | 'k') {
                letter
            } else {
                c
            }
        })
        .collect()
}

// the pattern for a request, as V8 and ICU would pick it
fn pattern(options: &DateOptions, locale: &Locale) -> String {
    let letter = hour_letter(options, locale);
    let style_index = |style: Option<&str>| -> Option<usize> {
        style.map(|style| match style {
            "full" => 0,
            "long" => 1,
            "medium" => 2,
            _ => 3,
        })
    };
    if options.date_style.is_some() || options.time_style.is_some() {
        let date = style_index(options.date_style.as_deref())
            .map(|index| locale.text(DATE_STYLES + index));
        let time = style_index(options.time_style.as_deref())
            .map(|index| swap_hours(locale.text(TIME_STYLES + index), letter));
        return match (date, time) {
            (Some(date), Some(time)) => {
                let glue =
                    locale.text(GLUE + style_index(options.date_style.as_deref()).unwrap_or(3));
                glue.replace("{1}", date).replace("{0}", &time)
            }
            (Some(date), None) => date.to_string(),
            (None, Some(time)) => time,
            (None, None) => String::new(),
        };
    }
    let weekday = position(WEEKDAY_STYLES, options.weekday.as_deref());
    let year = position(YEAR_STYLES, options.year.as_deref());
    let month = position(MONTH_STYLES, options.month.as_deref());
    let day = position(DAY_STYLES, options.day.as_deref());
    let date_index = ((weekday * 3 + year) * 6 + month) * 3 + day;
    let hour = match options.hour.as_deref() {
        None => 0,
        Some(style) => {
            let double = usize::from(style == "2-digit");
            if matches!(letter, 'h' | 'K') {
                1 + double
            } else {
                3 + double
            }
        }
    };
    let two_or_one = |style: Option<&str>| match style {
        None => 0,
        Some("2-digit") => 2,
        Some(_) => 1,
    };
    let minute = two_or_one(options.minute.as_deref());
    let second = two_or_one(options.second.as_deref());
    let zone = position(ZONE_STYLES, options.time_zone_name.as_deref());
    let time_index = ((hour * 3 + minute) * 3 + second) * 7 + zone;
    let date = locale.text(date_index);
    let mut time = if options.default_time && options.time_zone_name.is_none() {
        let default = match (options.hour12, options.hour_cycle.as_deref()) {
            (None, None) => 0,
            _ if matches!(letter, 'H' | 'k') => 1,
            _ => 2,
        };
        swap_hours(locale.text(DEFAULT_TIMES + default), letter)
    } else {
        swap_hours(locale.text(TIMES + time_index), letter)
    };
    if let Some(digits) = options.fractional.filter(|digits| *digits > 0) {
        time = insert_fraction(&time, digits, super::decimal_separator(&options.locale));
    }
    match (date.is_empty(), time.is_empty()) {
        (false, false) => {
            let glue = match (month, weekday) {
                (5, 3) => 0,
                (5, _) => 1,
                (4, _) => 2,
                _ => 3,
            };
            locale
                .text(GLUE + glue)
                .replace("{1}", date)
                .replace("{0}", &time)
        }
        (false, true) => date.to_string(),
        (true, false) => time,
        (true, true) => locale.text(date_index_of_default()).to_string(),
    }
}

fn date_index_of_default() -> usize {
    // yMd: numeric year, month and day
    ((2 * 6) + 2) * 3 + 2
}

// a fraction of a second after the seconds field, as ICU adds S to a skeleton
fn insert_fraction(pattern: &str, digits: u8, separator: &str) -> String {
    let fraction = format!("{separator}{}", "S".repeat(usize::from(digits)));
    let mut quoted = false;
    let characters: Vec<char> = pattern.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    let mut inserted = false;
    while index < characters.len() {
        let c = characters[index];
        if c == '\'' {
            quoted = !quoted;
        }
        output.push(c);
        if !quoted && c == 's' && characters.get(index + 1) != Some(&'s') && !inserted {
            output.push_str(&fraction.replace('.', "'.'").replace(',', "','"));
            inserted = true;
        }
        index += 1;
    }
    if !inserted {
        output.push_str(&fraction);
    }
    output
}

fn localize_digits(text: &str, digits: &str) -> String {
    if digits.starts_with('0') {
        return text.to_string();
    }
    let digits: Vec<char> = digits.chars().collect();
    text.chars()
        .map(|c| c.to_digit(10).map_or(c, |value| digits[value as usize]))
        .collect()
}

fn padded(value: i64, width: usize) -> String {
    format!("{value:0width$}")
}

// a zone's name in a style, or its offset in the locale's GMT format
fn zone_name(
    locale: &Locale,
    letter: char,
    count: usize,
    zone: Option<&str>,
    offset_seconds: i64,
) -> String {
    let long = count >= 4;
    let named = match letter {
        'z' => Some(if long { 1 } else { 0 }),
        'v' => Some(if long { 3 } else { 2 }),
        _ => None,
    };
    if let (Some(style), Some(zone)) = (named, zone) {
        let primary = super::super::tz::primary(zone).unwrap_or(zone);
        let primary = if matches!(primary, "Etc/UTC" | "Etc/GMT") {
            "UTC"
        } else {
            primary
        };
        if let Some(index) = PATTERN_ZONES.iter().position(|found| *found == primary)
            && let Some((_, found)) = super::super::tz::lookup(primary)
        {
            let january = i64::from(found.offset(1_704_110_400));
            let july = i64::from(found.offset(1_720_094_400));
            let season = if offset_seconds == january {
                Some(0)
            } else if offset_seconds == july {
                Some(1)
            } else {
                None
            };
            if let Some(season) = season {
                let name = locale.text(ZONES + (index * 4 + style) * 2 + season);
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    let base = GMT + if long { 5 } else { 0 };
    if offset_seconds == 0 {
        return locale.text(base).to_string();
    }
    let sign = if offset_seconds < 0 {
        locale.text(base + 3)
    } else {
        locale.text(base + 4)
    };
    let minutes = offset_seconds.abs() / 60;
    let (hours, rest) = (minutes / 60, minutes % 60);
    let number = if long {
        format!("{hours:02}:{rest:02}")
    } else if rest == 0 {
        hours.to_string()
    } else {
        format!("{hours}:{rest:02}")
    };
    let digits = locale.text(DIGITS);
    format!(
        "{}{sign}{}{}",
        locale.text(base + 1),
        localize_digits(&number, digits),
        locale.text(base + 2)
    )
}

// a wall clock time, in milliseconds as if it were UTC, formatted into parts
pub(super) fn format_parts(
    options: &DateOptions,
    local: f64,
    offset_seconds: i64,
    zone: Option<&str>,
) -> Vec<Part> {
    let locale = locale(&options.locale);
    let pattern = pattern(options, &locale);
    let days = (local / 86_400_000.0).floor();
    let (year, month, day) = super::super::dates::civil_from_days(days);
    let (year, month, day) = (year as i64, month as usize, day as i64);
    let within = local.rem_euclid(86_400_000.0) as i64;
    let (hour, minute, second, millisecond) = (
        within / 3_600_000,
        within / 60_000 % 60,
        within / 1000 % 60,
        within % 1000,
    );
    let weekday = (days + 4.0).rem_euclid(7.0) as usize;
    let digits = locale.text(DIGITS);
    let numeric = |value: i64, width: usize| localize_digits(&padded(value, width), digits);
    let mut parts: Vec<Part> = Vec::new();
    let mut push = |kind: &'static str, value: String| {
        if kind == "literal"
            && let Some(last) = parts.last_mut()
            && last.kind == "literal"
        {
            last.value.push_str(&value);
            return;
        }
        parts.push(Part { kind, value });
    };
    let characters: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let c = characters[index];
        if c == '\'' {
            let mut literal = String::new();
            index += 1;
            while index < characters.len() {
                if characters[index] == '\'' {
                    if characters.get(index + 1) == Some(&'\'') {
                        literal.push('\'');
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                literal.push(characters[index]);
                index += 1;
            }
            if literal.is_empty() {
                literal.push('\'');
            }
            push("literal", literal);
            continue;
        }
        if !c.is_ascii_alphabetic() {
            push("literal", c.to_string());
            index += 1;
            continue;
        }
        let mut count = 1;
        while characters.get(index + count) == Some(&c) {
            count += 1;
        }
        index += count;
        let era_year = if year > 0 { year } else { 1 - year };
        match c {
            'G' => {
                let era = usize::from(year > 0);
                let text = match count {
                    4 => locale.text(ERAS + 2 + era).to_string(),
                    5 => locale.text(ERAS + era).chars().take(1).collect(),
                    _ => locale.text(ERAS + era).to_string(),
                };
                push("era", text);
            }
            'y' | 'Y' | 'u' => {
                let text = if count == 2 {
                    numeric(era_year % 100, 2)
                } else {
                    numeric(era_year, count)
                };
                push("year", text);
            }
            'M' | 'L' => {
                let standalone = if c == 'L' { 3 } else { 0 };
                let text = match count {
                    1 => numeric(month as i64 + 1, 1),
                    2 => numeric(month as i64 + 1, 2),
                    3 => locale
                        .text(MONTHS + (standalone + 1) * 12 + month)
                        .to_string(),
                    4 => locale.text(MONTHS + standalone * 12 + month).to_string(),
                    _ => locale
                        .text(MONTHS + (standalone + 2) * 12 + month)
                        .to_string(),
                };
                push("month", text);
            }
            'd' => push("day", numeric(day, count)),
            'E' | 'c' | 'e' => {
                let standalone = if c == 'c' { 4 } else { 0 };
                let form = match count {
                    1..=3 => 1,
                    4 => 0,
                    5 => 3,
                    _ => 2,
                };
                push(
                    "weekday",
                    locale
                        .text(WEEKDAYS + (standalone + form) * 7 + weekday)
                        .to_string(),
                );
            }
            'a' | 'b' | 'B' => push(
                "dayPeriod",
                locale.text(PERIODS + usize::from(hour >= 12)).to_string(),
            ),
            'h' => push(
                "hour",
                numeric(if hour % 12 == 0 { 12 } else { hour % 12 }, count),
            ),
            'K' => push("hour", numeric(hour % 12, count)),
            'H' => push("hour", numeric(hour, count)),
            'k' => push("hour", numeric(if hour == 0 { 24 } else { hour }, count)),
            'm' => push("minute", numeric(minute, count)),
            's' => push("second", numeric(second, count)),
            'S' => {
                let text: String = padded(millisecond, 3)
                    .chars()
                    .chain(std::iter::repeat('0'))
                    .take(count)
                    .collect();
                push("fractionalSecond", localize_digits(&text, digits));
            }
            'z' | 'v' | 'V' | 'O' | 'Z' | 'X' | 'x' => {
                push(
                    "timeZoneName",
                    zone_name(&locale, c, count, zone, offset_seconds),
                );
            }
            _ => push("literal", c.to_string().repeat(count)),
        }
    }
    parts
}

pub(super) fn format(
    options: &DateOptions,
    local: f64,
    offset_seconds: i64,
    zone: Option<&str>,
) -> String {
    format_parts(options, local, offset_seconds, zone)
        .into_iter()
        .map(|part| part.value)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(locale: &str, fields: &[(&str, &str)]) -> DateOptions {
        let mut options = DateOptions {
            locale: locale.to_string(),
            ..DateOptions::default()
        };
        for &(name, value) in fields {
            let slot = match name {
                "weekday" => &mut options.weekday,
                "year" => &mut options.year,
                "month" => &mut options.month,
                "day" => &mut options.day,
                "hour" => &mut options.hour,
                "minute" => &mut options.minute,
                "second" => &mut options.second,
                _ => &mut options.time_zone_name,
            };
            *slot = Some(value.to_string());
        }
        options
    }

    #[test]
    fn skeletons_pick_each_locales_pattern() {
        // 2024-07-04T09:05:07 as a wall clock time
        let local = 1_720_083_907_000.0;
        let cases = [
            (
                "en-US",
                vec![("year", "numeric"), ("month", "numeric")],
                "7/2024",
            ),
            (
                "en-US",
                vec![("month", "2-digit"), ("day", "2-digit")],
                "07/04",
            ),
            (
                "en-GB",
                vec![
                    ("year", "numeric"),
                    ("month", "numeric"),
                    ("day", "numeric"),
                ],
                "04/07/2024",
            ),
            (
                "de-DE",
                vec![("month", "numeric"), ("day", "numeric")],
                "4.7.",
            ),
            (
                "es-ES",
                vec![("month", "2-digit"), ("day", "2-digit")],
                "4/7",
            ),
            ("ja-JP", vec![("year", "numeric")], "2024年"),
            (
                "en-US",
                vec![("hour", "2-digit"), ("minute", "2-digit")],
                "09:05 AM",
            ),
            (
                "en-US",
                vec![("weekday", "long"), ("month", "long"), ("day", "numeric")],
                "Thursday, July 4",
            ),
            ("ar-EG", vec![("day", "numeric")], "٤"),
            (
                "en-US",
                vec![("hour", "numeric"), ("time_zone_name", "short")],
                "9 AM EDT",
            ),
        ];
        for (locale, fields, expected) in cases {
            let zone_offset = if locale == "en-US" { -4 * 3600 } else { 0 };
            assert_eq!(
                format(
                    &options(locale, &fields),
                    local,
                    zone_offset,
                    Some("America/New_York")
                ),
                expected,
                "{locale} {fields:?}"
            );
        }
        let mut defaulted = options("de-DE", &[]);
        defaulted.fill_defaults(true, true);
        assert_eq!(format(&defaulted, local, 0, None), "4.7.2024, 09:05:07");
        let parts: Vec<String> = format_parts(
            &options("en-US", &[("month", "short"), ("day", "numeric")]),
            local,
            0,
            None,
        )
        .into_iter()
        .map(|part| format!("{}:{}", part.kind, part.value))
        .collect();
        assert_eq!(parts, ["month:Jul", "literal: ", "day:4"]);
    }
}
