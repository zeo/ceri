// URL and URLSearchParams over the WHATWG parser, as classes pages can extend

use super::object::{
    JsObject, ObjectClass, ObjectHandle, Property, Slot, is_symbol_key, new_object,
};
use super::realm::{Builder, Native};
use super::*;
use crate::weburl::{Url, percent_decode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UrlMember {
    Href,
    SetHref,
    Origin,
    Protocol,
    SetProtocol,
    Username,
    SetUsername,
    Password,
    SetPassword,
    Host,
    SetHost,
    Hostname,
    SetHostname,
    Port,
    SetPort,
    Pathname,
    SetPathname,
    Search,
    SetSearch,
    SearchParams,
    Hash,
    SetHash,
    ToString,
    CanParse,
    Parse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamsMember {
    Append,
    Delete,
    Get,
    GetAll,
    Has,
    Set,
    Sort,
    ToString,
    ForEach,
    Keys,
    Values,
    Entries,
    Size,
}

const URL_ACCESSORS: &[(&str, UrlMember, Option<UrlMember>)] = &[
    ("href", UrlMember::Href, Some(UrlMember::SetHref)),
    ("origin", UrlMember::Origin, None),
    (
        "protocol",
        UrlMember::Protocol,
        Some(UrlMember::SetProtocol),
    ),
    (
        "username",
        UrlMember::Username,
        Some(UrlMember::SetUsername),
    ),
    (
        "password",
        UrlMember::Password,
        Some(UrlMember::SetPassword),
    ),
    ("host", UrlMember::Host, Some(UrlMember::SetHost)),
    (
        "hostname",
        UrlMember::Hostname,
        Some(UrlMember::SetHostname),
    ),
    ("port", UrlMember::Port, Some(UrlMember::SetPort)),
    (
        "pathname",
        UrlMember::Pathname,
        Some(UrlMember::SetPathname),
    ),
    ("search", UrlMember::Search, Some(UrlMember::SetSearch)),
    ("searchParams", UrlMember::SearchParams, None),
    ("hash", UrlMember::Hash, Some(UrlMember::SetHash)),
];

const PARAMS_METHODS: &[(&str, ParamsMember, u32)] = &[
    ("append", ParamsMember::Append, 2),
    ("delete", ParamsMember::Delete, 1),
    ("get", ParamsMember::Get, 1),
    ("getAll", ParamsMember::GetAll, 1),
    ("has", ParamsMember::Has, 1),
    ("set", ParamsMember::Set, 2),
    ("sort", ParamsMember::Sort, 0),
    ("toString", ParamsMember::ToString, 0),
    ("forEach", ParamsMember::ForEach, 1),
    ("keys", ParamsMember::Keys, 0),
    ("values", ParamsMember::Values, 0),
    ("entries", ParamsMember::Entries, 0),
];

pub(super) struct UrlInterfaces {
    url: ObjectHandle,
    params: ObjectHandle,
}

pub(super) struct UrlState {
    pub(super) url: Url,
    params: Rc<RefCell<ParamsState>>,
    search_params: ObjectHandle,
}

pub(super) struct ParamsState {
    pub(super) pairs: Vec<(String, String)>,
    // the URL whose query this list mirrors, for the searchParams of a URL
    url: Option<Weak<RefCell<UrlState>>>,
}

fn interface_member(target: &ObjectHandle, name: &str, slot: Slot) {
    target.borrow_mut().define(
        name.to_string(),
        Property {
            writable: matches!(slot, Slot::Data(_)),
            slot,
            enumerable: true,
            configurable: true,
        },
    );
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> UrlInterfaces {
    let plain = || {
        new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let url = plain();
    for &(name, getter, setter) in URL_ACCESSORS {
        let get = Value::Object(build.function(Native::Url(getter), &format!("get {name}"), 0));
        let set = setter.map(|setter| {
            Value::Object(build.function(Native::Url(setter), &format!("set {name}"), 1))
        });
        interface_member(
            &url,
            name,
            Slot::Accessor {
                get: Some(get),
                set,
            },
        );
    }
    for name in ["toString", "toJSON"] {
        let function = build.function(Native::Url(UrlMember::ToString), name, 0);
        interface_member(&url, name, Slot::Data(Value::Object(function)));
    }
    url.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("URL".into())),
    );
    let url_constructor = build.constructor(Native::UrlConstructor, "URL", 1, &url);
    for (name, member) in [
        ("canParse", UrlMember::CanParse),
        ("parse", UrlMember::Parse),
    ] {
        let function = build.function(Native::Url(member), name, 1);
        interface_member(&url_constructor, name, Slot::Data(Value::Object(function)));
    }
    globals.push(("URL", Value::Object(url_constructor)));

    let params = plain();
    for &(name, member, length) in PARAMS_METHODS {
        let function = build.function(Native::UrlSearchParams(member), name, length);
        interface_member(&params, name, Slot::Data(Value::Object(function)));
    }
    let size = build.function(Native::UrlSearchParams(ParamsMember::Size), "get size", 0);
    interface_member(
        &params,
        "size",
        Slot::Accessor {
            get: Some(Value::Object(size)),
            set: None,
        },
    );
    let entries = params.borrow().get("entries").cloned();
    if let Some(entries) = entries {
        params
            .borrow_mut()
            .define("@@iterator".to_string(), Property::hidden(entries));
    }
    params.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("URLSearchParams".into())),
    );
    let params_constructor = build.constructor(
        Native::UrlSearchParamsConstructor,
        "URLSearchParams",
        0,
        &params,
    );
    globals.push(("URLSearchParams", Value::Object(params_constructor)));
    UrlInterfaces { url, params }
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

// the application/x-www-form-urlencoded parser, which never fails
pub(super) fn parse_form_urlencoded(input: &str) -> Vec<(String, String)> {
    let decode =
        |text: &str| String::from_utf8_lossy(&percent_decode(&text.replace('+', " "))).into_owned();
    input
        .split('&')
        .filter(|sequence| !sequence.is_empty())
        .map(|sequence| {
            let (name, value) = sequence.split_once('=').unwrap_or((sequence, ""));
            (decode(name), decode(value))
        })
        .collect()
}

// the href of a URL object and the serialized list of a URLSearchParams one
pub(super) fn display(class: &ObjectClass) -> Option<String> {
    match class {
        ObjectClass::Url(state) => Some(state.borrow().url.to_string()),
        ObjectClass::UrlSearchParams(params) => {
            Some(serialize_form_urlencoded(&params.borrow().pairs))
        }
        _ => None,
    }
}

pub(super) fn search_params_pairs(value: &Value) -> Option<Vec<(String, String)>> {
    match value {
        Value::Object(object) => match &object.borrow().class {
            ObjectClass::UrlSearchParams(params) => Some(params.borrow().pairs.clone()),
            _ => None,
        },
        _ => None,
    }
}

impl ScriptRuntime {
    fn new_url(&mut self, url: Url, prototype: ObjectHandle) -> Value {
        let params = Rc::new(RefCell::new(ParamsState {
            pairs: parse_form_urlencoded(url.query().unwrap_or_default()),
            url: None,
        }));
        let search_params = new_object(JsObject::with_prototype(
            Some(self.intrinsics.urls.params.clone()),
            ObjectClass::UrlSearchParams(params.clone()),
        ));
        let state = Rc::new(RefCell::new(UrlState {
            url,
            params: params.clone(),
            search_params,
        }));
        params.borrow_mut().url = Some(Rc::downgrade(&state));
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Url(state),
        )))
    }

    // the URL a script names and the base it resolves against, as the constructor reads them
    fn url_arguments(&mut self, arguments: &[Value]) -> Result<Option<Url>, ScriptError> {
        let input = self.coerce_string(argument(arguments, 0))?;
        let base = match argument(arguments, 1) {
            Value::Undefined => None,
            base => {
                let base = self.coerce_string(base)?;
                match Url::parse(&base, None) {
                    Some(base) => Some(base),
                    None => return Ok(None),
                }
            }
        };
        Ok(Url::parse(&input, base.as_ref()))
    }

    pub(super) fn construct_url(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if arguments.is_empty() {
            return Err(self.type_error(
                "Failed to construct 'URL': 1 argument required, but only 0 present.",
            ));
        }
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.urls.url.clone(),
        };
        let input = self.coerce_string(arguments[0].clone())?;
        match self.url_arguments(&arguments)? {
            Some(url) => Ok(self.new_url(url, prototype)),
            None => {
                Err(self.type_error(format!("Failed to construct 'URL': Invalid URL '{input}'")))
            }
        }
    }

    pub(super) fn construct_url_search_params(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.urls.params.clone(),
        };
        let init = argument(&arguments, 0);
        let pairs = if matches!(init, Value::Undefined) {
            Vec::new()
        } else if !super::operations::is_primitive(&init) {
            let iterator = self.get_property(&init, "@@iterator")?;
            if matches!(iterator, Value::Undefined | Value::Null) {
                let mut pairs = Vec::new();
                for (name, value) in self.own_enumerable_entries(&init)? {
                    if !is_symbol_key(&name) {
                        pairs.push((name, self.coerce_string(value)?));
                    }
                }
                pairs
            } else {
                let mut pairs = Vec::new();
                for pair in self.iterate_to_vec(init)? {
                    if super::operations::is_primitive(&pair) {
                        return Err(self.type_error(
                            "Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.",
                        ));
                    }
                    let pair = self.iterate_to_vec(pair)?;
                    let [name, value] = <[Value; 2]>::try_from(pair).map_err(|_| {
                        self.type_error(
                            "Failed to construct 'URLSearchParams': Sequence initializer must only contain pair elements",
                        )
                    })?;
                    pairs.push((self.coerce_string(name)?, self.coerce_string(value)?));
                }
                pairs
            }
        } else {
            let text = self.coerce_string(init)?;
            parse_form_urlencoded(text.strip_prefix('?').unwrap_or(&text))
        };
        let params = Rc::new(RefCell::new(ParamsState { pairs, url: None }));
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::UrlSearchParams(params),
        ))))
    }

    fn this_url(&mut self, this: &Value) -> Result<Rc<RefCell<UrlState>>, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::Url(state) = &object.borrow().class
        {
            return Ok(state.clone());
        }
        Err(self.type_error("Illegal invocation"))
    }

    fn this_params(&mut self, this: &Value) -> Result<Rc<RefCell<ParamsState>>, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::UrlSearchParams(params) = &object.borrow().class
        {
            return Ok(params.clone());
        }
        Err(self.type_error("Illegal invocation"))
    }

    pub(super) fn call_url(
        &mut self,
        member: UrlMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use UrlMember as M;
        match member {
            M::CanParse => return Ok(Value::Bool(self.url_arguments(&arguments)?.is_some())),
            M::Parse => {
                return Ok(match self.url_arguments(&arguments)? {
                    Some(url) => {
                        let prototype = self.intrinsics.urls.url.clone();
                        self.new_url(url, prototype)
                    }
                    None => Value::Null,
                });
            }
            _ => {}
        }
        let state = self.this_url(&this)?;
        let text = |text: String| Ok(Value::String(text.into()));
        let read = state.borrow();
        let url = &read.url;
        match member {
            M::Href | M::ToString => return text(url.to_string()),
            M::Origin => return text(url.origin()),
            M::Protocol => return text(format!("{}:", url.scheme())),
            M::Username => return text(url.username().to_string()),
            M::Password => return text(url.password().to_string()),
            M::Host => return text(url.host()),
            M::Hostname => return text(url.hostname().to_string()),
            M::Port => return text(url.port().map(|port| port.to_string()).unwrap_or_default()),
            M::Pathname => return text(url.pathname()),
            M::Search => return text(url.search()),
            M::Hash => return text(url.hash()),
            M::SearchParams => return Ok(Value::Object(read.search_params.clone())),
            _ => {}
        }
        drop(read);
        let value = self.coerce_string(argument(&arguments, 0))?;
        let mut state = state.borrow_mut();
        let url = &mut state.url;
        match member {
            M::SetHref => match Url::parse(&value, None) {
                Some(parsed) => *url = parsed,
                None => {
                    return Err(self.type_error(format!(
                        "Failed to set the 'href' property on 'URL': Invalid URL '{value}'"
                    )));
                }
            },
            M::SetProtocol => url.set_protocol(&value),
            M::SetUsername => url.set_username(&value),
            M::SetPassword => url.set_password(&value),
            M::SetHost => url.set_host(&value),
            M::SetHostname => url.set_hostname(&value),
            M::SetPort => url.set_port(&value),
            M::SetPathname => url.set_pathname(&value),
            M::SetSearch => url.set_search(&value),
            M::SetHash => url.set_hash(&value),
            _ => unreachable!("the getters returned above"),
        }
        if matches!(member, M::SetHref | M::SetSearch) {
            let pairs = parse_form_urlencoded(state.url.query().unwrap_or_default());
            state.params.borrow_mut().pairs = pairs;
        }
        Ok(Value::Undefined)
    }

    pub(super) fn call_url_search_params(
        &mut self,
        member: ParamsMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use ParamsMember as M;
        let params = self.this_params(&this)?;
        let required = match member {
            M::Append | M::Set => 2,
            M::Delete | M::Get | M::GetAll | M::Has | M::ForEach => 1,
            _ => 0,
        };
        if arguments.len() < required {
            return Err(self.type_error(format!(
                "Failed to execute '{}' on 'URLSearchParams': {required} argument{} required, but only {} present.",
                PARAMS_METHODS
                    .iter()
                    .find(|(_, candidate, _)| *candidate == member)
                    .map_or("", |(name, _, _)| *name),
                if required == 1 { "" } else { "s" },
                arguments.len()
            )));
        }
        let strings = |runtime: &mut Self, count: usize| -> Result<Vec<String>, ScriptError> {
            arguments
                .iter()
                .take(count)
                .map(|value| runtime.coerce_string(value.clone()))
                .collect()
        };
        // the second argument of delete and has narrows them to one value when it is given
        let optional_value = |runtime: &mut Self| -> Result<Option<String>, ScriptError> {
            match argument(&arguments, 1) {
                Value::Undefined => Ok(None),
                value => runtime.coerce_string(value).map(Some),
            }
        };
        let string = |text: &str| Value::String(text.to_string().into());
        let changed = match member {
            M::Append => {
                let [name, value] = <[String; 2]>::try_from(strings(self, 2)?).expect("two");
                params.borrow_mut().pairs.push((name, value));
                true
            }
            M::Delete => {
                let name = self.coerce_string(argument(&arguments, 0))?;
                let value = optional_value(self)?;
                params.borrow_mut().pairs.retain(|(key, entry)| {
                    key != &name || value.as_ref().is_some_and(|value| entry != value)
                });
                true
            }
            M::Set => {
                let [name, value] = <[String; 2]>::try_from(strings(self, 2)?).expect("two");
                let mut params = params.borrow_mut();
                match params.pairs.iter().position(|(key, _)| key == &name) {
                    Some(first) => {
                        params.pairs[first].1 = value;
                        let mut index = 0;
                        params.pairs.retain(|(key, _)| {
                            index += 1;
                            index - 1 <= first || key != &name
                        });
                    }
                    None => params.pairs.push((name, value)),
                }
                true
            }
            M::Sort => {
                params
                    .borrow_mut()
                    .pairs
                    .sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
                true
            }
            _ => false,
        };
        if changed {
            let url = params.borrow().url.as_ref().and_then(Weak::upgrade);
            if let Some(url) = url {
                let query = serialize_form_urlencoded(&params.borrow().pairs);
                url.borrow_mut()
                    .url
                    .set_query((!query.is_empty()).then_some(query));
            }
            return Ok(Value::Undefined);
        }
        match member {
            M::Get => {
                let name = self.coerce_string(argument(&arguments, 0))?;
                Ok(params
                    .borrow()
                    .pairs
                    .iter()
                    .find(|(key, _)| key == &name)
                    .map_or(Value::Null, |(_, value)| string(value)))
            }
            M::GetAll => {
                let name = self.coerce_string(argument(&arguments, 0))?;
                Ok(Self::new_array(
                    params
                        .borrow()
                        .pairs
                        .iter()
                        .filter(|(key, _)| key == &name)
                        .map(|(_, value)| string(value))
                        .collect(),
                ))
            }
            M::Has => {
                let name = self.coerce_string(argument(&arguments, 0))?;
                let value = optional_value(self)?;
                Ok(Value::Bool(params.borrow().pairs.iter().any(
                    |(key, entry)| {
                        key == &name && value.as_ref().is_none_or(|value| entry == value)
                    },
                )))
            }
            M::ToString => Ok(string(&serialize_form_urlencoded(&params.borrow().pairs))),
            M::Size => Ok(Value::Number(params.borrow().pairs.len() as f64)),
            M::ForEach => {
                let callback = argument(&arguments, 0);
                if !is_callable(&callback) {
                    return Err(self.type_error(
                        "Failed to execute 'forEach' on 'URLSearchParams': The callback provided as parameter 1 is not a function.",
                    ));
                }
                let this_argument = argument(&arguments, 1);
                let mut index = 0;
                // the list is read live, so entries the callback appends are visited too
                while let Some((name, value)) = params.borrow().pairs.get(index).cloned() {
                    self.call_function(
                        callback.clone(),
                        this_argument.clone(),
                        vec![string(&value), string(&name), this.clone()],
                    )?;
                    index += 1;
                }
                Ok(Value::Undefined)
            }
            M::Keys | M::Values | M::Entries => {
                let values = params
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(name, value)| match member {
                        M::Keys => string(name),
                        M::Values => string(value),
                        _ => Self::new_array(vec![string(name), string(value)]),
                    })
                    .collect();
                Ok(iterator_value(values))
            }
            _ => unreachable!("the mutators returned above"),
        }
    }
}
