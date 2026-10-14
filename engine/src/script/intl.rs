// a compact Intl: number, date, plural, relative-time and collation formatting for English
// and the common European locales, enough for apps that format dates, money and counts

use super::numbers::to_fixed;
use super::object::{JsObject, ObjectClass, new_object};
use super::operations::is_primitive;
use super::strings::locale_compare;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    English,
    German,
    French,
    Spanish,
    Italian,
    Portuguese,
    Dutch,
    Russian,
    Polish,
    Japanese,
    Chinese,
}

fn language(locale: &str) -> Language {
    match locale
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_ascii_lowercase()
        .as_str()
    {
        "de" => Language::German,
        "fr" => Language::French,
        "es" => Language::Spanish,
        "it" => Language::Italian,
        "pt" => Language::Portuguese,
        "nl" => Language::Dutch,
        "ru" | "uk" => Language::Russian,
        "pl" => Language::Polish,
        "ja" => Language::Japanese,
        "zh" => Language::Chinese,
        _ => Language::English,
    }
}

// group separator, decimal separator, and whether currency follows the number
fn separators(language: Language) -> (&'static str, &'static str, bool) {
    match language {
        Language::German
        | Language::Spanish
        | Language::Italian
        | Language::Portuguese
        | Language::Dutch => (".", ",", true),
        Language::French => ("\u{202f}", ",", true),
        Language::Russian | Language::Polish => ("\u{a0}", ",", true),
        _ => (",", ".", false),
    }
}

const MONTHS: [[&str; 12]; 7] = [
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
    [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ],
    [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ],
    [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ],
    [
        "gennaio",
        "febbraio",
        "marzo",
        "aprile",
        "maggio",
        "giugno",
        "luglio",
        "agosto",
        "settembre",
        "ottobre",
        "novembre",
        "dicembre",
    ],
    [
        "janeiro",
        "fevereiro",
        "março",
        "abril",
        "maio",
        "junho",
        "julho",
        "agosto",
        "setembro",
        "outubro",
        "novembro",
        "dezembro",
    ],
    [
        "januari",
        "februari",
        "maart",
        "april",
        "mei",
        "juni",
        "juli",
        "augustus",
        "september",
        "oktober",
        "november",
        "december",
    ],
];

const WEEKDAYS: [[&str; 7]; 7] = [
    [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ],
    [
        "Sonntag",
        "Montag",
        "Dienstag",
        "Mittwoch",
        "Donnerstag",
        "Freitag",
        "Samstag",
    ],
    [
        "dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi",
    ],
    [
        "domingo",
        "lunes",
        "martes",
        "miércoles",
        "jueves",
        "viernes",
        "sábado",
    ],
    [
        "domenica",
        "lunedì",
        "martedì",
        "mercoledì",
        "giovedì",
        "venerdì",
        "sabato",
    ],
    [
        "domingo",
        "segunda-feira",
        "terça-feira",
        "quarta-feira",
        "quinta-feira",
        "sexta-feira",
        "sábado",
    ],
    [
        "zondag",
        "maandag",
        "dinsdag",
        "woensdag",
        "donderdag",
        "vrijdag",
        "zaterdag",
    ],
];

fn names_index(language: Language) -> usize {
    match language {
        Language::German => 1,
        Language::French => 2,
        Language::Spanish => 3,
        Language::Italian => 4,
        Language::Portuguese => 5,
        Language::Dutch => 6,
        _ => 0,
    }
}

fn currency_symbol(code: &str) -> (String, usize) {
    let (symbol, digits) = match code {
        "USD" => ("$", 2),
        "EUR" => ("€", 2),
        "GBP" => ("£", 2),
        "JPY" => ("¥", 0),
        "CNY" => ("CN¥", 2),
        "INR" => ("₹", 2),
        "KRW" => ("₩", 0),
        "CAD" => ("CA$", 2),
        "AUD" => ("A$", 2),
        "BRL" => ("R$", 2),
        "MXN" => ("MX$", 2),
        "CHF" => ("CHF", 2),
        "SEK" => ("SEK", 2),
        "RUB" => ("RUB", 2),
        "PLN" => ("PLN", 2),
        other => (other, 2),
    };
    (symbol.to_string(), digits)
}

#[derive(Clone, Debug)]
pub(super) enum NumberStyle {
    Decimal,
    Percent,
    Currency(String),
}

#[derive(Clone, Debug)]
pub(super) struct NumberOptions {
    locale: String,
    style: NumberStyle,
    minimum_fraction: usize,
    maximum_fraction: usize,
    minimum_integer: usize,
    grouping: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DateOptions {
    locale: String,
    date_style: Option<String>,
    time_style: Option<String>,
    weekday: Option<String>,
    year: Option<String>,
    month: Option<String>,
    day: Option<String>,
    hour: Option<String>,
    minute: Option<String>,
    second: Option<String>,
    hour12: Option<bool>,
    utc: bool,
}

impl DateOptions {
    // no component or style was asked for
    pub(super) fn is_empty(&self) -> bool {
        self.date_style.is_none()
            && self.time_style.is_none()
            && self.weekday.is_none()
            && self.year.is_none()
            && self.month.is_none()
            && self.day.is_none()
            && self.hour.is_none()
            && self.minute.is_none()
            && self.second.is_none()
    }

    // the components toLocaleString and its siblings show by default
    pub(super) fn fill_defaults(&mut self, date: bool, time: bool) {
        if date {
            self.year = Some("numeric".to_string());
            self.month = Some("numeric".to_string());
            self.day = Some("numeric".to_string());
        }
        if time {
            self.hour = Some("numeric".to_string());
            self.minute = Some("2-digit".to_string());
            self.second = Some("2-digit".to_string());
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct CollatorOptions {
    numeric: bool,
    base: bool,
}

#[derive(Clone, Debug)]
pub(super) struct PluralOptions {
    locale: String,
    ordinal: bool,
}

#[derive(Clone, Debug)]
pub(super) struct RelativeOptions {
    auto: bool,
}

#[derive(Clone, Debug)]
pub(super) enum IntlMethod {
    NumberFormatConstructor,
    DateTimeFormatConstructor,
    CollatorConstructor,
    PluralRulesConstructor,
    RelativeTimeFormatConstructor,
    GetCanonicalLocales,
    SupportedLocalesOf,
    // getters that hand back a bound format or compare function
    FormatGetter,
    CompareGetter,
    Select,
    RelativeFormat,
    ResolvedOptions,
    FormatNumber(Rc<NumberOptions>),
    FormatDate(Rc<DateOptions>),
    Compare(Rc<CollatorOptions>),
}

pub(super) enum IntlState {
    Number(Rc<NumberOptions>),
    Date(Rc<DateOptions>),
    Collator(Rc<CollatorOptions>),
    Plural(Rc<PluralOptions>),
    Relative(Rc<RelativeOptions>),
}

fn group(integer: &str, separator: &str) -> String {
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    for (index, character) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index).is_multiple_of(3) {
            grouped.push_str(separator);
        }
        grouped.push(character);
    }
    grouped
}

pub(super) fn format_number(options: &NumberOptions, value: f64) -> String {
    let language = language(&options.locale);
    let (group_separator, decimal, currency_after) = separators(language);
    if value.is_nan() {
        return "NaN".to_string();
    }
    let scaled = if matches!(options.style, NumberStyle::Percent) {
        value * 100.0
    } else {
        value
    };
    let body = if scaled.is_infinite() {
        "∞".to_string()
    } else {
        let fixed = to_fixed(scaled.abs(), options.maximum_fraction);
        let (integer, fraction) = fixed.split_once('.').unwrap_or((&fixed, ""));
        let mut fraction = fraction.to_string();
        while fraction.len() > options.minimum_fraction && fraction.ends_with('0') {
            fraction.pop();
        }
        let integer = format!("{integer:0>width$}", width = options.minimum_integer);
        let integer = if options.grouping {
            group(&integer, group_separator)
        } else {
            integer
        };
        if fraction.is_empty() {
            integer
        } else {
            format!("{integer}{decimal}{fraction}")
        }
    };
    let negative = scaled < 0.0
        || (scaled == 0.0 && scaled.is_sign_negative() && body.bytes().any(|byte| byte != b'0'));
    let sign = if negative { "-" } else { "" };
    match &options.style {
        NumberStyle::Decimal => format!("{sign}{body}"),
        NumberStyle::Percent => match language {
            Language::English | Language::Japanese | Language::Chinese => format!("{sign}{body}%"),
            Language::French => format!("{sign}{body}\u{202f}%"),
            _ => format!("{sign}{body}\u{a0}%"),
        },
        NumberStyle::Currency(code) => {
            let (symbol, _) = currency_symbol(code);
            if currency_after {
                format!("{sign}{body}\u{a0}{symbol}")
            } else {
                format!("{sign}{symbol}{body}")
            }
        }
    }
}

fn two(value: f64) -> String {
    format!("{:02}", value as i64)
}

pub(super) fn format_date(options: &DateOptions, local: f64) -> String {
    if local.is_nan() {
        return "Invalid Date".to_string();
    }
    let language = language(&options.locale);
    let names = names_index(language);
    let days = (local / 86_400_000.0).floor();
    let (year, month, date) = super::dates::civil_from_days(days);
    let within = local.rem_euclid(86_400_000.0);
    let (hours, minutes, seconds) = (
        (within / 3_600_000.0).floor(),
        (within / 60_000.0).floor() % 60.0,
        (within / 1000.0).floor() % 60.0,
    );
    let weekday = (days + 4.0).rem_euclid(7.0) as usize;
    let english_order = matches!(language, Language::English)
        && !options.locale.to_ascii_lowercase().starts_with("en-gb");
    let hour12 = options
        .hour12
        .unwrap_or(matches!(language, Language::English) && english_order);
    let month_name = MONTHS[names][month as usize];
    let short_month: String = month_name.chars().take(3).collect();
    let weekday_name = WEEKDAYS[names][weekday];

    let time = |with_seconds: bool| -> String {
        let seconds_text = if with_seconds {
            format!(":{}", two(seconds))
        } else {
            String::new()
        };
        if hour12 {
            let hour = hours as i64 % 12;
            let hour = if hour == 0 { 12 } else { hour };
            let meridiem = if hours < 12.0 { "AM" } else { "PM" };
            format!("{hour}:{}{seconds_text} {meridiem}", two(minutes))
        } else {
            format!("{}:{}{seconds_text}", two(hours), two(minutes))
        }
    };
    let numeric_date = |two_digit_year: bool| -> String {
        let year_text = if two_digit_year {
            two(year.rem_euclid(100.0))
        } else {
            format!("{}", year as i64)
        };
        match language {
            Language::English if english_order => {
                format!("{}/{}/{year_text}", month as i64 + 1, date as i64)
            }
            Language::English => format!("{}/{}/{year_text}", two(date), two(month + 1.0)),
            Language::German | Language::Russian | Language::Polish => {
                format!("{}.{}.{year_text}", two(date), two(month + 1.0))
            }
            Language::Dutch => format!("{}-{}-{year_text}", date as i64, month as i64 + 1),
            Language::Japanese | Language::Chinese => {
                format!("{year_text}/{}/{}", month as i64 + 1, date as i64)
            }
            _ => format!("{}/{}/{year_text}", two(date), two(month + 1.0)),
        }
    };
    let long_date = |weekday_too: bool, abbreviated: bool| -> String {
        let month_text = if abbreviated {
            short_month.clone()
        } else {
            month_name.to_string()
        };
        let day_month = match language {
            Language::English if english_order => {
                format!("{month_text} {}, {}", date as i64, year as i64)
            }
            Language::German => format!("{}. {month_text} {}", date as i64, year as i64),
            Language::Spanish | Language::Portuguese => {
                format!("{} de {month_text} de {}", date as i64, year as i64)
            }
            _ => format!("{} {month_text} {}", date as i64, year as i64),
        };
        if weekday_too {
            format!("{weekday_name}, {day_month}")
        } else {
            day_month
        }
    };

    if options.date_style.is_some() || options.time_style.is_some() {
        let date_part = options.date_style.as_deref().map(|style| match style {
            "full" => long_date(true, false),
            "long" => long_date(false, false),
            "medium" => long_date(false, true),
            _ => numeric_date(true),
        });
        let time_part = options
            .time_style
            .as_deref()
            .map(|style| time(style != "short"));
        return match (date_part, time_part) {
            (Some(date), Some(time)) => {
                let joiner = if matches!(options.date_style.as_deref(), Some("full" | "long"))
                    && matches!(language, Language::English)
                {
                    " at "
                } else {
                    ", "
                };
                format!("{date}{joiner}{time}")
            }
            (Some(date), None) => date,
            (None, Some(time)) => time,
            (None, None) => String::new(),
        };
    }

    let any_date = options.weekday.is_some()
        || options.year.is_some()
        || options.month.is_some()
        || options.day.is_some();
    let any_time = options.hour.is_some() || options.minute.is_some() || options.second.is_some();
    let (date_requested, time_requested) = if !any_date && !any_time {
        (true, false)
    } else {
        (any_date, any_time)
    };
    let mut parts = Vec::new();
    if date_requested {
        let textual_month = matches!(options.month.as_deref(), Some("long" | "short" | "narrow"));
        if textual_month {
            let month_text = match options.month.as_deref() {
                Some("long") => month_name.to_string(),
                Some("narrow") => month_name.chars().take(1).collect(),
                _ => short_month.clone(),
            };
            let mut text = match (options.day.is_some(), options.year.is_some()) {
                (true, true) => match language {
                    Language::English if english_order => {
                        format!("{month_text} {}, {}", date as i64, year as i64)
                    }
                    Language::German => format!("{}. {month_text} {}", date as i64, year as i64),
                    _ => format!("{} {month_text} {}", date as i64, year as i64),
                },
                (true, false) => match language {
                    Language::English if english_order => format!("{month_text} {}", date as i64),
                    Language::German => format!("{}. {month_text}", date as i64),
                    _ => format!("{} {month_text}", date as i64),
                },
                (false, true) => format!("{month_text} {}", year as i64),
                (false, false) => month_text,
            };
            if let Some(style) = &options.weekday {
                let weekday_text: String = match style.as_str() {
                    "long" => weekday_name.to_string(),
                    "narrow" => weekday_name.chars().take(1).collect(),
                    _ => weekday_name.chars().take(3).collect(),
                };
                text = format!("{weekday_text}, {text}");
            }
            parts.push(text);
        } else if options.year.is_some() && options.month.is_none() && options.day.is_none() {
            parts.push(if options.year.as_deref() == Some("2-digit") {
                two(year.rem_euclid(100.0))
            } else {
                format!("{}", year as i64)
            });
        } else if let (Some(style), false) = (
            &options.weekday,
            options.day.is_some() || options.month.is_some() || options.year.is_some(),
        ) {
            parts.push(match style.as_str() {
                "long" => weekday_name.to_string(),
                "narrow" => weekday_name.chars().take(1).collect(),
                _ => weekday_name.chars().take(3).collect(),
            });
        } else {
            parts.push(numeric_date(options.year.as_deref() == Some("2-digit")));
        }
    }
    if time_requested {
        let with_seconds = options.second.is_some();
        if options.hour.is_some() {
            parts.push(time(with_seconds));
        } else {
            parts.push(if with_seconds {
                format!("{}:{}", two(minutes), two(seconds))
            } else {
                two(minutes)
            });
        }
    }
    parts.join(", ")
}

fn plural_category(options: &PluralOptions, value: f64) -> &'static str {
    let integer = value.fract() == 0.0;
    let n = value.abs();
    if options.ordinal {
        if language(&options.locale) != Language::English {
            return "other";
        }
        let (ten, hundred) = (n % 10.0, n % 100.0);
        return if ten == 1.0 && hundred != 11.0 {
            "one"
        } else if ten == 2.0 && hundred != 12.0 {
            "two"
        } else if ten == 3.0 && hundred != 13.0 {
            "few"
        } else {
            "other"
        };
    }
    match language(&options.locale) {
        Language::French => {
            if n < 2.0 {
                "one"
            } else {
                "other"
            }
        }
        Language::Japanese | Language::Chinese => "other",
        Language::Russian if integer => {
            let (ten, hundred) = (n % 10.0, n % 100.0);
            if ten == 1.0 && hundred != 11.0 {
                "one"
            } else if (2.0..=4.0).contains(&ten) && !(12.0..=14.0).contains(&hundred) {
                "few"
            } else {
                "many"
            }
        }
        Language::Polish if integer => {
            let (ten, hundred) = (n % 10.0, n % 100.0);
            if n == 1.0 {
                "one"
            } else if (2.0..=4.0).contains(&ten) && !(12.0..=14.0).contains(&hundred) {
                "few"
            } else {
                "many"
            }
        }
        Language::Russian | Language::Polish => "other",
        _ => {
            if n == 1.0 && integer {
                "one"
            } else {
                "other"
            }
        }
    }
}

fn relative_text(options: &RelativeOptions, value: f64, unit: &str) -> Option<String> {
    let unit = unit.trim_end_matches('s');
    if !matches!(
        unit,
        "second" | "minute" | "hour" | "day" | "week" | "month" | "quarter" | "year"
    ) {
        return None;
    }
    if options.auto {
        let phrase = match (unit, value as i64, value.fract() == 0.0) {
            ("day", 0, true) => Some("today"),
            ("day", -1, true) => Some("yesterday"),
            ("day", 1, true) => Some("tomorrow"),
            ("second", 0, true) => Some("now"),
            ("week" | "month" | "quarter" | "year", -1, true) => Some("last"),
            ("week" | "month" | "quarter" | "year", 0, true) => Some("this"),
            ("week" | "month" | "quarter" | "year", 1, true) => Some("next"),
            _ => None,
        };
        if let Some(phrase) = phrase {
            return Some(match phrase {
                "last" | "this" | "next" => format!("{phrase} {unit}"),
                other => other.to_string(),
            });
        }
    }
    let magnitude = value.abs();
    let count = super::number_to_string(magnitude);
    let noun = if magnitude == 1.0 {
        unit.to_string()
    } else {
        format!("{unit}s")
    };
    Some(
        if value < 0.0 || (value == 0.0 && value.is_sign_negative()) {
            format!("{count} {noun} ago")
        } else {
            format!("in {count} {noun}")
        },
    )
}

fn natural_compare(left: &str, right: &str, base: bool) -> std::cmp::Ordering {
    let fold = |text: &str| {
        if base {
            text.to_lowercase()
        } else {
            text.to_string()
        }
    };
    let (left, right) = (fold(left), fold(right));
    let mut a = left.chars().peekable();
    let mut b = right.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut first = String::new();
                while let Some(digit) = a.peek().copied().filter(char::is_ascii_digit) {
                    first.push(digit);
                    a.next();
                }
                let mut second = String::new();
                while let Some(digit) = b.peek().copied().filter(char::is_ascii_digit) {
                    second.push(digit);
                    b.next();
                }
                let first = first.trim_start_matches('0');
                let second = second.trim_start_matches('0');
                let order = first
                    .len()
                    .cmp(&second.len())
                    .then_with(|| first.cmp(second));
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = locale_compare(&x.to_string(), &y.to_string());
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    fn locale_argument(&mut self, value: &Value) -> Result<String, ScriptError> {
        Ok(match value {
            Value::Undefined => "en-US".to_string(),
            Value::String(text) => text.to_string(),
            Value::Array(list) => match list.borrow().first() {
                Some(Value::String(text)) => text.to_string(),
                _ => "en-US".to_string(),
            },
            other => self.coerce_string(other.clone())?,
        })
    }

    fn option_text(&mut self, options: &Value, key: &str) -> Result<Option<String>, ScriptError> {
        if is_primitive(options) {
            return Ok(None);
        }
        match self.get_property(options, key)? {
            Value::Undefined => Ok(None),
            value => Ok(Some(self.coerce_string(value)?)),
        }
    }

    fn option_number(
        &mut self,
        options: &Value,
        key: &str,
        range: (f64, f64),
    ) -> Result<Option<usize>, ScriptError> {
        if is_primitive(options) {
            return Ok(None);
        }
        match self.get_property(options, key)? {
            Value::Undefined => Ok(None),
            value => {
                let number = self.coerce_number(value)?.floor();
                if !(range.0..=range.1).contains(&number) {
                    return Err(self.range_error(format!("{key} value is out of range.")));
                }
                Ok(Some(number as usize))
            }
        }
    }

    pub(super) fn number_options(
        &mut self,
        locales: &Value,
        options: &Value,
    ) -> Result<NumberOptions, ScriptError> {
        let locale = self.locale_argument(locales)?;
        let style = match self.option_text(options, "style")?.as_deref() {
            None | Some("decimal") | Some("unit") => NumberStyle::Decimal,
            Some("percent") => NumberStyle::Percent,
            Some("currency") => match self.option_text(options, "currency")? {
                Some(code) => NumberStyle::Currency(code.to_ascii_uppercase()),
                None => {
                    return Err(self.type_error("Currency code is required with currency style."));
                }
            },
            Some(other) => {
                return Err(self.range_error(format!(
                    "Value {other} out of range for Intl.NumberFormat options property style"
                )));
            }
        };
        let (default_minimum, default_maximum) = match &style {
            NumberStyle::Currency(code) => {
                let digits = currency_symbol(code).1;
                (digits, digits)
            }
            NumberStyle::Percent => (0, 0),
            NumberStyle::Decimal => (0, 3),
        };
        let minimum_fraction =
            self.option_number(options, "minimumFractionDigits", (0.0, 100.0))?;
        let maximum_fraction =
            self.option_number(options, "maximumFractionDigits", (0.0, 100.0))?;
        let minimum_fraction_value =
            minimum_fraction.unwrap_or(default_minimum.min(maximum_fraction.unwrap_or(usize::MAX)));
        let maximum_fraction_value =
            maximum_fraction.unwrap_or(default_maximum.max(minimum_fraction_value));
        if minimum_fraction_value > maximum_fraction_value {
            return Err(self.range_error("maximumFractionDigits value is out of range."));
        }
        let grouping = if is_primitive(options) {
            true
        } else {
            !matches!(
                self.get_property(options, "useGrouping")?,
                Value::Bool(false)
            )
        };
        Ok(NumberOptions {
            locale,
            style,
            minimum_fraction: minimum_fraction_value,
            maximum_fraction: maximum_fraction_value,
            minimum_integer: self
                .option_number(options, "minimumIntegerDigits", (1.0, 21.0))?
                .unwrap_or(1),
            grouping,
        })
    }

    pub(super) fn date_options(
        &mut self,
        locales: &Value,
        options: &Value,
    ) -> Result<DateOptions, ScriptError> {
        let mut result = DateOptions {
            locale: self.locale_argument(locales)?,
            ..DateOptions::default()
        };
        result.date_style = self.option_text(options, "dateStyle")?;
        result.time_style = self.option_text(options, "timeStyle")?;
        result.weekday = self.option_text(options, "weekday")?;
        result.year = self.option_text(options, "year")?;
        result.month = self.option_text(options, "month")?;
        result.day = self.option_text(options, "day")?;
        result.hour = self.option_text(options, "hour")?;
        result.minute = self.option_text(options, "minute")?;
        result.second = self.option_text(options, "second")?;
        if !is_primitive(options) {
            result.hour12 = match self.get_property(options, "hour12")? {
                Value::Undefined => None,
                value => Some(value.truthy()),
            };
            result.utc = matches!(
                self.option_text(options, "timeZone")?.as_deref(),
                Some("UTC" | "Etc/UTC" | "GMT")
            );
        }
        Ok(result)
    }

    // a date's time value in the formatter's zone
    fn zoned(&self, options: &DateOptions, time: f64) -> f64 {
        if options.utc {
            time
        } else {
            time + f64::from(self.time_zone.offset_minutes(time)) * 60_000.0
        }
    }

    pub(super) fn format_date_with(
        &mut self,
        options: &DateOptions,
        date: &Value,
    ) -> Result<String, ScriptError> {
        let time = match date {
            Value::Undefined => self.now_milliseconds(),
            Value::Object(object) => match object.borrow().class {
                ObjectClass::Date(time) => Some(time),
                _ => None,
            }
            .unwrap_or(f64::NAN),
            other => self.coerce_number(other.clone())?,
        };
        if time.is_nan() {
            return Err(self.range_error("Invalid time value"));
        }
        Ok(format_date(options, self.zoned(options, time)))
    }

    fn intl_state(&mut self, this: &Value) -> Result<Rc<IntlState>, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::Intl(state) = &object.borrow().class
        {
            return Ok(state.clone());
        }
        Err(self.type_error("Method called on incompatible receiver"))
    }

    fn intl_object(&self, state: IntlState, prototype: object::ObjectHandle) -> Value {
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Intl(Rc::new(state)),
        )))
    }

    pub(super) fn call_intl(
        &mut self,
        method: IntlMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use IntlMethod as M;
        let first = argument(&arguments, 0);
        let second = argument(&arguments, 1);
        match method {
            M::NumberFormatConstructor => {
                let options = self.number_options(&first, &second)?;
                let prototype = self.intrinsics.intl_prototypes[0].clone();
                Ok(self.intl_object(IntlState::Number(Rc::new(options)), prototype))
            }
            M::DateTimeFormatConstructor => {
                let options = self.date_options(&first, &second)?;
                let prototype = self.intrinsics.intl_prototypes[1].clone();
                Ok(self.intl_object(IntlState::Date(Rc::new(options)), prototype))
            }
            M::CollatorConstructor => {
                let numeric =
                    !is_primitive(&second) && self.get_property(&second, "numeric")?.truthy();
                let sensitivity = self.option_text(&second, "sensitivity")?;
                let base = matches!(sensitivity.as_deref(), Some("base" | "accent"));
                let prototype = self.intrinsics.intl_prototypes[2].clone();
                Ok(self.intl_object(
                    IntlState::Collator(Rc::new(CollatorOptions { numeric, base })),
                    prototype,
                ))
            }
            M::PluralRulesConstructor => {
                let locale = self.locale_argument(&first)?;
                let ordinal = self.option_text(&second, "type")?.as_deref() == Some("ordinal");
                let prototype = self.intrinsics.intl_prototypes[3].clone();
                Ok(self.intl_object(
                    IntlState::Plural(Rc::new(PluralOptions { locale, ordinal })),
                    prototype,
                ))
            }
            M::RelativeTimeFormatConstructor => {
                let auto = self.option_text(&second, "numeric")?.as_deref() == Some("auto");
                let prototype = self.intrinsics.intl_prototypes[4].clone();
                Ok(self.intl_object(
                    IntlState::Relative(Rc::new(RelativeOptions { auto })),
                    prototype,
                ))
            }
            M::GetCanonicalLocales | M::SupportedLocalesOf => {
                let list = match &first {
                    Value::Undefined => Vec::new(),
                    Value::Array(list) => list.borrow().clone(),
                    other => vec![other.clone()],
                };
                let mut locales = Vec::new();
                for value in list {
                    locales.push(Value::String(self.coerce_string(value)?.into()));
                }
                Ok(Self::new_array(locales))
            }
            M::FormatGetter => {
                let state = self.intl_state(&this)?;
                let native = match &*state {
                    IntlState::Number(options) => M::FormatNumber(options.clone()),
                    IntlState::Date(options) => M::FormatDate(options.clone()),
                    _ => return Err(self.type_error("format is not available on this object")),
                };
                Ok(self.native_function(Native::Intl(Rc::new(native)), "", 1))
            }
            M::CompareGetter => {
                let state = self.intl_state(&this)?;
                let IntlState::Collator(options) = &*state else {
                    return Err(self.type_error("compare is not available on this object"));
                };
                Ok(self.native_function(Native::Intl(Rc::new(M::Compare(options.clone()))), "", 2))
            }
            M::FormatNumber(options) => {
                let number = self.coerce_number(first)?;
                Ok(Value::String(format_number(&options, number).into()))
            }
            M::FormatDate(options) => Ok(Value::String(
                self.format_date_with(&options, &first)?.into(),
            )),
            M::Compare(options) => {
                let left = self.coerce_string(first)?;
                let right = self.coerce_string(second)?;
                let order = if options.numeric || options.base {
                    natural_compare(&left, &right, options.base)
                } else {
                    locale_compare(&left, &right)
                };
                Ok(Value::Number(match order {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                }))
            }
            M::Select => {
                let state = self.intl_state(&this)?;
                let IntlState::Plural(options) = &*state else {
                    return Err(self.type_error("select is not available on this object"));
                };
                let options = options.clone();
                let number = self.coerce_number(first)?;
                Ok(Value::String(plural_category(&options, number).into()))
            }
            M::RelativeFormat => {
                let state = self.intl_state(&this)?;
                let IntlState::Relative(options) = &*state else {
                    return Err(self.type_error("format is not available on this object"));
                };
                let options = options.clone();
                let value = self.coerce_number(first)?;
                let unit = self.coerce_string(second)?;
                match relative_text(&options, value, &unit) {
                    Some(text) => Ok(Value::String(text.into())),
                    None => {
                        Err(self
                            .range_error(format!("Invalid unit argument for format() '{unit}'")))
                    }
                }
            }
            M::ResolvedOptions => {
                let state = self.intl_state(&this)?;
                let entries = match &*state {
                    IntlState::Number(options) => vec![
                        (
                            "locale".to_string(),
                            Value::String(options.locale.as_str().into()),
                        ),
                        ("numberingSystem".to_string(), Value::String("latn".into())),
                        (
                            "minimumFractionDigits".to_string(),
                            Value::Number(options.minimum_fraction as f64),
                        ),
                        (
                            "maximumFractionDigits".to_string(),
                            Value::Number(options.maximum_fraction as f64),
                        ),
                    ],
                    IntlState::Date(options) => vec![
                        (
                            "locale".to_string(),
                            Value::String(options.locale.as_str().into()),
                        ),
                        ("calendar".to_string(), Value::String("gregory".into())),
                        ("numberingSystem".to_string(), Value::String("latn".into())),
                        (
                            "timeZone".to_string(),
                            Value::String(if options.utc {
                                "UTC".into()
                            } else {
                                self.time_zone
                                    .name(0.0)
                                    .unwrap_or_else(|| "UTC".to_string())
                                    .into()
                            }),
                        ),
                    ],
                    _ => vec![("locale".to_string(), Value::String("en-US".into()))],
                };
                Ok(self.object_from_entries(entries))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(locale: &str, style: NumberStyle, minimum: usize, maximum: usize) -> NumberOptions {
        NumberOptions {
            locale: locale.to_string(),
            style,
            minimum_fraction: minimum,
            maximum_fraction: maximum,
            minimum_integer: 1,
            grouping: true,
        }
    }

    #[test]
    fn numbers_format_per_locale() {
        assert_eq!(
            format_number(&options("en-US", NumberStyle::Decimal, 0, 3), 1234567.8912),
            "1,234,567.891"
        );
        assert_eq!(
            format_number(&options("de-DE", NumberStyle::Decimal, 0, 3), 1234.5),
            "1.234,5"
        );
        assert_eq!(
            format_number(
                &options("en-US", NumberStyle::Currency("USD".into()), 2, 2),
                -5.5
            ),
            "-$5.50"
        );
        assert_eq!(
            format_number(
                &options("de-DE", NumberStyle::Currency("EUR".into()), 2, 2),
                9.99
            ),
            "9,99\u{a0}€"
        );
        assert_eq!(
            format_number(&options("en-US", NumberStyle::Percent, 0, 0), 0.256),
            "26%"
        );
    }

    #[test]
    fn dates_plurals_and_relative_times_format() {
        let local = 1_705_331_045_000.0;
        let mut date = DateOptions {
            locale: "en-US".to_string(),
            ..DateOptions::default()
        };
        assert_eq!(format_date(&date, local), "1/15/2024");
        date.date_style = Some("full".to_string());
        date.time_style = Some("short".to_string());
        assert_eq!(
            format_date(&date, local),
            "Monday, January 15, 2024 at 3:04 PM"
        );
        let german = DateOptions {
            locale: "de-DE".to_string(),
            month: Some("long".into()),
            day: Some("numeric".into()),
            year: Some("numeric".into()),
            ..DateOptions::default()
        };
        assert_eq!(format_date(&german, local), "15. Januar 2024");
        let plural = PluralOptions {
            locale: "en".to_string(),
            ordinal: true,
        };
        assert_eq!(
            [1.0, 2.0, 3.0, 11.0, 22.0].map(|n| plural_category(&plural, n)),
            ["one", "two", "few", "other", "two"]
        );
        let relative = RelativeOptions { auto: true };
        assert_eq!(
            relative_text(&relative, -1.0, "day").as_deref(),
            Some("yesterday")
        );
        assert_eq!(
            relative_text(&relative, 3.0, "hours").as_deref(),
            Some("in 3 hours")
        );
        assert_eq!(
            natural_compare("item10", "item9", false),
            std::cmp::Ordering::Greater
        );
    }
}
