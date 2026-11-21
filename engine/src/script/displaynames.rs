// Intl.DisplayNames in English, the one language ceri's Intl speaks, from the CLDR names in
// display_names_data.rs

use super::display_names_data::*;
use super::locale::LocaleTag;
use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DisplayMember {
    Constructor,
    Of,
    ResolvedOptions,
}

#[derive(Debug)]
pub(super) struct DisplayOptions {
    style: &'static str,
    kind: &'static str,
    fallback: &'static str,
    language_display: &'static str,
}

pub(super) struct DisplayInterfaces {
    prototype: ObjectHandle,
}

const STYLES: [&str; 3] = ["narrow", "short", "long"];
const KINDS: [&str; 6] = [
    "language",
    "region",
    "script",
    "currency",
    "calendar",
    "dateTimeField",
];

// (field, long, short, narrow)
const FIELDS: [(&str, &str, &str, &str); 12] = [
    ("era", "era", "era", "era"),
    ("year", "year", "yr.", "yr"),
    ("quarter", "quarter", "qtr.", "qtr"),
    ("month", "month", "mo.", "mo"),
    ("weekOfYear", "week", "wk.", "wk"),
    ("weekday", "day of the week", "day of wk.", "day of wk."),
    ("day", "day", "day", "day"),
    ("dayPeriod", "AM/PM", "AM/PM", "AM/PM"),
    ("hour", "hour", "hr.", "hr"),
    ("minute", "minute", "min.", "min"),
    ("second", "second", "sec.", "sec"),
    ("timeZoneName", "time zone", "zone", "zone"),
];

fn find(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(held, _)| *held == key)
        .map(|(_, name)| *name)
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    intl: &ObjectHandle,
) -> DisplayInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("of", DisplayMember::Of, 1),
        ("resolvedOptions", DisplayMember::ResolvedOptions, 0),
    ] {
        let function = build.function(Native::DisplayNames(member), name, length);
        prototype
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Intl.DisplayNames".into()))
        },
    );
    let constructor = build.constructor(
        Native::DisplayNames(DisplayMember::Constructor),
        "DisplayNames",
        2,
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
        "DisplayNames".to_string(),
        Property::hidden(Value::Object(constructor)),
    );
    DisplayInterfaces { prototype }
}

// the English name for a language tag: a dialect name when CLDR has one for the tag or a part
// of it, otherwise the language with its script, region and variants in parentheses
fn language_name(tag: &LocaleTag, dialect: bool) -> Option<String> {
    let base = tag.base_name();
    let mut parts: Vec<&str> = base.split('-').collect();
    let language = parts.remove(0);
    let mut name = None;
    let mut rest = parts.clone();
    if dialect {
        // the longest leading part of the tag that has a dialect name
        for take in (1..=parts.len()).rev() {
            let candidate = std::iter::once(language)
                .chain(parts[..take].iter().copied())
                .collect::<Vec<_>>()
                .join("-");
            if let Some(found) = find(DIALECT_NAMES, &candidate) {
                name = Some(found.to_string());
                rest = parts[take..].to_vec();
                break;
            }
        }
    }
    let name = match name {
        Some(name) => name,
        None => find(LANGUAGE_NAMES, language)?.to_string(),
    };
    let details: Vec<String> = rest
        .iter()
        .map(|part| {
            if part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                find(SCRIPT_NAMES, part).unwrap_or(part).to_string()
            } else if part.len() <= 3 {
                find(REGION_NAMES, part).unwrap_or(part).to_string()
            } else {
                part.to_string()
            }
        })
        .collect();
    Some(if details.is_empty() {
        name
    } else {
        format!("{name} ({})", details.join(", "))
    })
}

impl ScriptRuntime {
    fn display_options(value: &Value) -> Option<Rc<DisplayOptions>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::DisplayNames(options) => Some(options.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn display_choice(
        &mut self,
        options: &Value,
        name: &str,
        allowed: &[&'static str],
        default: Option<&'static str>,
    ) -> Result<&'static str, ScriptError> {
        match self.option_text(options, name)? {
            None => match default {
                Some(default) => Ok(default),
                None => Err(self.type_error(format!(
                    "Required option '{name}' is missing for Intl.DisplayNames"
                ))),
            },
            Some(given) => match allowed.iter().find(|choice| **choice == given) {
                Some(choice) => Ok(choice),
                None => Err(self.range_error(format!(
                    "Value {given} out of range for Intl.DisplayNames options property {name}"
                ))),
            },
        }
    }

    pub(super) fn construct_display_names(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if matches!(new_target, Value::Undefined) {
            return Err(self.type_error("Constructor Intl.DisplayNames requires 'new'"));
        }
        let locales = arguments.first().cloned().unwrap_or(Value::Undefined);
        self.locale_argument(&locales)?;
        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        if is_primitive(&options) {
            return Err(self.type_error("Intl.DisplayNames needs an options object with a type"));
        }
        self.display_choice(
            &options,
            "localeMatcher",
            &["lookup", "best fit"],
            Some("best fit"),
        )?;
        let style = self.display_choice(&options, "style", &STYLES, Some("long"))?;
        let kind = self.display_choice(&options, "type", &KINDS, None)?;
        let fallback =
            self.display_choice(&options, "fallback", &["code", "none"], Some("code"))?;
        let language_display = self.display_choice(
            &options,
            "languageDisplay",
            &["dialect", "standard"],
            Some("dialect"),
        )?;
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.display_names.prototype.clone(),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::DisplayNames(Rc::new(DisplayOptions {
                style,
                kind,
                fallback,
                language_display,
            })),
        ))))
    }

    pub(super) fn call_display_names(
        &mut self,
        member: DisplayMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member == DisplayMember::Constructor {
            return Err(self.type_error("Constructor Intl.DisplayNames requires 'new'"));
        }
        let Some(options) = Self::display_options(&this) else {
            return Err(self
                .type_error("Method Intl.DisplayNames.prototype called on incompatible receiver"));
        };
        let text = |text: &str| Value::String(text.to_string().into());
        if member == DisplayMember::ResolvedOptions {
            let mut entries = vec![
                ("locale".to_string(), text("en")),
                ("style".to_string(), text(options.style)),
                ("type".to_string(), text(options.kind)),
                ("fallback".to_string(), text(options.fallback)),
            ];
            if options.kind == "language" {
                entries.push((
                    "languageDisplay".to_string(),
                    text(options.language_display),
                ));
            }
            return Ok(plain_object(entries));
        }
        let code = arguments.first().cloned().unwrap_or(Value::Undefined);
        let code = self.coerce_string(code)?;
        let invalid = |runtime: &mut Self| runtime.range_error(format!("invalid_argument: {code}"));
        let all = |predicate: fn(u8) -> bool| code.bytes().all(predicate);
        // the code checked and in canonical case, and the name when CLDR has one
        let (canonical, name): (String, Option<String>) = match options.kind {
            "language" => {
                let tag = LocaleTag::parse(&code)
                    .filter(|tag| tag.tag() == tag.base_name() && !code.starts_with("root"));
                let Some(tag) = tag else {
                    return Err(invalid(self));
                };
                let name = language_name(&tag, options.language_display == "dialect");
                (tag.base_name(), name)
            }
            "region" => {
                let valid = (code.len() == 2 && all(|byte| byte.is_ascii_alphabetic()))
                    || (code.len() == 3 && all(|byte| byte.is_ascii_digit()));
                if !valid {
                    return Err(invalid(self));
                }
                let code = code.to_ascii_uppercase();
                let name = find(REGION_NAMES, &code).map(str::to_string);
                (code, name)
            }
            "script" => {
                if code.len() != 4 || !all(|byte| byte.is_ascii_alphabetic()) {
                    return Err(invalid(self));
                }
                let mut code = code.to_ascii_lowercase();
                code[..1].make_ascii_uppercase();
                let name = find(SCRIPT_NAMES, &code).map(str::to_string);
                (code, name)
            }
            "currency" => {
                if code.len() != 3 || !all(|byte| byte.is_ascii_alphabetic()) {
                    return Err(invalid(self));
                }
                let code = code.to_ascii_uppercase();
                let name = find(CURRENCY_NAMES, &code).map(str::to_string);
                (code, name)
            }
            "calendar" => {
                let valid = !code.is_empty()
                    && code.split('-').all(|part| {
                        (3..=8).contains(&part.len())
                            && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    });
                if !valid {
                    return Err(invalid(self));
                }
                let code = code.to_ascii_lowercase();
                let lookup = if code == "gregorian" {
                    "gregory"
                } else {
                    code.as_str()
                };
                let name = find(CALENDAR_NAMES, lookup).map(str::to_string);
                (code, name)
            }
            _ => {
                let Some(&(_, long, short, narrow)) =
                    FIELDS.iter().find(|(field, _, _, _)| *field == code)
                else {
                    return Err(invalid(self));
                };
                let name = match options.style {
                    "short" => short,
                    "narrow" => narrow,
                    _ => long,
                };
                (code.clone(), Some(name.to_string()))
            }
        };
        Ok(match (name, options.fallback) {
            (Some(name), _) => Value::String(name.into()),
            (None, "code") => Value::String(canonical.into()),
            (None, _) => Value::Undefined,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_name_codes_in_english() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const names = (type, extra) => new Intl.DisplayNames("en", { type, ...extra });
                const language = names("language");
                const standard = names("language", { languageDisplay: "standard" });
                const seen = [
                    language.of("en-US"), standard.of("en-US"), language.of("fr"), language.of("zh-Hant-HK"),
                    language.of("de-CH"), standard.of("sr-Latn"), names("region").of("de"), names("region").of("419"),
                    names("script").of("cyrl"), names("currency").of("eur"), names("calendar").of("gregory"),
                    names("dateTimeField").of("weekday"), names("dateTimeField", { style: "short" }).of("hour"),
                    names("region").of("XA"), names("region", { fallback: "none" }).of("XA"),
                    language.resolvedOptions().languageDisplay, new Intl.DisplayNames("de", { type: "region" }).resolvedOptions().locale,
                ];
                for (make of [() => new Intl.DisplayNames("en"), () => names("region").of("USA"),
                              () => names("unknown"), () => language.of("en-u-ca-gregory")]) {
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
                "American English|English (United States)|French|Traditional Chinese (Hong Kong SAR)|\
                 Swiss High German|Serbian (Latin)|Germany|Latin America|Cyrillic|Euro|Gregorian Calendar|\
                 day of the week|hr.|XA||dialect|en|TypeError|RangeError|RangeError|RangeError"
            )
        );
    }
}
