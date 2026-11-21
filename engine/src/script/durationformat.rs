// Intl.DurationFormat in English: each unit's style and display resolved as ECMA-402 orders
// them, numeric hours, minutes and seconds joined with colons, fractional units folded into
// the unit before them, and the parts joined as an English unit list

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DurationMember {
    Constructor,
    Format,
    FormatToParts,
    ResolvedOptions,
}

const UNITS: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

// (long singular, long plural, short singular, short plural, narrow)
const NAMES: [(&str, &str, &str, &str, &str); 10] = [
    ("year", "years", "yr", "yrs", "y"),
    ("month", "months", "mth", "mths", "m"),
    ("week", "weeks", "wk", "wks", "w"),
    ("day", "days", "day", "days", "d"),
    ("hour", "hours", "hr", "hr", "h"),
    ("minute", "minutes", "min", "min", "m"),
    ("second", "seconds", "sec", "sec", "s"),
    ("millisecond", "milliseconds", "ms", "ms", "ms"),
    (
        "microsecond",
        "microseconds",
        "\u{3bc}s",
        "\u{3bc}s",
        "\u{3bc}s",
    ),
    ("nanosecond", "nanoseconds", "ns", "ns", "ns"),
];

#[derive(Debug)]
pub(super) struct DurationOptions {
    style: &'static str,
    // each unit's style and whether it shows when zero
    units: [(&'static str, &'static str); 10],
    fractional_digits: Option<u32>,
}

pub(super) struct DurationInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    intl: &ObjectHandle,
) -> DurationInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("format", DurationMember::Format, 1),
        ("formatToParts", DurationMember::FormatToParts, 1),
        ("resolvedOptions", DurationMember::ResolvedOptions, 0),
    ] {
        let function = build.function(Native::DurationFormat(member), name, length);
        prototype
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Intl.DurationFormat".into()))
        },
    );
    let constructor = build.constructor(
        Native::DurationFormat(DurationMember::Constructor),
        "DurationFormat",
        0,
        &prototype,
    );
    let supported = build.function(
        Native::Intl(Rc::new(super::intl::IntlMethod::SupportedLocalesOf)),
        "supportedLocalesOf",
        1,
    );
    constructor.borrow_mut().define(
        "supportedLocalesOf".to_string(),
        Property::hidden(Value::Object(supported)),
    );
    intl.borrow_mut().define(
        "DurationFormat".to_string(),
        Property::hidden(Value::Object(constructor)),
    );
    DurationInterfaces { prototype }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

// a piece of the formatted text, with the unit a number belongs to
struct Piece {
    kind: &'static str,
    value: String,
    unit: Option<&'static str>,
}

impl ScriptRuntime {
    fn duration_options(value: &Value) -> Option<Rc<DurationOptions>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::DurationFormat(options) => Some(options.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn duration_choice(
        &mut self,
        options: &Value,
        name: &str,
        allowed: &[&'static str],
    ) -> Result<Option<&'static str>, ScriptError> {
        match self.option_text(options, name)? {
            None => Ok(None),
            Some(given) => match allowed.iter().find(|choice| **choice == given) {
                Some(choice) => Ok(Some(choice)),
                None => Err(self.range_error(format!(
                    "Value {given} out of range for Intl.DurationFormat options property {name}"
                ))),
            },
        }
    }

    pub(super) fn construct_duration_format(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let locales = arguments.first().cloned().unwrap_or(Value::Undefined);
        self.locale_argument(&locales)?;
        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        if !matches!(options, Value::Undefined) && is_primitive(&options) {
            return Err(self.type_error("Options must be an object"));
        }
        self.duration_choice(&options, "localeMatcher", &["lookup", "best fit"])?;
        if let Some(system) = self.option_text(&options, "numberingSystem")?
            && !(3..=8).contains(&system.len())
        {
            return Err(self.range_error("Invalid numberingSystem"));
        }
        let style = self
            .duration_choice(&options, "style", &["long", "short", "narrow", "digital"])?
            .unwrap_or("short");
        let mut units = [("", ""); 10];
        let mut previous: Option<&'static str> = None;
        for (index, unit) in UNITS.into_iter().enumerate() {
            let (styles, digital): (&[&'static str], &'static str) = match index {
                0..=3 => (&["long", "short", "narrow"], "short"),
                4..=6 => (
                    &["long", "short", "narrow", "numeric", "2-digit"],
                    "numeric",
                ),
                _ => (&["long", "short", "narrow", "numeric"], "numeric"),
            };
            let given = self.duration_choice(&options, unit, styles)?;
            let mut display_default = "always";
            let mut unit_style = match given {
                Some(given) => given,
                None if style == "digital" => {
                    if !(4..=6).contains(&index) {
                        display_default = "auto";
                    }
                    digital
                }
                None if matches!(previous, Some("fractional" | "numeric" | "2-digit")) => {
                    if !matches!(unit, "minutes" | "seconds") {
                        display_default = "auto";
                    }
                    "numeric"
                }
                None => {
                    display_default = "auto";
                    style
                }
            };
            if unit_style == "numeric" && index >= 7 {
                unit_style = "fractional";
                display_default = "auto";
            }
            let display_name = format!("{unit}Display");
            let display = self
                .duration_choice(&options, &display_name, &["auto", "always"])?
                .unwrap_or(display_default);
            match previous {
                Some("fractional") if unit_style != "fractional" => {
                    return Err(self.range_error(format!("{unit} cannot follow a fractional unit")));
                }
                Some("numeric" | "2-digit") => {
                    if !matches!(unit_style, "numeric" | "2-digit" | "fractional") {
                        return Err(self
                            .range_error(format!("{unit} must be numeric after a numeric unit")));
                    }
                    if matches!(unit, "minutes" | "seconds") {
                        unit_style = "2-digit";
                    }
                }
                _ => {}
            }
            units[index] = (unit_style, display);
            previous = Some(unit_style);
        }
        let fractional_digits = if is_primitive(&options) {
            None
        } else {
            match self.get_property(&options, "fractionalDigits")? {
                Value::Undefined => None,
                value => {
                    let digits = self.coerce_number(value)?;
                    if !(0.0..=9.0).contains(&digits) || digits.is_nan() {
                        return Err(self.range_error("fractionalDigits value is out of range."));
                    }
                    Some(digits.floor() as u32)
                }
            }
        };
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.duration_formats.prototype.clone(),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::DurationFormat(Rc::new(DurationOptions {
                style,
                units,
                fractional_digits,
            })),
        ))))
    }

    // ToDurationRecord: an object of whole numbers that all lean the same way
    fn duration_record(&mut self, value: Value) -> Result<[f64; 10], ScriptError> {
        if is_primitive(&value) {
            return Err(self.type_error("Invalid duration value"));
        }
        let mut record = [0.0; 10];
        let mut any = false;
        for (index, unit) in UNITS.into_iter().enumerate() {
            let field = self.get_property(&value, unit)?;
            if matches!(field, Value::Undefined) {
                continue;
            }
            any = true;
            let number = self.coerce_number(field)?;
            if !number.is_finite() || number.fract() != 0.0 {
                return Err(self.range_error(format!("Invalid duration value for {unit}")));
            }
            record[index] = number;
        }
        if !any {
            return Err(self.type_error("A duration needs at least one unit"));
        }
        if record.iter().any(|value| *value < 0.0) && record.iter().any(|value| *value > 0.0) {
            return Err(self.range_error("A duration's units must all have the same sign"));
        }
        Ok(record)
    }

    fn duration_pieces(&self, options: &DurationOptions, record: [f64; 10]) -> Vec<Vec<Piece>> {
        let negative = record.iter().any(|value| *value < 0.0);
        let mut signed = false;
        let mut groups: Vec<Vec<Piece>> = Vec::new();
        let mut clock: Vec<Piece> = Vec::new();
        let mut index = 0;
        while index < UNITS.len() {
            let (style, display) = options.units[index];
            let unit = UNITS[index];
            let mut value = record[index].abs();
            // fractional units below fold into this one as its decimals
            let folds = index < 9 && options.units[index + 1].0 == "fractional";
            let mut fraction = String::new();
            if folds {
                let mut scaled: f64 = 0.0;
                let mut scale = 1.0;
                for (below, amount) in record.iter().enumerate().skip(index + 1) {
                    if options.units[below].0 != "fractional" {
                        break;
                    }
                    scale *= 1000.0;
                    scaled += amount.abs() / scale;
                }
                value += scaled.trunc();
                let remainder = scaled.fract();
                let digits = options.fractional_digits;
                let mut text = format!("{:.9}", remainder);
                text = text.trim_start_matches('0').to_string();
                let mut decimals = text.trim_start_matches('.').to_string();
                match digits {
                    Some(digits) => decimals.truncate(digits as usize),
                    None => decimals = decimals.trim_end_matches('0').to_string(),
                }
                if let Some(digits) = digits {
                    while decimals.len() < digits as usize {
                        decimals.push('0');
                    }
                }
                fraction = decimals;
            }
            let numeric = matches!(style, "numeric" | "2-digit");
            let shown = value != 0.0 || !fraction.is_empty() || display == "always";
            if shown {
                let sign = if negative && !signed {
                    signed = true;
                    "-"
                } else {
                    ""
                };
                let whole = value as u64;
                if numeric {
                    let digits = if style == "2-digit" {
                        format!("{whole:02}")
                    } else {
                        whole.to_string()
                    };
                    if !clock.is_empty() {
                        clock.push(Piece {
                            kind: "literal",
                            value: ":".to_string(),
                            unit: None,
                        });
                    }
                    clock.push(Piece {
                        kind: "integer",
                        value: format!("{sign}{digits}"),
                        unit: Some(&unit[..unit.len() - 1]),
                    });
                    if !fraction.is_empty() {
                        clock.push(Piece {
                            kind: "decimal",
                            value: ".".to_string(),
                            unit: Some(&unit[..unit.len() - 1]),
                        });
                        clock.push(Piece {
                            kind: "fraction",
                            value: fraction.clone(),
                            unit: Some(&unit[..unit.len() - 1]),
                        });
                    }
                } else {
                    if !clock.is_empty() {
                        groups.push(std::mem::take(&mut clock));
                    }
                    let (long, longs, short, shorts, narrow) = NAMES[index];
                    let one = whole == 1 && fraction.is_empty();
                    let label = match style {
                        "long" => {
                            if one {
                                long
                            } else {
                                longs
                            }
                        }
                        "narrow" => narrow,
                        _ => {
                            if one {
                                short
                            } else {
                                shorts
                            }
                        }
                    };
                    let mut group = vec![Piece {
                        kind: "integer",
                        value: format!("{sign}{}", grouped(whole)),
                        unit: Some(&unit[..unit.len() - 1]),
                    }];
                    if !fraction.is_empty() {
                        group.push(Piece {
                            kind: "decimal",
                            value: ".".to_string(),
                            unit: Some(&unit[..unit.len() - 1]),
                        });
                        group.push(Piece {
                            kind: "fraction",
                            value: fraction.clone(),
                            unit: Some(&unit[..unit.len() - 1]),
                        });
                    }
                    group.push(Piece {
                        kind: "literal",
                        value: if style == "narrow" {
                            label.to_string()
                        } else {
                            format!(" {label}")
                        },
                        unit: None,
                    });
                    groups.push(group);
                }
            }
            if folds {
                // the fractional units below are spent
                while index + 1 < UNITS.len() && options.units[index + 1].0 == "fractional" {
                    index += 1;
                }
            }
            index += 1;
        }
        if !clock.is_empty() {
            groups.push(clock);
        }
        groups
    }

    pub(super) fn call_duration_format(
        &mut self,
        member: DurationMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member == DurationMember::Constructor {
            return Err(self.type_error("Constructor Intl.DurationFormat requires 'new'"));
        }
        let Some(options) = Self::duration_options(&this) else {
            return Err(self.type_error(
                "Method Intl.DurationFormat.prototype called on incompatible receiver",
            ));
        };
        let text = |text: &str| Value::String(text.to_string().into());
        if member == DurationMember::ResolvedOptions {
            let mut entries = vec![
                ("locale".to_string(), text("en")),
                ("numberingSystem".to_string(), text("latn")),
                ("style".to_string(), text(options.style)),
            ];
            for (index, unit) in UNITS.into_iter().enumerate() {
                let (style, display) = options.units[index];
                let style = if style == "fractional" {
                    "numeric"
                } else {
                    style
                };
                entries.push((unit.to_string(), text(style)));
                entries.push((format!("{unit}Display"), text(display)));
            }
            if let Some(digits) = options.fractional_digits {
                entries.push((
                    "fractionalDigits".to_string(),
                    Value::Number(f64::from(digits)),
                ));
            }
            return Ok(plain_object(entries));
        }
        let duration = arguments.first().cloned().unwrap_or(Value::Undefined);
        let record = self.duration_record(duration)?;
        let groups = self.duration_pieces(&options, record);
        // an English unit list: a space between narrow parts, a comma and space otherwise
        let separator = if options.style == "narrow" { " " } else { ", " };
        let mut pieces = Vec::new();
        for (index, group) in groups.into_iter().enumerate() {
            if index > 0 {
                pieces.push(Piece {
                    kind: "literal",
                    value: separator.to_string(),
                    unit: None,
                });
            }
            pieces.extend(group);
        }
        if member == DurationMember::Format {
            return Ok(Value::String(
                pieces
                    .iter()
                    .map(|piece| piece.value.as_str())
                    .collect::<String>()
                    .into(),
            ));
        }
        Ok(Self::new_array(
            pieces
                .into_iter()
                .map(|piece| {
                    let mut entries = vec![
                        ("type".to_string(), text(piece.kind)),
                        ("value".to_string(), Value::String(piece.value.into())),
                    ];
                    if let Some(unit) = piece.unit {
                        entries.push(("unit".to_string(), text(unit)));
                    }
                    plain_object(entries)
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_as_english_unit_lists() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const format = (options, duration) => new Intl.DurationFormat("en", options).format(duration);
                const time = { hours: 1, minutes: 2, seconds: 3 };
                const seen = [
                    format({}, time), format({ style: "long" }, time), format({ style: "narrow" }, time),
                    format({ style: "digital" }, time), format({ style: "digital" }, { minutes: 5 }),
                    format({ style: "long" }, { years: 2, days: 1 }), format({}, { hours: -1, minutes: -30 }),
                    format({ style: "digital", fractionalDigits: 2 }, { seconds: 3, milliseconds: 456 }),
                    format({ style: "digital" }, { days: 1, hours: 2, seconds: 5, milliseconds: 500 }),
                    format({ hours: "numeric" }, { hours: 1, minutes: 5 }),
                    format({ style: "long" }, { minutes: 1, seconds: 1 }),
                    new Intl.DurationFormat("en", { style: "digital" }).resolvedOptions().minutes,
                    new Intl.DurationFormat().formatToParts({ hours: 2 }).map((part) => part.type + ":" + part.value).join(),
                ];
                for (make of [() => format({}, { hours: 1.5 }), () => format({}, { hours: 1, minutes: -1 }), () => format({}, {}),
                              () => new Intl.DurationFormat("en", { hours: "numeric", minutes: "long" }), () => Intl.DurationFormat()]) {
                    try { make(); } catch (error) { seen.push(error.name); }
                }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "1 hr, 2 min, 3 sec|1 hour, 2 minutes, 3 seconds|1h 2m 3s|1:02:03|0:05:00|\
                 2 years, 1 day|-1 hr, 30 min|0:00:03.45|1 day, 2:00:05.5|1:05:00|1 minute, 1 second|2-digit|\
                 integer:2,literal: hr|RangeError|RangeError|TypeError|RangeError|TypeError"
            )
        );
    }
}
