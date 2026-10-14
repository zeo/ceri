// the console object: printf-style substitution, value inspection in the style of a text
// console, groups, counters and timers; output lands in the runtime log

use super::object::{ObjectClass, is_private_key};
use super::operations::{is_primitive, symbol_description};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConsoleMethod {
    Log,
    Info,
    Debug,
    Warn,
    Error,
    Trace,
    Dir,
    Table,
    Group,
    GroupEnd,
    Time,
    TimeLog,
    TimeEnd,
    Count,
    CountReset,
    Assert,
    Clear,
}

pub(super) const CONSOLE_METHODS: &[(&str, ConsoleMethod)] = &[
    ("log", ConsoleMethod::Log),
    ("info", ConsoleMethod::Info),
    ("debug", ConsoleMethod::Debug),
    ("warn", ConsoleMethod::Warn),
    ("error", ConsoleMethod::Error),
    ("trace", ConsoleMethod::Trace),
    ("dir", ConsoleMethod::Dir),
    ("dirxml", ConsoleMethod::Dir),
    ("table", ConsoleMethod::Table),
    ("group", ConsoleMethod::Group),
    ("groupCollapsed", ConsoleMethod::Group),
    ("groupEnd", ConsoleMethod::GroupEnd),
    ("time", ConsoleMethod::Time),
    ("timeLog", ConsoleMethod::TimeLog),
    ("timeEnd", ConsoleMethod::TimeEnd),
    ("count", ConsoleMethod::Count),
    ("countReset", ConsoleMethod::CountReset),
    ("assert", ConsoleMethod::Assert),
    ("clear", ConsoleMethod::Clear),
];

const INSPECT_DEPTH: usize = 2;
const INSPECT_ITEMS: usize = 100;

fn quote(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push(quote);
    for character in text.chars() {
        match character {
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\\' => quoted.push_str("\\\\"),
            character if character == quote => {
                quoted.push('\\');
                quoted.push(character);
            }
            character => quoted.push(character),
        }
    }
    quoted.push(quote);
    quoted
}

fn display_key(key: &str) -> String {
    if super::object::is_symbol_key(key) {
        return format!("[Symbol({})]", symbol_description(key).unwrap_or_default());
    }
    let identifier = key
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && key
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '$');
    if identifier {
        key.to_string()
    } else {
        quote(key)
    }
}

impl ScriptRuntime {
    // a readable form of any value; strings inside collections are quoted
    pub(super) fn inspect(&mut self, value: &Value, depth: usize, seen: &mut Vec<usize>) -> String {
        match value {
            Value::String(text) if depth > 0 => quote(text),
            Value::Number(number) if *number == 0.0 && number.is_sign_negative() => {
                "-0".to_string()
            }
            Value::Symbol(key) => {
                format!("Symbol({})", symbol_description(key).unwrap_or_default())
            }
            Value::BigInt(value) => format!("{}n", value.to_string_radix(10)),
            value if is_primitive(value) => value.string_value(),
            Value::Binding(cell, _) => {
                let inner = cell.borrow().clone();
                self.inspect(&inner, depth, seen)
            }
            Value::Array(array) => {
                let address = Rc::as_ptr(array) as usize;
                if seen.contains(&address) {
                    return "[Circular]".to_string();
                }
                if depth > INSPECT_DEPTH {
                    return "[Array]".to_string();
                }
                seen.push(address);
                let items = array.borrow().clone();
                let mut parts: Vec<String> = items
                    .iter()
                    .take(INSPECT_ITEMS)
                    .map(|item| self.inspect(item, depth + 1, seen))
                    .collect();
                if items.len() > INSPECT_ITEMS {
                    parts.push(format!("... {} more items", items.len() - INSPECT_ITEMS));
                }
                seen.pop();
                if parts.is_empty() {
                    "[]".to_string()
                } else {
                    format!("[ {} ]", parts.join(", "))
                }
            }
            Value::Object(object) => {
                let address = Rc::as_ptr(object) as usize;
                if seen.contains(&address) {
                    return "[Circular]".to_string();
                }
                let class = object.borrow().class.clone();
                match class {
                    ObjectClass::Function(_)
                    | ObjectClass::Bound { .. }
                    | ObjectClass::Native(_) => {
                        let name = match self.get_property(value, "name") {
                            Ok(Value::String(name)) if !name.is_empty() => name.to_string(),
                            _ => "(anonymous)".to_string(),
                        };
                        let is_class = matches!(&class, ObjectClass::Function(closure) if matches!(closure.code.kind, ast::FunctionKind::ClassConstructor | ast::FunctionKind::DerivedConstructor));
                        return if is_class {
                            format!("[class {name}]")
                        } else {
                            format!("[Function: {name}]")
                        };
                    }
                    ObjectClass::Error => {
                        self.pending_exception = None;
                        let text = self
                            .coerce_string(value.clone())
                            .unwrap_or_else(|_| "Error".to_string());
                        self.pending_exception = None;
                        return text;
                    }
                    ObjectClass::RegExp(_) => {
                        let text = self.coerce_string(value.clone()).unwrap_or_default();
                        self.pending_exception = None;
                        return text;
                    }
                    ObjectClass::Boolean(_) | ObjectClass::Number(_) | ObjectClass::String(_) => {
                        return format!("[{}]", value.string_value());
                    }
                    ObjectClass::Date(time) => {
                        return if time.is_nan() {
                            "Invalid Date".to_string()
                        } else {
                            super::dates::iso_string(time)
                        };
                    }
                    ObjectClass::Map(collection) | ObjectClass::Set(collection) => {
                        let set = matches!(object.borrow().class, ObjectClass::Set(_));
                        if depth > INSPECT_DEPTH {
                            return if set {
                                "[Set]".to_string()
                            } else {
                                "[Map]".to_string()
                            };
                        }
                        seen.push(address);
                        let entries = collection.borrow().values();
                        let parts: Vec<String> = entries
                            .iter()
                            .take(INSPECT_ITEMS)
                            .map(|(key, value)| {
                                if set {
                                    self.inspect(key, depth + 1, seen)
                                } else {
                                    format!(
                                        "{} => {}",
                                        self.inspect(key, depth + 1, seen),
                                        self.inspect(value, depth + 1, seen)
                                    )
                                }
                            })
                            .collect();
                        seen.pop();
                        let name = if set { "Set" } else { "Map" };
                        return if parts.is_empty() {
                            format!("{name}(0) {{}}")
                        } else {
                            format!("{name}({}) {{ {} }}", entries.len(), parts.join(", "))
                        };
                    }
                    _ => {}
                }
                if depth > INSPECT_DEPTH {
                    return "[Object]".to_string();
                }
                seen.push(address);
                let entries: Vec<(String, Option<Value>)> = {
                    let borrowed = object.borrow();
                    borrowed
                        .ordered_keys()
                        .into_iter()
                        .filter(|key| !is_private_key(key))
                        .filter_map(|key| {
                            let property = borrowed.property(&key)?;
                            property
                                .enumerable
                                .then(|| (key.clone(), property.value().cloned()))
                        })
                        .collect()
                };
                let mut parts = Vec::new();
                for (key, value) in entries.iter().take(INSPECT_ITEMS) {
                    let shown = match value {
                        Some(value) => self.inspect(value, depth + 1, seen),
                        None => "[Getter/Setter]".to_string(),
                    };
                    parts.push(format!("{}: {shown}", display_key(key)));
                }
                if entries.len() > INSPECT_ITEMS {
                    parts.push(format!("... {} more items", entries.len() - INSPECT_ITEMS));
                }
                seen.pop();
                let prefix = match class {
                    ObjectClass::Arguments => "[Arguments] ",
                    _ => "",
                };
                if parts.is_empty() {
                    format!("{prefix}{{}}")
                } else {
                    format!("{prefix}{{ {} }}", parts.join(", "))
                }
            }
            Value::Promise(promise) => {
                let state = match &promise.borrow().outcome {
                    PromiseOutcome::Pending => "<pending>".to_string(),
                    PromiseOutcome::Fulfilled(value) => value.string_value(),
                    PromiseOutcome::Rejected(reason) => {
                        format!("<rejected> {}", reason.string_value())
                    }
                };
                format!("Promise {{ {state} }}")
            }
            Value::Builtin(_) => "[Function (native)]".to_string(),
            other => other.string_value(),
        }
    }

    // console arguments joined as a console prints them, with %s %d %i %f %o %O %c applied
    pub(super) fn console_text(&mut self, arguments: &[Value]) -> String {
        let mut parts = Vec::new();
        let mut rest = arguments;
        if let Some(Value::String(format)) = arguments.first()
            && format.contains('%')
        {
            let mut output = String::new();
            let mut remaining = arguments[1..].iter();
            let mut characters = format.chars().peekable();
            while let Some(character) = characters.next() {
                if character != '%' {
                    output.push(character);
                    continue;
                }
                let Some(&specifier) = characters.peek() else {
                    output.push('%');
                    break;
                };
                if specifier == '%' {
                    characters.next();
                    output.push('%');
                    continue;
                }
                if !matches!(specifier, 's' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c') {
                    output.push('%');
                    continue;
                }
                characters.next();
                let Some(value) = remaining.next() else {
                    output.push('%');
                    output.push(specifier);
                    continue;
                };
                match specifier {
                    's' => match value {
                        Value::String(text) => output.push_str(text),
                        other => output.push_str(&self.inspect(other, 1, &mut Vec::new())),
                    },
                    'd' | 'i' => {
                        let number = match value {
                            Value::Number(number) => *number,
                            other => super::operations::string_to_number(&other.string_value()),
                        };
                        output.push_str(&number_to_string(number.trunc()));
                    }
                    'f' => {
                        let number = match value {
                            Value::Number(number) => *number,
                            other => super::numbers::parse_float(&other.string_value()),
                        };
                        output.push_str(&number_to_string(number));
                    }
                    'c' => {}
                    _ => output.push_str(&self.inspect(value, 1, &mut Vec::new())),
                }
            }
            parts.push(output);
            rest = remaining.as_slice();
        }
        for value in rest {
            parts.push(self.inspect(value, 0, &mut Vec::new()));
        }
        parts.join(" ")
    }

    fn console_write(&mut self, text: String) {
        let indent = "  ".repeat(self.console_depth);
        for line in text.split('\n') {
            self.logs.push(format!("{indent}{line}"));
        }
    }

    pub(super) fn call_console(
        &mut self,
        method: ConsoleMethod,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use ConsoleMethod as M;
        let label = || -> String {
            match arguments.first() {
                Some(Value::Undefined) | None => "default".to_string(),
                Some(value) => value.string_value(),
            }
        };
        match method {
            M::Log | M::Info | M::Debug | M::Warn | M::Error | M::Dir => {
                let text = self.console_text(&arguments);
                self.console_write(text);
            }
            M::Trace => {
                let text = self.console_text(&arguments);
                self.console_write(if text.is_empty() {
                    "Trace".to_string()
                } else {
                    format!("Trace: {text}")
                });
            }
            M::Table => {
                let text = self.console_text(&arguments);
                self.console_write(text);
            }
            M::Group => {
                if !arguments.is_empty() {
                    let text = self.console_text(&arguments);
                    self.console_write(text);
                }
                self.console_depth += 1;
            }
            M::GroupEnd => self.console_depth = self.console_depth.saturating_sub(1),
            M::Time => {
                let label = label();
                self.console_timers.insert(label, self.clock);
            }
            M::TimeLog | M::TimeEnd => {
                let label = label();
                let started = if method == M::TimeEnd {
                    self.console_timers.remove(&label)
                } else {
                    self.console_timers.get(&label).copied()
                };
                let text = match started {
                    Some(started) => format!("{label}: {}ms", self.clock.saturating_sub(started)),
                    None => format!("Timer '{label}' does not exist"),
                };
                self.console_write(text);
            }
            M::Count => {
                let label = label();
                let count = self.console_counts.entry(label.clone()).or_insert(0);
                *count += 1;
                let text = format!("{label}: {count}");
                self.console_write(text);
            }
            M::CountReset => {
                let label = label();
                self.console_counts.insert(label, 0);
            }
            M::Assert => {
                if !arguments.first().is_some_and(Value::truthy) {
                    let text = self.console_text(&arguments[arguments.len().min(1)..]);
                    self.console_write(if text.is_empty() {
                        "Assertion failed".to_string()
                    } else {
                        format!("Assertion failed: {text}")
                    });
                }
            }
            M::Clear => self.console_depth = 0,
        }
        Ok(Value::Undefined)
    }

    // an uncaught error from a task, reported to the console as browsers do
    pub(super) fn report_exception(&mut self, error: ScriptError) {
        let value = self.take_thrown(error);
        let text = self.inspect(&value, 0, &mut Vec::new());
        self.logs.push(format!("Uncaught {text}"));
    }
}
