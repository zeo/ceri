// a compact Intl: number, date, plural, relative-time and collation formatting for English
// and the common European locales, enough for apps that format dates, money and counts

use super::numbers::to_fixed;
use super::object::{JsObject, ObjectClass, new_object};
use super::operations::is_primitive;
use super::strings::locale_compare;
use super::*;

mod date_patterns;
mod date_patterns_data;

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

// the decimal separator a locale writes
pub(super) fn decimal_separator(locale: &str) -> &'static str {
    separators(language(locale)).1
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

// an offset zone as +hh:mm, from +hh, +hhmm or +hh:mm
fn offset_zone(text: &str) -> Option<String> {
    let sign = text
        .chars()
        .next()
        .filter(|sign| matches!(sign, '+' | '-'))?;
    let digits: String = text[1..].chars().filter(|&found| found != ':').collect();
    let valid_colon = !text.contains(':') || text.len() == 6 && &text[3..4] == ":";
    if !valid_colon || !digits.chars().all(|found| found.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes) = match digits.len() {
        2 => (&digits[..2], "00"),
        4 => (&digits[..2], &digits[2..]),
        _ => return None,
    };
    (hours < "24" && minutes < "60").then(|| format!("{sign}{hours}:{minutes}"))
}

// zones whose IANA name changed while CLDR, and so Chrome's resolved options, keep the old one
const CLDR_ZONE_NAMES: &[(&str, &str)] = &[
    ("Africa/Asmara", "Africa/Asmera"),
    ("America/Argentina/Buenos_Aires", "America/Buenos_Aires"),
    ("America/Argentina/Catamarca", "America/Catamarca"),
    ("America/Atikokan", "America/Coral_Harbour"),
    ("America/Argentina/Cordoba", "America/Cordoba"),
    ("America/Nuuk", "America/Godthab"),
    ("America/Indiana/Indianapolis", "America/Indianapolis"),
    ("America/Argentina/Jujuy", "America/Jujuy"),
    ("America/Kentucky/Louisville", "America/Louisville"),
    ("America/Argentina/Mendoza", "America/Mendoza"),
    ("Asia/Kolkata", "Asia/Calcutta"),
    ("Asia/Kathmandu", "Asia/Katmandu"),
    ("Asia/Yangon", "Asia/Rangoon"),
    ("Asia/Ho_Chi_Minh", "Asia/Saigon"),
    ("Atlantic/Faroe", "Atlantic/Faeroe"),
    ("Europe/Kyiv", "Europe/Kiev"),
    ("Pacific/Kanton", "Pacific/Enderbury"),
    ("Pacific/Pohnpei", "Pacific/Ponape"),
    ("Pacific/Chuuk", "Pacific/Truk"),
];

// a time zone as a formatter resolves it: UTC for its aliases, a link by the zone it names,
// in CLDR's spelling
pub(super) fn canonical_zone(zone: &str) -> Option<String> {
    if let Some(offset) = offset_zone(zone) {
        return Some(offset);
    }
    let primary = super::tz::primary(zone)?;
    Some(match primary {
        "Etc/UTC" | "Etc/GMT" | "Etc/UCT" | "Etc/Universal" | "Etc/Zulu" | "Etc/Greenwich" => {
            "UTC".to_string()
        }
        _ if zone.eq_ignore_ascii_case("utc") || zone.eq_ignore_ascii_case("gmt") => {
            "UTC".to_string()
        }
        other => CLDR_ZONE_NAMES
            .iter()
            .find(|(iana, _)| *iana == other)
            .map_or(other, |(_, cldr)| cldr)
            .to_string(),
    })
}

// a canonical zone's offset from UTC, in seconds, at a time value
pub(super) fn zone_offset(zone: &str, time: f64) -> i32 {
    if let Some(sign) = zone.strip_prefix(['+', '-']).map(|_| zone.starts_with('-')) {
        let hours: i32 = zone[1..3].parse().unwrap_or(0);
        let minutes: i32 = zone[4..6].parse().unwrap_or(0);
        let offset = hours * 3600 + minutes * 60;
        return if sign { -offset } else { offset };
    }
    super::tz::lookup(zone).map_or(0, |(_, found)| found.offset((time / 1000.0).floor() as i64))
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
    hour_cycle: Option<String>,
    time_zone_name: Option<String>,
    fractional: Option<u8>,
    // the time fields came from toLocaleString's defaults rather than the caller
    default_time: bool,
    // the zone asked for, as an IANA identifier or a +hh:mm offset; none for the host's own
    zone: Option<String>,
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
            && self.time_zone_name.is_none()
            && self.fractional.is_none()
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
            self.default_time = true;
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
pub(super) struct ListOptions {
    locale: String,
    // conjunction, disjunction or unit
    kind: String,
    // long, short or narrow
    style: String,
}

// the pieces a list is joined with: what goes between two items, between the earlier ones of
// more, and before the last of more
fn list_joints(options: &ListOptions) -> (String, String, String) {
    let language = options
        .locale
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_ascii_lowercase();
    let narrow = options.style == "narrow";
    if options.kind == "unit" {
        return match (language.as_str(), narrow) {
            ("en", true) => (" ".into(), " ".into(), " ".into()),
            _ => (", ".into(), ", ".into(), ", ".into()),
        };
    }
    let disjunction = options.kind == "disjunction";
    let word = match (language.as_str(), disjunction) {
        ("en", false) if options.style == "short" => "&",
        ("en", false) => "and",
        ("en", true) => "or",
        ("de", false) => "und",
        ("de", true) => "oder",
        ("fr", false) => "et",
        ("fr", true) => "ou",
        ("es", false) => "y",
        ("es", true) => "o",
        ("it", false) => "e",
        ("it", true) => "o",
        ("nl", false) => "en",
        ("nl", true) => "of",
        ("pt", false) => "e",
        ("pt", true) => "ou",
        ("pl", false) => "i",
        ("pl", true) => "lub",
        ("uk", false) => "\u{456}",
        ("uk", true) => "\u{430}\u{431}\u{43e}",
        ("ru", false) => "\u{438}",
        ("ru", true) => "\u{438}\u{43b}\u{438}",
        (_, false) => "and",
        (_, true) => "or",
    };
    if language == "en" && narrow && !disjunction {
        return (", ".into(), ", ".into(), ", ".into());
    }
    let pair = format!(" {word} ");
    // english puts a comma before the last item of three or more
    let last = if language == "en" {
        format!(", {word} ")
    } else {
        pair.clone()
    };
    (pair, ", ".into(), last)
}

// the list as pieces of text: each item and the joints between them
fn list_parts(options: &ListOptions, items: &[String]) -> Vec<(&'static str, String)> {
    let (pair, middle, last) = list_joints(options);
    let mut parts = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            let joint = match (items.len(), index == items.len() - 1) {
                (2, _) => &pair,
                (_, true) => &last,
                _ => &middle,
            };
            parts.push(("literal", joint.clone()));
        }
        parts.push(("element", item.clone()));
    }
    parts
}

#[derive(Clone, Debug)]
pub(super) enum IntlMethod {
    NumberFormatConstructor,
    DateTimeFormatConstructor,
    CollatorConstructor,
    PluralRulesConstructor,
    RelativeTimeFormatConstructor,
    ListFormatConstructor,
    ListFormat,
    ListFormatToParts,
    FormatDateToParts,
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
    List(Rc<ListOptions>),
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
    pub(super) fn locale_argument(&mut self, value: &Value) -> Result<String, ScriptError> {
        Ok(match value {
            Value::Undefined => self.locale.clone(),
            Value::String(text) => text.to_string(),
            Value::Array(list) => match list.borrow().first() {
                Some(Value::String(text)) => text.to_string(),
                _ => self.locale.clone(),
            },
            other => self.coerce_string(other.clone())?,
        })
    }

    pub(super) fn option_text(
        &mut self,
        options: &Value,
        key: &str,
    ) -> Result<Option<String>, ScriptError> {
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
        let numeric: &[&str] = &["numeric", "2-digit"];
        let text_or_numeric: &[&str] = &["numeric", "2-digit", "narrow", "short", "long"];
        let styles: &[&str] = &["full", "long", "medium", "short"];
        result.weekday = self.date_option(options, "weekday", &["narrow", "short", "long"])?;
        result.year = self.date_option(options, "year", numeric)?;
        result.month = self.date_option(options, "month", text_or_numeric)?;
        result.day = self.date_option(options, "day", numeric)?;
        result.hour = self.date_option(options, "hour", numeric)?;
        result.minute = self.date_option(options, "minute", numeric)?;
        result.second = self.date_option(options, "second", numeric)?;
        result.time_zone_name = self.date_option(
            options,
            "timeZoneName",
            &[
                "short",
                "long",
                "shortOffset",
                "longOffset",
                "shortGeneric",
                "longGeneric",
            ],
        )?;
        result.date_style = self.date_option(options, "dateStyle", styles)?;
        result.time_style = self.date_option(options, "timeStyle", styles)?;
        if !is_primitive(options) {
            result.hour12 = match self.get_property(options, "hour12")? {
                Value::Undefined => None,
                value => Some(value.truthy()),
            };
            result.hour_cycle =
                self.date_option(options, "hourCycle", &["h11", "h12", "h23", "h24"])?;
            result.fractional = match self.get_property(options, "fractionalSecondDigits")? {
                Value::Undefined => None,
                value => {
                    let digits = self.coerce_number(value)?;
                    if !(1.0..=3.0).contains(&digits.floor()) {
                        return Err(
                            self.range_error("fractionalSecondDigits value is out of range.")
                        );
                    }
                    Some(digits.floor() as u8)
                }
            };
            if let Some(zone) = self.option_text(options, "timeZone")? {
                result.zone = Some(match canonical_zone(&zone) {
                    Some(zone) => zone,
                    None => {
                        return Err(
                            self.range_error(format!("Invalid time zone specified: {zone}"))
                        );
                    }
                });
            }
        }
        let components = result.weekday.is_some()
            || result.year.is_some()
            || result.month.is_some()
            || result.day.is_some()
            || result.hour.is_some()
            || result.minute.is_some()
            || result.second.is_some()
            || result.time_zone_name.is_some()
            || result.fractional.is_some();
        if components && (result.date_style.is_some() || result.time_style.is_some()) {
            return Err(self.type_error(
                "Can't set option dateStyle or timeStyle together with date and time components",
            ));
        }
        Ok(result)
    }

    // a date option, refused unless it is one of the values the option takes
    fn date_option(
        &mut self,
        options: &Value,
        key: &str,
        allowed: &[&str],
    ) -> Result<Option<String>, ScriptError> {
        match self.option_text(options, key)? {
            Some(value) if !allowed.contains(&value.as_str()) => Err(self.range_error(format!(
                "Value {value} out of range for Intl.DateTimeFormat options property {key}"
            ))),
            value => Ok(value),
        }
    }

    // a date's time value in the formatter's zone
    fn zoned(&self, options: &DateOptions, time: f64) -> f64 {
        match &options.zone {
            Some(zone) => time + f64::from(zone_offset(zone, time)) * 1000.0,
            None => time + f64::from(self.time_zone.offset_minutes(time)) * 60_000.0,
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
        let (local, offset, zone) = self.zone_view(options, time);
        Ok(date_patterns::format(
            options,
            local,
            offset,
            zone.as_deref(),
        ))
    }

    // the wall clock time a formatter shows for a time value, its offset in seconds and the
    // zone it is in
    fn zone_view(&self, options: &DateOptions, time: f64) -> (f64, i64, Option<String>) {
        let local = self.zoned(options, time);
        let offset = ((local - time) / 1000.0).round() as i64;
        let zone = match &options.zone {
            Some(zone) => Some(zone.clone()),
            None => self.time_zone.name(time),
        };
        (local, offset, zone)
    }

    fn format_date_parts(
        &mut self,
        options: &DateOptions,
        date: &Value,
    ) -> Result<Value, ScriptError> {
        let time = match date {
            Value::Undefined => self.now_milliseconds(),
            other => self.coerce_number(other.clone())?,
        };
        if time.is_nan() {
            return Err(self.range_error("Invalid time value"));
        }
        let (local, offset, zone) = self.zone_view(options, time);
        let parts = date_patterns::format_parts(options, local, offset, zone.as_deref())
            .into_iter()
            .map(|part| {
                self.object_from_entries(vec![
                    ("type".to_string(), Value::String(part.kind.into())),
                    ("value".to_string(), Value::String(part.value.into())),
                ])
            })
            .collect();
        Ok(Self::new_array(parts))
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
            M::ListFormatConstructor => {
                let locale = self.locale_argument(&first)?;
                let kind = self
                    .option_text(&second, "type")?
                    .unwrap_or_else(|| "conjunction".to_string());
                let style = self
                    .option_text(&second, "style")?
                    .unwrap_or_else(|| "long".to_string());
                if !matches!(kind.as_str(), "conjunction" | "disjunction" | "unit") {
                    return Err(self.range_error(format!(
                        "Value {kind} out of range for Intl.ListFormat options property type"
                    )));
                }
                if !matches!(style.as_str(), "long" | "short" | "narrow") {
                    return Err(self.range_error(format!(
                        "Value {style} out of range for Intl.ListFormat options property style"
                    )));
                }
                let prototype = self.intrinsics.intl_prototypes[5].clone();
                Ok(self.intl_object(
                    IntlState::List(Rc::new(ListOptions {
                        locale,
                        kind,
                        style,
                    })),
                    prototype,
                ))
            }
            M::ListFormat | M::ListFormatToParts => {
                let state = self.intl_state(&this)?;
                let IntlState::List(options) = &*state else {
                    return Err(self.type_error("format is not available on this object"));
                };
                let options = options.clone();
                let values = match &first {
                    Value::Undefined => Vec::new(),
                    Value::Array(list) => list.borrow().to_vec(),
                    _ => {
                        return Err(
                            self.type_error("Iterable yielded a value which is not a string")
                        );
                    }
                };
                let mut items = Vec::new();
                for value in values {
                    let Value::String(text) = value.resolve() else {
                        return Err(
                            self.type_error("Iterable yielded a value which is not a string")
                        );
                    };
                    items.push(text.to_string());
                }
                let parts = list_parts(&options, &items);
                if matches!(method, M::ListFormat) {
                    let joined: String = parts.into_iter().map(|(_, text)| text).collect();
                    return Ok(Value::String(joined.into()));
                }
                let objects = parts
                    .into_iter()
                    .map(|(kind, text)| {
                        self.object_from_entries(vec![
                            ("type".to_string(), Value::String(kind.into())),
                            ("value".to_string(), Value::String(text.into())),
                        ])
                    })
                    .collect();
                Ok(Self::new_array(objects))
            }
            M::GetCanonicalLocales | M::SupportedLocalesOf => {
                let list = match &first {
                    Value::Undefined => Vec::new(),
                    Value::Array(list) => list.borrow().to_vec(),
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
            M::FormatDateToParts => {
                let state = self.intl_state(&this)?;
                let IntlState::Date(options) = &*state else {
                    return Err(self.type_error("formatToParts is not available on this object"));
                };
                let options = options.clone();
                self.format_date_parts(&options, &first)
            }
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
                    IntlState::Date(options) => {
                        let mut entries = vec![
                            (
                                "locale".to_string(),
                                Value::String(options.locale.as_str().into()),
                            ),
                            ("calendar".to_string(), Value::String("gregory".into())),
                            ("numberingSystem".to_string(), Value::String("latn".into())),
                            (
                                "timeZone".to_string(),
                                Value::String(match &options.zone {
                                    Some(zone) => zone.as_str().into(),
                                    None => self
                                        .time_zone
                                        .name(0.0)
                                        .unwrap_or_else(|| "UTC".to_string())
                                        .into(),
                                }),
                            ),
                        ];
                        if options.hour.is_some() || options.time_style.is_some() {
                            let cycle = date_patterns::hour_cycle(options);
                            entries.push(("hourCycle".to_string(), Value::String(cycle.into())));
                            entries.push((
                                "hour12".to_string(),
                                Value::Bool(matches!(cycle, "h11" | "h12")),
                            ));
                        }
                        let defaulted = options.is_empty();
                        let fields = [
                            ("weekday", &options.weekday),
                            ("year", &options.year),
                            ("month", &options.month),
                            ("day", &options.day),
                            ("hour", &options.hour),
                            ("minute", &options.minute),
                            ("second", &options.second),
                            ("timeZoneName", &options.time_zone_name),
                            ("dateStyle", &options.date_style),
                            ("timeStyle", &options.time_style),
                        ];
                        for (name, value) in fields {
                            let shown = match value {
                                Some(value) => Some(value.clone()),
                                None if defaulted && matches!(name, "year" | "month" | "day") => {
                                    Some("numeric".to_string())
                                }
                                None => None,
                            };
                            if let Some(shown) = shown {
                                entries.push((name.to_string(), Value::String(shown.into())));
                            }
                        }
                        if let Some(digits) = options.fractional {
                            entries.push((
                                "fractionalSecondDigits".to_string(),
                                Value::Number(f64::from(digits)),
                            ));
                        }
                        entries
                    }
                    IntlState::List(options) => vec![
                        (
                            "locale".to_string(),
                            Value::String(options.locale.as_str().into()),
                        ),
                        (
                            "type".to_string(),
                            Value::String(options.kind.as_str().into()),
                        ),
                        (
                            "style".to_string(),
                            Value::String(options.style.as_str().into()),
                        ),
                    ],
                    _ => vec![(
                        "locale".to_string(),
                        Value::String(self.locale.as_str().into()),
                    )],
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
    fn time_zones_resolve_and_offset_as_chrome_reports_them() {
        let resolved: Vec<Option<String>> = [
            "asia/kolkata",
            "US/Eastern",
            "Etc/GMT",
            "utc",
            "-0800",
            "+05:30",
            "Mars/Olympus",
            "+25:00",
        ]
        .into_iter()
        .map(canonical_zone)
        .collect();
        assert_eq!(
            resolved,
            [
                Some("Asia/Calcutta".to_string()),
                Some("America/New_York".to_string()),
                Some("UTC".to_string()),
                Some("UTC".to_string()),
                Some("-08:00".to_string()),
                Some("+05:30".to_string()),
                None,
                None,
            ]
        );
        let summer = 1_720_107_000_000.0;
        let winter = 1_705_332_600_000.0;
        assert_eq!(zone_offset("America/New_York", summer), -4 * 3600);
        assert_eq!(zone_offset("America/New_York", winter), -5 * 3600);
        assert_eq!(zone_offset("Asia/Calcutta", winter), 19_800);
        assert_eq!(zone_offset("-08:00", winter), -8 * 3600);
        assert_eq!(zone_offset("America/St_Johns", summer), -9000);
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
        let format_date = |options: &DateOptions, local: f64| {
            date_patterns::format(options, local, 0, Some("UTC"))
        };
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
        let padded = DateOptions {
            locale: "en-US".to_string(),
            hour: Some("2-digit".into()),
            minute: Some("2-digit".into()),
            ..DateOptions::default()
        };
        assert_eq!(format_date(&padded, local - 28_800_000.0), "07:04 AM");
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
