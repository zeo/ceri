// PerformanceObserver over the marks and measures the page makes, delivered in a task of their
// own, and ReportingObserver, which ceri never has a report for

use super::geometry::PerformanceEntry;
use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_callable, is_primitive};
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

// the entry types ceri records
pub(super) const ENTRY_TYPES: [&str; 2] = ["mark", "measure"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObserverMember {
    Constructor,
    Observe,
    Disconnect,
    TakeRecords,
    ListEntries,
    ListByType,
    ListByName,
    ReportingConstructor,
    ReportingObserve,
    ReportingDisconnect,
    ReportingTakeRecords,
}

pub(super) struct PerfObserver {
    callback: Value,
    types: RefCell<Vec<&'static str>>,
    buffer: RefCell<Vec<PerformanceEntry>>,
    queued: Cell<bool>,
}

impl PerfObserver {
    pub(super) fn trace(self: &Rc<Self>, tracer: &mut super::heap::Tracer) {
        if !tracer.visit_rc(self) {
            return;
        }
        tracer.trace_value(&self.callback);
    }
}

pub(super) struct PerfInterfaces {
    observer: ObjectHandle,
    list: ObjectHandle,
    reporting: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> PerfInterfaces {
    let plain = |tag: &str| {
        let object = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        object.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(tag.into()))
            },
        );
        object
    };
    let method = |target: &ObjectHandle, name: &str, member: ObserverMember, length: u32| {
        let function = build.function(Native::PerfObserver(member), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    };
    let observer = plain("PerformanceObserver");
    method(&observer, "observe", ObserverMember::Observe, 0);
    method(&observer, "disconnect", ObserverMember::Disconnect, 0);
    method(&observer, "takeRecords", ObserverMember::TakeRecords, 0);
    let constructor = build.constructor(
        Native::PerfObserver(ObserverMember::Constructor),
        "PerformanceObserver",
        1,
        &observer,
    );
    let supported = ScriptRuntime::new_array(
        ENTRY_TYPES
            .iter()
            .map(|kind| Value::String((*kind).into()))
            .collect(),
    );
    constructor.borrow_mut().define(
        "supportedEntryTypes".to_string(),
        Property {
            writable: false,
            ..Property::hidden(supported)
        },
    );
    globals.push(("PerformanceObserver", Value::Object(constructor)));
    let list = plain("PerformanceObserverEntryList");
    method(&list, "getEntries", ObserverMember::ListEntries, 0);
    method(&list, "getEntriesByType", ObserverMember::ListByType, 1);
    method(&list, "getEntriesByName", ObserverMember::ListByName, 1);
    let list_constructor = build.constructor(
        Native::IllegalConstructor,
        "PerformanceObserverEntryList",
        0,
        &list,
    );
    globals.push((
        "PerformanceObserverEntryList",
        Value::Object(list_constructor),
    ));
    let reporting = plain("ReportingObserver");
    method(&reporting, "observe", ObserverMember::ReportingObserve, 0);
    method(
        &reporting,
        "disconnect",
        ObserverMember::ReportingDisconnect,
        0,
    );
    method(
        &reporting,
        "takeRecords",
        ObserverMember::ReportingTakeRecords,
        0,
    );
    let reporting_constructor = build.constructor(
        Native::PerfObserver(ObserverMember::ReportingConstructor),
        "ReportingObserver",
        1,
        &reporting,
    );
    globals.push(("ReportingObserver", Value::Object(reporting_constructor)));
    PerfInterfaces {
        observer,
        list,
        reporting,
    }
}

impl ScriptRuntime {
    fn perf_observer(value: &Value) -> Option<Rc<PerfObserver>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::PerfObserver(observer) => Some(observer.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn entry_list(&self, entries: Vec<PerformanceEntry>) -> Value {
        Value::Object(new_object(JsObject::with_prototype(
            Some(self.intrinsics.perf_observers.list.clone()),
            ObjectClass::PerfEntries(Rc::new(entries)),
        )))
    }

    // a new mark or measure goes to each observer watching its type, which hears of it in a
    // task once the current one is done
    pub(super) fn notify_perf_observers(&mut self, entry: &PerformanceEntry) {
        let observers: Vec<Rc<PerfObserver>> = self
            .perf_observers
            .iter()
            .map(|(_, observer)| observer.clone())
            .filter(|observer| observer.types.borrow().contains(&entry.kind()))
            .collect();
        for observer in observers {
            observer.buffer.borrow_mut().push(entry.clone());
            if !observer.queued.replace(true) {
                self.schedule_native_task(Native::PerfDeliver(observer));
            }
        }
    }

    pub(super) fn deliver_perf_entries(
        &mut self,
        observer: &Rc<PerfObserver>,
    ) -> Result<Value, ScriptError> {
        observer.queued.set(false);
        let entries = std::mem::take(&mut *observer.buffer.borrow_mut());
        let Some(object) = self
            .perf_observers
            .iter()
            .find(|(_, held)| Rc::ptr_eq(held, observer))
            .map(|(object, _)| object.clone())
        else {
            return Ok(Value::Undefined);
        };
        if entries.is_empty() {
            return Ok(Value::Undefined);
        }
        let list = self.entry_list(entries);
        let object = Value::Object(object);
        self.call_function(
            observer.callback.clone(),
            object.clone(),
            vec![list, object],
        )?;
        Ok(Value::Undefined)
    }

    pub(super) fn construct_perf_observer(
        &mut self,
        member: ObserverMember,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        let reporting = member == ObserverMember::ReportingConstructor;
        if !is_callable(&callback) {
            return Err(self.type_error(format!(
                "Failed to construct '{}': The callback provided as parameter 1 is not a function.",
                if reporting {
                    "ReportingObserver"
                } else {
                    "PerformanceObserver"
                }
            )));
        }
        let fallback = if reporting {
            self.intrinsics.perf_observers.reporting.clone()
        } else {
            self.intrinsics.perf_observers.observer.clone()
        };
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => fallback,
        };
        let class = if reporting {
            ObjectClass::Ordinary
        } else {
            ObjectClass::PerfObserver(Rc::new(PerfObserver {
                callback,
                types: RefCell::new(Vec::new()),
                buffer: RefCell::new(Vec::new()),
                queued: Cell::new(false),
            }))
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            class,
        ))))
    }

    fn entry_type(&mut self, value: Value) -> Result<Option<&'static str>, ScriptError> {
        let text = self.coerce_string(value)?;
        Ok(ENTRY_TYPES.iter().copied().find(|kind| *kind == text))
    }

    pub(super) fn call_perf_observer(
        &mut self,
        member: ObserverMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match member {
            ObserverMember::Constructor | ObserverMember::ReportingConstructor => {
                return Err(self.type_error("Constructor requires 'new'"));
            }
            // ceri makes no deprecation, intervention or crash reports
            ObserverMember::ReportingObserve | ObserverMember::ReportingDisconnect => {
                return Ok(Value::Undefined);
            }
            ObserverMember::ReportingTakeRecords => return Ok(Self::new_array(Vec::new())),
            ObserverMember::ListEntries
            | ObserverMember::ListByType
            | ObserverMember::ListByName => {
                let entries = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::PerfEntries(entries) => entries.clone(),
                        _ => return Err(self.type_error("Illegal invocation")),
                    },
                    _ => return Err(self.type_error("Illegal invocation")),
                };
                let wanted = match member {
                    ObserverMember::ListEntries => None,
                    _ => Some(self.coerce_string(first)?),
                };
                let kind = match arguments.get(1) {
                    Some(value)
                        if member == ObserverMember::ListByName
                            && !matches!(value, Value::Undefined) =>
                    {
                        Some(self.coerce_string(value.clone())?)
                    }
                    _ => None,
                };
                let picked: Vec<Value> = entries
                    .iter()
                    .filter(|entry| match (member, &wanted) {
                        (ObserverMember::ListByType, Some(wanted)) => entry.kind() == wanted,
                        (ObserverMember::ListByName, Some(wanted)) => {
                            entry.name() == wanted
                                && kind.as_deref().is_none_or(|kind| entry.kind() == kind)
                        }
                        _ => true,
                    })
                    .map(|entry| self.performance_entry(entry))
                    .collect();
                return Ok(Self::new_array(picked));
            }
            _ => {}
        }
        let Some(observer) = Self::perf_observer(&this) else {
            return Err(self.type_error("Illegal invocation"));
        };
        let Value::Object(object) = &this else {
            unreachable!("an observer is an object");
        };
        match member {
            ObserverMember::Observe => {
                if is_primitive(&first) {
                    return Err(self.type_error(
                        "Failed to execute 'observe' on 'PerformanceObserver': An observe() call must include either entryTypes or type arguments.",
                    ));
                }
                let entry_types = self.get_property(&first, "entryTypes")?;
                let single = self.get_property(&first, "type")?;
                let has_list = !matches!(entry_types, Value::Undefined);
                if has_list == !matches!(single, Value::Undefined) {
                    return Err(self.type_error(
                        "Failed to execute 'observe' on 'PerformanceObserver': An observe() call must include either entryTypes or type arguments, but not both.",
                    ));
                }
                let types = match has_list {
                    false => {
                        let kind = self.entry_type(single)?;
                        let buffered = self.get_property(&first, "buffered")?.truthy();
                        if let (Some(kind), true) = (kind, buffered) {
                            let earlier: Vec<PerformanceEntry> = self
                                .performance_entry_list()
                                .into_iter()
                                .filter(|entry| entry.kind() == kind)
                                .collect();
                            if !earlier.is_empty() {
                                observer.buffer.borrow_mut().extend(earlier);
                                if !observer.queued.replace(true) {
                                    self.schedule_native_task(Native::PerfDeliver(
                                        observer.clone(),
                                    ));
                                }
                            }
                        }
                        let mut types = observer.types.borrow().clone();
                        types.extend(kind.filter(|kind| !types.contains(kind)));
                        types
                    }
                    true => {
                        let mut types = Vec::new();
                        for value in self.iterate_to_vec(entry_types)? {
                            if let Some(kind) = self.entry_type(value)? {
                                types.push(kind);
                            }
                        }
                        types
                    }
                };
                *observer.types.borrow_mut() = types;
                if !self
                    .perf_observers
                    .iter()
                    .any(|(_, held)| Rc::ptr_eq(held, &observer))
                {
                    self.perf_observers.push((object.clone(), observer));
                }
                Ok(Value::Undefined)
            }
            ObserverMember::Disconnect => {
                self.perf_observers
                    .retain(|(_, held)| !Rc::ptr_eq(held, &observer));
                observer.buffer.borrow_mut().clear();
                observer.types.borrow_mut().clear();
                Ok(Value::Undefined)
            }
            _ => {
                let entries = std::mem::take(&mut *observer.buffer.borrow_mut());
                Ok(Self::new_array(
                    entries
                        .iter()
                        .map(|entry| self.performance_entry(entry))
                        .collect(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_observers_hear_marks_and_measures_in_a_task() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                performance.mark("early");
                const observer = new PerformanceObserver((list, self) => {
                    seen.push(self === observer, list.getEntries().map((entry) => entry.entryType + ":" + entry.name).join());
                    seen.push(list.getEntriesByType("measure").length, list.getEntriesByName("start", "mark").length);
                    document.getElementById("out").textContent = seen.join("|");
                });
                observer.observe({ entryTypes: ["mark", "measure", "paint"] });
                performance.mark("start", { startTime: 5, detail: { why: "test" } });
                const measured = performance.measure("span", { start: "start", duration: 10 });
                seen.push(measured.startTime, measured.duration, performance.getEntriesByName("start")[0].detail.why);
                seen.push(PerformanceObserver.supportedEntryTypes.join(), observer.takeRecords().length);
                performance.mark("after");
                try { observer.observe({ type: "mark", entryTypes: ["mark"] }); } catch (error) { seen.push(error.name); }
                seen.push(new ReportingObserver(() => {}).takeRecords().length);
                "#,
            )
            .expect("script should run");
        runtime.advance_time(1).expect("tasks should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("5|10|test|mark,measure|2|TypeError|0|true|mark:after|0|0")
        );
    }
}
