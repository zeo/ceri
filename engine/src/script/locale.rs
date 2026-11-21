// Intl.Locale over BCP 47 tags, with likely subtags, week data, hour cycles and time zones from
// locale_data.rs, and Intl.supportedValuesOf answering what ceri's Intl formats

use super::locale_data::*;
use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LocaleMember {
    Constructor,
    // an index into GETTERS, which keeps natives small
    Getter(u8),
    ToString,
    Maximize,
    Minimize,
    Calendars,
    Collations,
    HourCycles,
    NumberingSystems,
    TimeZones,
    TextInfo,
    WeekInfo,
    SupportedValuesOf,
}

// a structurally valid tag in canonical case: the unicode extension's keywords sorted by key,
// other extensions and private use kept as written
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LocaleTag {
    language: String,
    script: Option<String>,
    region: Option<String>,
    variants: Vec<String>,
    attributes: Vec<String>,
    keywords: Vec<(String, String)>,
    others: Vec<(char, String)>,
    private: Option<String>,
}

const GETTERS: [&str; 10] = [
    "baseName",
    "calendar",
    "caseFirst",
    "collation",
    "hourCycle",
    "numeric",
    "numberingSystem",
    "language",
    "script",
    "region",
];

const KEYWORDS: [(&str, &str); 6] = [
    ("calendar", "ca"),
    ("collation", "co"),
    ("hourCycle", "hc"),
    ("caseFirst", "kf"),
    ("numeric", "kn"),
    ("numberingSystem", "nu"),
];

const RIGHT_TO_LEFT: [&str; 10] = [
    "Arab", "Hebr", "Thaa", "Syrc", "Nkoo", "Adlm", "Rohg", "Mand", "Samr", "Mend",
];

fn alpha(text: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn alphanumeric(text: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn is_language(text: &str) -> bool {
    alpha(text, 2..=3) || alpha(text, 5..=8)
}

fn is_script(text: &str) -> bool {
    alpha(text, 4..=4)
}

fn is_region(text: &str) -> bool {
    alpha(text, 2..=2) || (text.len() == 3 && text.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_variant(text: &str) -> bool {
    alphanumeric(text, 5..=8)
        || (text.len() == 4 && text.as_bytes()[0].is_ascii_digit() && alphanumeric(text, 4..=4))
}

// a unicode type value such as gregory or islamic-civil
fn is_type(text: &str) -> bool {
    !text.is_empty() && text.split('-').all(|part| alphanumeric(part, 3..=8))
}

fn title(text: &str) -> String {
    let mut lower = text.to_ascii_lowercase();
    if let Some(first) = lower.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    lower
}

impl LocaleTag {
    pub(super) fn parse(tag: &str) -> Option<LocaleTag> {
        let lower = tag.to_ascii_lowercase();
        let mut parts = lower.split('-').peekable();
        let language = parts.next().filter(|part| is_language(part))?.to_string();
        let mut locale = LocaleTag {
            language,
            script: None,
            region: None,
            variants: Vec::new(),
            attributes: Vec::new(),
            keywords: Vec::new(),
            others: Vec::new(),
            private: None,
        };
        if let Some(part) = parts.next_if(|part| is_script(part)) {
            locale.script = Some(title(part));
        }
        if let Some(part) = parts.next_if(|part| is_region(part)) {
            locale.region = Some(part.to_ascii_uppercase());
        }
        while let Some(part) = parts.next_if(|part| is_variant(part)) {
            if locale.variants.iter().any(|seen| seen == part) {
                return None;
            }
            locale.variants.push(part.to_string());
        }
        locale.variants.sort();
        let mut seen = Vec::new();
        while let Some(singleton) = parts.next() {
            if singleton.len() != 1 {
                return None;
            }
            let singleton = singleton.chars().next()?;
            if singleton == 'x' {
                let rest: Vec<&str> = parts.by_ref().collect();
                if rest.is_empty() || !rest.iter().all(|part| alphanumeric(part, 1..=8)) {
                    return None;
                }
                locale.private = Some(rest.join("-"));
                break;
            }
            if !singleton.is_ascii_alphanumeric() || seen.contains(&singleton) {
                return None;
            }
            seen.push(singleton);
            let mut body = Vec::new();
            while let Some(part) = parts.next_if(|part| part.len() > 1) {
                if !alphanumeric(part, 2..=8) {
                    return None;
                }
                body.push(part.to_string());
            }
            if body.is_empty() {
                return None;
            }
            if singleton == 'u' {
                let mut index = 0;
                while index < body.len() && body[index].len() >= 3 {
                    locale.attributes.push(body[index].clone());
                    index += 1;
                }
                while index < body.len() {
                    let key = body[index].clone();
                    let bytes = key.as_bytes();
                    if key.len() != 2 || !bytes[1].is_ascii_alphabetic() {
                        return None;
                    }
                    index += 1;
                    let mut value = Vec::new();
                    while index < body.len() && body[index].len() >= 3 {
                        value.push(body[index].clone());
                        index += 1;
                    }
                    if !locale.keywords.iter().any(|(held, _)| *held == key) {
                        locale.keywords.push((key, value.join("-")));
                    }
                }
            } else {
                locale.others.push((singleton, body.join("-")));
            }
        }
        // a keyword whose value is true says so by being there
        for (_, value) in &mut locale.keywords {
            if value == "true" {
                value.clear();
            }
        }
        locale.keywords.sort();
        locale.others.sort();
        Some(locale)
    }

    pub(super) fn base_name(&self) -> String {
        let mut name = self.language.clone();
        for part in self
            .script
            .iter()
            .chain(self.region.iter())
            .chain(self.variants.iter())
        {
            name.push('-');
            name.push_str(part);
        }
        name
    }

    fn keyword(&self, key: &str) -> Option<&str> {
        self.keywords
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.as_str())
    }

    fn set_keyword(&mut self, key: &str, value: String) {
        let value = if value == "true" {
            String::new()
        } else {
            value
        };
        self.keywords.retain(|(held, _)| held != key);
        self.keywords.push((key.to_string(), value));
        self.keywords.sort();
    }

    pub(super) fn tag(&self) -> String {
        let mut tag = self.base_name();
        let mut extensions = self.others.clone();
        if !self.attributes.is_empty() || !self.keywords.is_empty() {
            let mut body = self.attributes.clone();
            for (key, value) in &self.keywords {
                body.push(key.clone());
                if !value.is_empty() {
                    body.push(value.clone());
                }
            }
            extensions.push(('u', body.join("-")));
            extensions.sort();
        }
        for (singleton, body) in extensions {
            tag.push('-');
            tag.push(singleton);
            tag.push('-');
            tag.push_str(&body);
        }
        if let Some(private) = &self.private {
            tag.push_str("-x-");
            tag.push_str(private);
        }
        tag
    }

    // the likely script and region filled in, and a language for und
    pub(super) fn maximized(&self) -> LocaleTag {
        let mut locale = self.clone();
        if locale.language == "und" {
            let from_region = locale
                .region
                .as_deref()
                .and_then(|region| LIKELY_REGIONS.iter().find(|(held, _, _)| *held == region));
            let from_script = locale
                .script
                .as_deref()
                .and_then(|script| LIKELY_SCRIPTS.iter().find(|(held, _, _)| *held == script));
            match (from_region, from_script) {
                (Some((_, language, script)), _) => {
                    locale.language = language.to_string();
                    locale.script.get_or_insert_with(|| script.to_string());
                }
                (None, Some((_, language, region))) => {
                    locale.language = language.to_string();
                    locale.region.get_or_insert_with(|| region.to_string());
                }
                (None, None) => locale.language = "en".to_string(),
            }
        }
        let Some(&(_, script, region)) = LIKELY_LANGUAGES
            .iter()
            .find(|(held, _, _)| *held == locale.language)
        else {
            return locale;
        };
        if locale.script.is_none() {
            let special = locale.region.as_deref().and_then(|region| {
                LIKELY_LANGUAGE_REGIONS
                    .iter()
                    .find(|(language, held, _)| *language == locale.language && *held == region)
                    .map(|(_, _, script)| *script)
            });
            locale.script = Some(special.unwrap_or(script).to_string());
        }
        if locale.region.is_none() {
            let special = locale.script.as_deref().and_then(|script| {
                LIKELY_LANGUAGE_SCRIPTS
                    .iter()
                    .find(|(language, held, _)| *language == locale.language && *held == script)
                    .map(|(_, _, region)| *region)
            });
            locale.region = Some(special.unwrap_or(region).to_string());
        }
        locale
    }

    // the shortest tag that maximizes to the same thing, as CLDR's remove likely subtags
    pub(super) fn minimized(&self) -> LocaleTag {
        let maximal = self.maximized();
        let key = |locale: &LocaleTag| {
            (
                locale.language.clone(),
                locale.script.clone(),
                locale.region.clone(),
            )
        };
        let target = key(&maximal);
        for (script, region) in [
            (None, None),
            (None, maximal.region.clone()),
            (maximal.script.clone(), None),
        ] {
            let trial = LocaleTag {
                script,
                region,
                ..maximal.clone()
            };
            if key(&trial.maximized()) == target {
                return trial;
            }
        }
        maximal
    }
}

pub(super) struct LocaleInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    intl: &ObjectHandle,
) -> LocaleInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    for (index, name) in GETTERS.iter().enumerate() {
        let get = build.function(
            Native::Locale(LocaleMember::Getter(index as u8)),
            &format!("get {name}"),
            0,
        );
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(get)),
                    set: None,
                },
                enumerable: false,
                writable: false,
                configurable: true,
            },
        );
    }
    for (name, member) in [
        ("toString", LocaleMember::ToString),
        ("maximize", LocaleMember::Maximize),
        ("minimize", LocaleMember::Minimize),
        ("getCalendars", LocaleMember::Calendars),
        ("getCollations", LocaleMember::Collations),
        ("getHourCycles", LocaleMember::HourCycles),
        ("getNumberingSystems", LocaleMember::NumberingSystems),
        ("getTimeZones", LocaleMember::TimeZones),
        ("getTextInfo", LocaleMember::TextInfo),
        ("getWeekInfo", LocaleMember::WeekInfo),
    ] {
        let function = build.function(Native::Locale(member), name, 0);
        prototype
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::String("Intl.Locale".into()))
        },
    );
    let constructor = build.constructor(
        Native::Locale(LocaleMember::Constructor),
        "Locale",
        1,
        &prototype,
    );
    let supported = build.function(
        Native::Locale(LocaleMember::SupportedValuesOf),
        "supportedValuesOf",
        1,
    );
    let mut intl = intl.borrow_mut();
    intl.define(
        "Locale".to_string(),
        Property::hidden(Value::Object(constructor)),
    );
    intl.define(
        "supportedValuesOf".to_string(),
        Property::hidden(Value::Object(supported)),
    );
    LocaleInterfaces { prototype }
}

impl ScriptRuntime {
    fn locale_tag(value: &Value) -> Option<Rc<LocaleTag>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Locale(tag) => Some(tag.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn locale_object(&self, prototype: ObjectHandle, tag: LocaleTag) -> Value {
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Locale(Rc::new(tag)),
        )))
    }

    fn invalid_locale(&mut self) -> ScriptError {
        self.range_error("Incorrect locale information provided")
    }

    pub(super) fn construct_locale(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let given = arguments.first().cloned().unwrap_or(Value::Undefined);
        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        let text = match &given {
            Value::String(text) => text.to_string(),
            value @ Value::Object(_) => match Self::locale_tag(value) {
                Some(tag) => tag.tag(),
                None => self.coerce_string(value.clone())?,
            },
            _ => {
                return Err(self.type_error(
                    "First argument to Intl.Locale constructor can't be empty or missing",
                ));
            }
        };
        let Some(mut tag) = LocaleTag::parse(&text) else {
            return Err(self.invalid_locale());
        };
        if !matches!(options, Value::Undefined) && is_primitive(&options) {
            return Err(self.type_error("Options must be an object"));
        }
        if let Some(language) = self.option_text(&options, "language")? {
            if !is_language(&language) {
                return Err(self.invalid_locale());
            }
            tag.language = language.to_ascii_lowercase();
        }
        if let Some(script) = self.option_text(&options, "script")? {
            if !is_script(&script) {
                return Err(self.invalid_locale());
            }
            tag.script = Some(title(&script));
        }
        if let Some(region) = self.option_text(&options, "region")? {
            if !is_region(&region) {
                return Err(self.invalid_locale());
            }
            tag.region = Some(region.to_ascii_uppercase());
        }
        for (option, key) in KEYWORDS {
            let value = if option == "numeric" {
                if is_primitive(&options) {
                    None
                } else {
                    match self.get_property(&options, option)? {
                        Value::Undefined => None,
                        value => Some(value.truthy().to_string()),
                    }
                }
            } else {
                self.option_text(&options, option)?
            };
            let Some(value) = value else {
                continue;
            };
            let valid = match option {
                "hourCycle" => matches!(value.as_str(), "h11" | "h12" | "h23" | "h24"),
                "caseFirst" => matches!(value.as_str(), "upper" | "lower" | "false"),
                "numeric" => true,
                _ => is_type(&value),
            };
            if !valid {
                return Err(self.range_error(format!(
                    "Value {value} out of range for Intl.Locale options property {option}"
                )));
            }
            tag.set_keyword(key, value.to_ascii_lowercase());
        }
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.locales.prototype.clone(),
        };
        Ok(self.locale_object(prototype, tag))
    }

    pub(super) fn call_locale(
        &mut self,
        member: LocaleMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let text = |text: &str| Value::String(text.to_string().into());
        let list = |items: Vec<String>| {
            Self::new_array(
                items
                    .into_iter()
                    .map(|item| Value::String(item.into()))
                    .collect(),
            )
        };
        match member {
            LocaleMember::Constructor => {
                return Err(self.type_error("Constructor Intl.Locale requires 'new'"));
            }
            LocaleMember::SupportedValuesOf => {
                let key = arguments.first().cloned().unwrap_or(Value::Undefined);
                let key = self.coerce_string(key)?;
                // what ceri's Intl formats: the gregorian calendar, latin digits, UTC and local
                // time, and any currency code, with no collation types or units
                let values: Vec<String> = match key.as_str() {
                    "calendar" => vec!["gregory".to_string()],
                    "collation" | "unit" => Vec::new(),
                    "currency" => CURRENCIES.split(' ').map(str::to_string).collect(),
                    "numberingSystem" => vec!["latn".to_string()],
                    "timeZone" => vec!["UTC".to_string()],
                    _ => {
                        return Err(self.range_error(format!("Invalid key : {key}")));
                    }
                };
                return Ok(list(values));
            }
            _ => {}
        }
        let Some(tag) = Self::locale_tag(&this) else {
            return Err(
                self.type_error("Method Intl.Locale.prototype called on incompatible receiver")
            );
        };
        let prototype = self.intrinsics.locales.prototype.clone();
        Ok(match member {
            LocaleMember::Getter(index) => match GETTERS[usize::from(index)] {
                "baseName" => text(&tag.base_name()),
                "language" => text(&tag.language),
                "script" => tag.script.as_deref().map_or(Value::Undefined, text),
                "region" => tag.region.as_deref().map_or(Value::Undefined, text),
                "numeric" => Value::Bool(tag.keyword("kn").is_some_and(|value| value != "false")),
                name => {
                    let key = KEYWORDS
                        .iter()
                        .find(|(option, _)| *option == name)
                        .map_or("", |(_, key)| *key);
                    tag.keyword(key).map_or(Value::Undefined, text)
                }
            },
            LocaleMember::ToString => text(&tag.tag()),
            LocaleMember::Maximize => self.locale_object(prototype, tag.maximized()),
            LocaleMember::Minimize => self.locale_object(prototype, tag.minimized()),
            LocaleMember::Calendars => {
                list(vec![tag.keyword("ca").unwrap_or("gregory").to_string()])
            }
            LocaleMember::Collations => {
                list(tag.keyword("co").map(str::to_string).into_iter().collect())
            }
            LocaleMember::NumberingSystems => {
                list(vec![tag.keyword("nu").unwrap_or("latn").to_string()])
            }
            LocaleMember::HourCycles => {
                let region = tag.maximized().region.unwrap_or_default();
                let cycle = tag.keyword("hc").unwrap_or_else(|| {
                    HOUR_CYCLES
                        .iter()
                        .find(|(held, _)| *held == region)
                        .map_or("h23", |(_, cycle)| cycle)
                });
                list(vec![cycle.to_string()])
            }
            LocaleMember::TimeZones => match &tag.region {
                None => Value::Undefined,
                Some(region) => list(
                    REGION_ZONES
                        .iter()
                        .find(|(held, _)| held == region)
                        .map(|(_, zones)| zones.split(' ').map(str::to_string).collect())
                        .unwrap_or_default(),
                ),
            },
            LocaleMember::TextInfo => {
                let script = tag.maximized().script.unwrap_or_default();
                let direction = if RIGHT_TO_LEFT.contains(&script.as_str()) {
                    "rtl"
                } else {
                    "ltr"
                };
                plain_object([("direction".to_string(), text(direction))])
            }
            LocaleMember::WeekInfo => {
                let region = tag.maximized().region.unwrap_or_default();
                let (_, first, minimal, weekend) = WEEK_DATA
                    .iter()
                    .find(|(held, _, _, _)| *held == region)
                    .or_else(|| WEEK_DATA.iter().find(|(held, _, _, _)| *held == "001"))
                    .copied()
                    .unwrap_or(("001", 1, 1, 96));
                let first = match tag.keyword("fw") {
                    Some(day) => ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                        .iter()
                        .position(|name| *name == day)
                        .map_or(first, |index| index as u8 + 1),
                    None => first,
                };
                let weekend = (1..=7u8)
                    .filter(|day| weekend & (1 << (day - 1)) != 0)
                    .map(|day| Value::Number(f64::from(day)))
                    .collect();
                plain_object([
                    ("firstDay".to_string(), Value::Number(f64::from(first))),
                    ("weekend".to_string(), Self::new_array(weekend)),
                    ("minimalDays".to_string(), Value::Number(f64::from(minimal))),
                ])
            }
            LocaleMember::Constructor | LocaleMember::SupportedValuesOf => {
                unreachable!("answered above")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_canonicalize_maximize_and_minimize_as_cldr_has_them() {
        let tag = |text: &str| LocaleTag::parse(text).expect("the tag should parse");
        assert_eq!(
            tag("EN-latn-us-u-NU-arab-ca-gregory").tag(),
            "en-Latn-US-u-ca-gregory-nu-arab"
        );
        assert_eq!(tag("de-u-kn-true").tag(), "de-u-kn");
        assert!(LocaleTag::parse("en-").is_none());
        assert!(LocaleTag::parse("e").is_none());
        assert!(LocaleTag::parse("en-u-ca-gregory-u-nu-latn").is_none());
        assert_eq!(tag("en").maximized().tag(), "en-Latn-US");
        assert_eq!(tag("zh-TW").maximized().tag(), "zh-Hant-TW");
        assert_eq!(tag("zh-Hant").maximized().tag(), "zh-Hant-TW");
        assert_eq!(tag("und-JP").maximized().tag(), "ja-Jpan-JP");
        assert_eq!(tag("sr").maximized().tag(), "sr-Cyrl-RS");
        assert_eq!(tag("en-Latn-US").minimized().tag(), "en");
        assert_eq!(tag("zh-Hant-TW").minimized().tag(), "zh-TW");
        assert_eq!(tag("en-GB").minimized().tag(), "en-GB");
    }

    #[test]
    fn locales_answer_their_parts_and_preferences() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const us = new Intl.Locale("en-US", { hourCycle: "h12", numeric: true });
                const seen = [us.toString(), us.baseName, us.region, us.script, us.numeric, us.hourCycle];
                seen.push(new Intl.Locale("en").maximize().toString(), new Intl.Locale("en-Latn-US").minimize().toString());
                const week = new Intl.Locale("en-US").getWeekInfo();
                seen.push(week.firstDay, week.weekend.join(), new Intl.Locale("de").getWeekInfo().minimalDays);
                seen.push(new Intl.Locale("ar").getTextInfo().direction, new Intl.Locale("en-GB").getHourCycles().join());
                seen.push(new Intl.Locale("en-IN").getTimeZones().join(), new Intl.Locale("en").getTimeZones());
                seen.push(Intl.supportedValuesOf("calendar").join(), Intl.supportedValuesOf("currency").includes("EUR"));
                for (make of [() => new Intl.Locale("not a tag"), () => Intl.Locale("en"), () => Intl.supportedValuesOf("x"),
                              () => new Intl.Locale("en", { hourCycle: "h13" })]) {
                    try { make(); } catch (error) { seen.push(error.name); }
                }
                seen.push(Object.prototype.toString.call(us), new Intl.Locale(us).toString());
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "en-US-u-hc-h12-kn|en-US|US||true|h12|en-Latn-US|en|7|6,7|4|rtl|h23|Asia/Kolkata||\
                 gregory|true|RangeError|TypeError|RangeError|RangeError|[object Intl.Locale]|en-US-u-hc-h12-kn"
            )
        );
    }
}
