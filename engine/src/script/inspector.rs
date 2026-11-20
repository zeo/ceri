// what the devtools protocol's Runtime domain sees of a page: console entries kept with their
// values, values handed out by id, and evaluation in the page's global scope

use super::debugger::Site;
use super::object::{ObjectClass, Slot, is_private_key, is_symbol_key};
use super::operations::{is_callable, number_to_string, symbol_description};
use super::*;
use serde_json::{Value as Json, json};
use std::sync::atomic::AtomicU64;

thread_local! {
    static FRAME_ID: RefCell<String> = RefCell::new("ceri-main-frame".to_string());
}

// a page's main frame shares its id with the devtools target the page's window is, as a
// chromium page's does, so automation can find one from the other
pub(crate) fn frame_id() -> String {
    FRAME_ID.with_borrow(Clone::clone)
}

pub(crate) fn set_frame_id(id: &str) {
    FRAME_ID.set(id.to_string());
}

// v8's console message storage keeps as many
const KEPT_MESSAGES: usize = 1000;
const PREVIEW_PROPERTIES: usize = 5;
const PREVIEW_INDEXES: usize = 100;

static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

struct ConsoleEntry {
    sequence: u64,
    // the protocol's ConsoleAPICalled type, or exception for an uncaught error
    kind: &'static str,
    arguments: Vec<Value>,
    // for an uncaught error, the frames it was thrown in
    frames: Vec<Site>,
    timestamp: f64,
}

struct Awaited {
    token: u64,
    promise: PromiseHandle,
    group: String,
    by_value: bool,
    preview: bool,
}

pub(super) struct Inspector {
    context: u64,
    pub(super) capturing: bool,
    messages: VecDeque<ConsoleEntry>,
    next_sequence: u64,
    objects: HashMap<u64, (Value, String)>,
    next_object: u64,
    awaited: Vec<Awaited>,
    next_token: u64,
    next_exception: u64,
    // the nodes last selected in the elements panel, newest first, for $0 to $4
    inspected: Vec<NodeId>,
    // the console's last result, for $_
    last_result: Value,
}

impl Inspector {
    pub(super) fn new() -> Self {
        Self {
            context: NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed),
            capturing: false,
            messages: VecDeque::new(),
            next_sequence: 0,
            objects: HashMap::new(),
            next_object: 1,
            awaited: Vec::new(),
            next_token: 1,
            next_exception: 1,
            inspected: Vec::new(),
            last_result: Value::Undefined,
        }
    }
}

// a value the DOM domain hands to page script
pub(crate) enum NodeArgument {
    Text(String),
    Node(Option<NodeId>),
}

// what a call on a node gave back: the nodes in it, and the value as JSON
pub(crate) struct NodeCall {
    pub(crate) nodes: Vec<NodeId>,
    pub(crate) value: Json,
}

// an answer now, or a token whose answer comes once the promise it waits on settles
pub(crate) enum Answer {
    Now(Result<Json, String>),
    Later(u64),
    Unknown,
}

impl ScriptRuntime {
    // keeps console calls and uncaught errors with their values, for a devtools session
    pub(crate) fn capture_console(&mut self) {
        self.inspector.capturing = true;
    }

    pub(crate) fn inspector_context(&self) -> u64 {
        self.inspector.context
    }

    pub(super) fn console_record(&mut self, kind: &'static str, arguments: Vec<Value>) {
        self.record(kind, arguments, Vec::new());
    }

    pub(super) fn exception_record(&mut self, thrown: Value, frames: Vec<Site>) {
        self.record("exception", vec![thrown], frames);
    }

    fn record(&mut self, kind: &'static str, arguments: Vec<Value>, frames: Vec<Site>) {
        if !self.inspector.capturing {
            return;
        }
        if self.inspector.messages.len() == KEPT_MESSAGES {
            self.inspector.messages.pop_front();
        }
        let sequence = self.inspector.next_sequence;
        self.inspector.next_sequence += 1;
        let timestamp = self.time_origin + self.clock as f64;
        self.inspector.messages.push_back(ConsoleEntry {
            sequence,
            kind,
            arguments,
            frames,
            timestamp,
        });
    }

    // the protocol events for console entries from sequence on, with the next sequence to ask for
    pub(crate) fn console_events(&mut self, from: u64) -> (Vec<Json>, u64) {
        let entries: Vec<_> = self
            .inspector
            .messages
            .iter()
            .filter(|entry| entry.sequence >= from)
            .map(|entry| {
                let frames = entry.frames.clone();
                (entry.kind, entry.arguments.clone(), frames, entry.timestamp)
            })
            .collect();
        let context = self.inspector.context;
        let events = entries
            .into_iter()
            .map(|(kind, arguments, frames, timestamp)| {
                if kind == "exception" {
                    let thrown = arguments.into_iter().next().unwrap_or(Value::Undefined);
                    let details = self.exception_details(&thrown, &frames, "console");
                    return json!({
                        "method": "Runtime.exceptionThrown",
                        "params": { "timestamp": timestamp, "exceptionDetails": details },
                    });
                }
                let args: Vec<Json> = arguments
                    .iter()
                    .map(|value| self.remote_object(value, "console", true))
                    .collect();
                json!({
                    "method": "Runtime.consoleAPICalled",
                    "params": {
                        "type": kind,
                        "args": args,
                        "executionContextId": context,
                        "timestamp": timestamp,
                    },
                })
            })
            .collect();
        (events, self.inspector.next_sequence)
    }

    // read from the resources the page loads from, so a paused page can answer too
    fn resource_content(&self, params: &Json) -> Result<Json, String> {
        let not_found = || "No resource with given URL found".to_string();
        let url = params["url"].as_str().ok_or_else(not_found)?;
        let url = AppUrl::parse(url).map_err(|_| not_found())?;
        let empty = ResourceBundle::default();
        let resources = self.resources.as_ref().unwrap_or(&empty);
        let resource = resolve_runtime_resource(resources, self.resource_provider.as_deref(), &url)
            .ok()
            .flatten()
            .ok_or_else(not_found)?;
        let (content, base64_encoded) = encoded_body(resource.body(), resource.mime_type());
        Ok(json!({ "content": content, "base64Encoded": base64_encoded }))
    }

    pub(crate) fn inspector_call(&mut self, session: u64, method: &str, params: &Json) -> Answer {
        let answer = match method {
            _ if method.starts_with("Debugger.") => {
                return self.debugger_call(session, method, params);
            }
            "Runtime.evaluate" => return self.inspector_evaluate(params),
            "Runtime.callFunctionOn" => return self.inspector_call_function(params),
            "Runtime.awaitPromise" => {
                return match self.held(params) {
                    Ok(Value::Promise(promise)) => self.settle_or_wait(
                        promise,
                        "console".to_string(),
                        params["returnByValue"] == true,
                        params["generatePreview"] == true,
                    ),
                    Ok(_) => Answer::Now(Err("Could not find promise with given id".to_string())),
                    Err(error) => Answer::Now(Err(error)),
                };
            }
            "Runtime.getProperties" => self.inspector_properties(params),
            "Runtime.releaseObject" => {
                if let Some(id) = self.object_number(params["objectId"].as_str()) {
                    self.inspector.objects.remove(&id);
                }
                Ok(json!({}))
            }
            "Runtime.releaseObjectGroup" => {
                let group = params["objectGroup"].as_str().unwrap_or_default();
                self.inspector
                    .objects
                    .retain(|_, (_, held)| held.as_str() != group);
                Ok(json!({}))
            }
            "Page.getResourceContent" => self.resource_content(params),
            "Runtime.discardConsoleEntries" => {
                self.inspector.messages.clear();
                Ok(json!({}))
            }
            "Runtime.compileScript" => {
                let source = params["expression"].as_str().unwrap_or_default();
                match self.parse_console_source(source) {
                    Ok(()) => Ok(json!({})),
                    Err(error) => {
                        let thrown = self.throw_error("SyntaxError", error.message);
                        let thrown = self.take_thrown(thrown);
                        Ok(
                            json!({ "exceptionDetails": self.exception_details(&thrown, &[], "console") }),
                        )
                    }
                }
            }
            "Runtime.globalLexicalScopeNames" => {
                let names: Vec<String> = self
                    .global
                    .borrow()
                    .values
                    .keys()
                    .filter(|key| !is_private_key(key) && !is_symbol_key(key))
                    .cloned()
                    .collect();
                Ok(json!({ "names": names }))
            }
            _ => return Answer::Unknown,
        };
        Answer::Now(answer)
    }

    pub(crate) fn inspector_node_object(&mut self, node: NodeId, group: &str) -> Json {
        let value = self.node_value(node);
        self.remote_object(&value, group, false)
    }

    pub(crate) fn inspector_object_node(&self, object_id: &str) -> Option<NodeId> {
        let id = self.object_number(Some(object_id))?;
        match &self.inspector.objects.get(&id)?.0 {
            Value::Element(handle) => Some(handle.id),
            Value::Document => Some(self.document.borrow().root()),
            _ => None,
        }
    }

    // runs a function with the node as this, so the DOM domain edits through the same calls
    // page script makes and observers see them as chromium's would
    pub(crate) fn inspector_node_call(
        &mut self,
        node: NodeId,
        function: &str,
        arguments: Vec<NodeArgument>,
    ) -> Result<NodeCall, String> {
        let arguments = arguments
            .into_iter()
            .map(|argument| match argument {
                NodeArgument::Text(text) => Value::String(text.into()),
                NodeArgument::Node(Some(node)) => self.node_value(node),
                NodeArgument::Node(None) => Value::Null,
            })
            .collect();
        let this = self.node_value(node);
        let outcome = self
            .evaluate_console_source(function)
            .and_then(|function| self.call_function(function, this, arguments));
        if let Err(error) = self.drain_microtasks() {
            self.report_exception(error);
        }
        let value = match outcome {
            Ok(value) => value,
            Err(error) => {
                let message = error.message.clone();
                self.take_thrown(error);
                return Err(message);
            }
        };
        let root = self.document.borrow().root();
        let node_of = |value: &Value| match value {
            Value::Element(handle) => Some(handle.id),
            Value::Document => Some(root),
            _ => None,
        };
        let nodes: Vec<NodeId> = match &value {
            Value::Array(items) => items.borrow().iter().filter_map(node_of).collect(),
            single => node_of(single).into_iter().collect(),
        };
        let value = match nodes.is_empty() {
            true => self.value_json(&value)["value"].take(),
            false => Json::Null,
        };
        Ok(NodeCall { nodes, value })
    }

    pub(crate) fn inspect_node(&mut self, node: NodeId) {
        let inspected = &mut self.inspector.inspected;
        inspected.retain(|held| *held != node);
        inspected.insert(0, node);
        inspected.truncate(5);
    }

    // the console's command line api, as globals for one evaluation where the page has none
    // of its own, given back by name so they can be taken away after
    pub(super) fn install_command_line(&mut self) -> Vec<String> {
        let inspected = self.inspector.inspected.clone();
        let mut entries = Vec::new();
        for index in 0..5 {
            let node = inspected
                .get(index)
                .copied()
                .filter(|node| self.document.borrow().contains(*node));
            let value = node.map_or(Value::Undefined, |node| self.node_value(node));
            entries.push((format!("${index}"), value));
        }
        entries.push(("$_".to_string(), self.inspector.last_result.clone()));
        for (name, source) in [
            (
                "$",
                "(selector, root = document) => root.querySelector(selector)",
            ),
            (
                "$$",
                "(selector, root = document) => Array.from(root.querySelectorAll(selector))",
            ),
        ] {
            if let Ok(function) = self.evaluate_console_source(source) {
                entries.push((name.to_string(), function));
            }
        }
        let mut installed = Vec::new();
        for (name, value) in entries {
            if matches!(self.lookup_global(&name), None | Some(Value::Undefined)) {
                self.global.borrow_mut().values.insert(name.clone(), value);
                installed.push(name);
            }
        }
        installed
    }

    // answers for awaited promises that have settled since the last call
    pub(crate) fn inspector_settled(&mut self) -> Vec<(u64, Result<Json, String>)> {
        let (settled, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.inspector.awaited)
            .into_iter()
            .partition(|awaited| {
                !matches!(awaited.promise.borrow().outcome, PromiseOutcome::Pending)
            });
        self.inspector.awaited = waiting;
        settled
            .into_iter()
            .map(|awaited| {
                let outcome = awaited.promise.borrow().outcome.clone();
                let answer = match outcome {
                    PromiseOutcome::Fulfilled(value) => Ok(self.result_json(
                        &value,
                        &awaited.group,
                        awaited.by_value,
                        awaited.preview,
                    )),
                    PromiseOutcome::Rejected(reason) => {
                        Ok(self.thrown_json(&reason, &awaited.group))
                    }
                    PromiseOutcome::Pending => unreachable!("partitioned on settled promises"),
                };
                (awaited.token, answer)
            })
            .collect()
    }

    fn inspector_evaluate(&mut self, params: &Json) -> Answer {
        let group = params["objectGroup"]
            .as_str()
            .unwrap_or("console")
            .to_string();
        let by_value = params["returnByValue"] == true;
        let preview = params["generatePreview"] == true;
        // the console's eager preview asks for no side effects, which this engine cannot promise
        if params["throwOnSideEffect"] == true {
            let thrown = self.throw_error("EvalError", "Possible side-effect in debug-evaluate");
            let thrown = self.take_thrown(thrown);
            return Answer::Now(Ok(self.thrown_json(&thrown, &group)));
        }
        let source = params["expression"].as_str().unwrap_or_default();
        let command_line = params["includeCommandLineAPI"] == true;
        let installed = match command_line {
            true => self.install_command_line(),
            false => Vec::new(),
        };
        let outcome = self.evaluate_console_input(source);
        for name in installed {
            self.global.borrow_mut().values.remove(&name);
        }
        if command_line && let Ok(value) = &outcome {
            self.inspector.last_result = value.clone();
        }
        self.finish_evaluation(
            outcome,
            group,
            by_value,
            preview,
            params["awaitPromise"] == true,
        )
    }

    fn inspector_call_function(&mut self, params: &Json) -> Answer {
        let group = params["objectGroup"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                let id = self.object_number(params["objectId"].as_str())?;
                self.inspector
                    .objects
                    .get(&id)
                    .map(|(_, group)| group.clone())
            })
            .unwrap_or_else(|| "console".to_string());
        let this = match params.get("objectId") {
            Some(_) => match self.held(params) {
                Ok(value) => value,
                Err(error) => return Answer::Now(Err(error)),
            },
            None => Value::Undefined,
        };
        let mut arguments = Vec::new();
        for argument in params["arguments"].as_array().into_iter().flatten() {
            match self.call_argument(argument) {
                Ok(value) => arguments.push(value),
                Err(error) => return Answer::Now(Err(error)),
            }
        }
        let declaration = params["functionDeclaration"].as_str().unwrap_or_default();
        let outcome = self
            .evaluate_console_source(declaration)
            .and_then(|function| self.call_function(function, this, arguments));
        self.finish_evaluation(
            outcome,
            group,
            params["returnByValue"] == true,
            params["generatePreview"] == true,
            params["awaitPromise"] == true,
        )
    }

    fn finish_evaluation(
        &mut self,
        outcome: Result<Value, ScriptError>,
        group: String,
        by_value: bool,
        preview: bool,
        await_promise: bool,
    ) -> Answer {
        // a paused page runs no jobs until it resumes
        if !self.debugger_paused()
            && let Err(error) = self.drain_microtasks()
        {
            self.report_exception(error);
        }
        match outcome {
            Ok(Value::Promise(promise)) if await_promise => {
                self.settle_or_wait(promise, group, by_value, preview)
            }
            Ok(value) => Answer::Now(Ok(self.result_json(&value, &group, by_value, preview))),
            Err(error) => {
                let thrown = self.take_thrown(error);
                Answer::Now(Ok(self.thrown_json(&thrown, &group)))
            }
        }
    }

    fn settle_or_wait(
        &mut self,
        promise: PromiseHandle,
        group: String,
        by_value: bool,
        preview: bool,
    ) -> Answer {
        let outcome = promise.borrow().outcome.clone();
        match outcome {
            PromiseOutcome::Fulfilled(value) => {
                Answer::Now(Ok(self.result_json(&value, &group, by_value, preview)))
            }
            PromiseOutcome::Rejected(reason) => Answer::Now(Ok(self.thrown_json(&reason, &group))),
            PromiseOutcome::Pending => {
                let token = self.inspector.next_token;
                self.inspector.next_token += 1;
                self.inspector.awaited.push(Awaited {
                    token,
                    promise,
                    group,
                    by_value,
                    preview,
                });
                Answer::Later(token)
            }
        }
    }

    // console input as the console reads it: an expression gives its value, one that awaits
    // runs in an async function, and anything else runs as a classic script
    fn evaluate_console_source(&mut self, source: &str) -> Result<Value, ScriptError> {
        // the line break keeps a trailing line comment from swallowing the parenthesis
        let body = format!("return (\n{source}\n)");
        for is_async in [false, true] {
            if let Ok((function, text)) = parser::parse_function_source("", &body, is_async, false)
            {
                let code = compiler::Compiler::new(&text).compile_function(&function)?;
                let closure = self.new_closure(code, None, None);
                return self.call_function(closure, Value::Window, Vec::new());
            }
        }
        let script = parser::parse_script(source)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        let code = compiler::Compiler::new(source)
            .compile_console_script(&script)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        self.run_script_code(code)
    }

    // typed into a console: a script for its completion value, so a debugger shows the source as
    // written, or else an expression that may await
    fn evaluate_console_input(&mut self, source: &str) -> Result<Value, ScriptError> {
        // a leading brace is an object literal, as chromium's console wraps it
        let object = source.trim_start().starts_with('{')
            && parser::parse_function_source(
                "",
                &format!(
                    "return (
{source}
)"
                ),
                false,
                false,
            )
            .is_ok();
        let script = match parser::parse_script(source) {
            Ok(script) if !object => script,
            _ => return self.evaluate_console_source(source),
        };
        let code = compiler::Compiler::new(source)
            .compile_console_script(&script)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        self.run_script_code(code)
    }

    fn parse_console_source(&mut self, source: &str) -> Result<(), ScriptError> {
        let body = format!("return (\n{source}\n)");
        let expression = [false, true]
            .into_iter()
            .any(|is_async| parser::parse_function_source("", &body, is_async, false).is_ok());
        match expression {
            true => Ok(()),
            false => parser::parse_script(source).map(|_| ()),
        }
    }

    pub(super) fn result_json(
        &mut self,
        value: &Value,
        group: &str,
        by_value: bool,
        preview: bool,
    ) -> Json {
        let result = match by_value {
            true => self.value_json(value),
            false => self.remote_object(value, group, preview),
        };
        json!({ "result": result })
    }

    pub(super) fn thrown_json(&mut self, thrown: &Value, group: &str) -> Json {
        let frames = self.take_thrown_frames(thrown);
        json!({
            "result": self.remote_object(thrown, group, false),
            "exceptionDetails": self.exception_details(thrown, &frames, group),
        })
    }

    // frames are where the value was thrown, innermost first, when running code threw it
    fn exception_details(&mut self, thrown: &Value, frames: &[Site], group: &str) -> Json {
        let id = self.inspector.next_exception;
        self.inspector.next_exception += 1;
        let mut details = json!({
            "exceptionId": id,
            "text": "Uncaught",
            "lineNumber": 0,
            "columnNumber": 0,
            "exception": self.remote_object(thrown, group, false),
            "executionContextId": self.inspector.context,
        });
        let frames = self.stack_frames_json(frames);
        if let Some(top) = frames.first() {
            for key in ["lineNumber", "columnNumber", "scriptId", "url"] {
                details[key] = top[key].clone();
            }
            details["stackTrace"] = json!({ "callFrames": frames });
        }
        details
    }

    // a value as JSON, as returnByValue asks; what JSON cannot hold goes as an unserializable
    // number or as undefined
    fn value_json(&mut self, value: &Value) -> Json {
        match value {
            Value::Undefined | Value::Uninitialized => json!({ "type": "undefined" }),
            Value::Number(number) => number_remote(*number),
            Value::BigInt(_) | Value::Symbol(_) => self.remote_object(value, "console", false),
            _ => {
                let (kind, subtype, _, _) = self.describe(value);
                let text = self.json_text(value);
                self.pending_exception = None;
                let mut result = json!({ "type": kind });
                if let Some(subtype) = subtype {
                    result["subtype"] = subtype.into();
                }
                result["value"] = text
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or(json!({}));
                result
            }
        }
    }

    fn hold(&mut self, value: Value, group: &str) -> String {
        let id = self.inspector.next_object;
        self.inspector.next_object += 1;
        self.inspector
            .objects
            .insert(id, (value, group.to_string()));
        format!("{}.{id}", self.inspector.context)
    }

    fn object_number(&self, id: Option<&str>) -> Option<u64> {
        let (context, number) = id?.split_once('.')?;
        (context.parse::<u64>().ok()? == self.inspector.context)
            .then(|| number.parse().ok())
            .flatten()
    }

    fn held(&self, params: &Json) -> Result<Value, String> {
        self.object_number(params["objectId"].as_str())
            .and_then(|id| self.inspector.objects.get(&id))
            .map(|(value, _)| value.clone())
            .ok_or_else(|| "Could not find object with given id".to_string())
    }

    fn call_argument(&mut self, argument: &Json) -> Result<Value, String> {
        if argument.get("objectId").is_some() {
            return self.held(argument);
        }
        if let Some(text) = argument["unserializableValue"].as_str() {
            return Ok(match text {
                "NaN" => Value::Number(f64::NAN),
                "Infinity" => Value::Number(f64::INFINITY),
                "-Infinity" => Value::Number(f64::NEG_INFINITY),
                "-0" => Value::Number(-0.0),
                bigint => self
                    .evaluate_console_source(bigint)
                    .map_err(|error| error.message)?,
            });
        }
        match argument.get("value") {
            None => Ok(Value::Undefined),
            Some(value) => json::parse(&value.to_string()),
        }
    }

    fn inspector_properties(&mut self, params: &Json) -> Result<Json, String> {
        let target = self.held(params)?;
        let group = self
            .object_number(params["objectId"].as_str())
            .and_then(|id| self.inspector.objects.get(&id))
            .map(|(_, group)| group.clone())
            .unwrap_or_default();
        let own_only = params["ownProperties"] == true;
        let accessors_only = params["accessorPropertiesOnly"] == true;
        let preview = params["generatePreview"] == true;
        let indexes = params["nonIndexedPropertiesOnly"] != true;
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(target.clone());
        let mut is_own = true;
        while let Some(holder) = current {
            let mut keys = self
                .own_property_keys(&holder, false)
                .map_err(|error| error.message)?;
            keys.extend(
                self.own_property_keys(&holder, true)
                    .map_err(|error| error.message)?,
            );
            for key in keys {
                if is_private_key(&key)
                    || !seen.insert(key.clone())
                    || (!indexes && key.parse::<u32>().is_ok())
                {
                    continue;
                }
                let Some(property) = self
                    .own_property_of(&holder, &key)
                    .map_err(|error| error.message)?
                else {
                    continue;
                };
                let accessor = matches!(property.slot, Slot::Accessor { .. });
                if accessors_only && !accessor {
                    continue;
                }
                let name = match is_symbol_key(&key) {
                    true => format!("Symbol({})", symbol_description(&key).unwrap_or_default()),
                    false => key.clone(),
                };
                let mut descriptor = json!({
                    "name": name,
                    "configurable": property.configurable,
                    "enumerable": property.enumerable,
                    "isOwn": is_own,
                });
                match &property.slot {
                    Slot::Data(value) => {
                        descriptor["value"] = self.remote_object(&value.resolve(), &group, preview);
                        descriptor["writable"] = property.writable.into();
                    }
                    Slot::Accessor { get, set } => {
                        let get = get.clone().unwrap_or(Value::Undefined);
                        let set = set.clone().unwrap_or(Value::Undefined);
                        descriptor["get"] = self.remote_object(&get, &group, false);
                        descriptor["set"] = self.remote_object(&set, &group, false);
                    }
                }
                if is_symbol_key(&key) {
                    descriptor["symbol"] =
                        self.remote_object(&Value::Symbol(key.clone()), &group, false);
                }
                result.push(descriptor);
            }
            if own_only && !accessors_only {
                break;
            }
            current = self
                .prototype_of_value(&holder)
                .map_err(|error| error.message)?
                .map(Value::Object);
            is_own = false;
        }
        self.pending_exception = None;
        let mut internal = Vec::new();
        if own_only && !accessors_only {
            if let Value::Promise(promise) = &target {
                let outcome = promise.borrow().outcome.clone();
                let (state, value) = match outcome {
                    PromiseOutcome::Pending => ("pending", None),
                    PromiseOutcome::Fulfilled(value) => ("fulfilled", Some(value)),
                    PromiseOutcome::Rejected(reason) => ("rejected", Some(reason)),
                };
                internal.push(json!({
                    "name": "[[PromiseState]]",
                    "value": { "type": "string", "value": state },
                }));
                if let Some(value) = value {
                    internal.push(json!({
                        "name": "[[PromiseResult]]",
                        "value": self.remote_object(&value, &group, preview),
                    }));
                }
            }
            if let Some(prototype) = self
                .prototype_of_value(&target)
                .map_err(|error| error.message)?
            {
                internal.push(json!({
                    "name": "[[Prototype]]",
                    "value": self.remote_object(&Value::Object(prototype), &group, false),
                }));
            }
        }
        self.pending_exception = None;
        Ok(json!({ "result": result, "internalProperties": internal }))
    }

    // a value as the protocol's RemoteObject: primitives inline, everything else by id
    pub(super) fn remote_object(&mut self, value: &Value, group: &str, preview: bool) -> Json {
        let value = value.resolve();
        match &value {
            Value::Undefined | Value::Uninitialized => json!({ "type": "undefined" }),
            Value::Null => json!({ "type": "object", "subtype": "null", "value": null }),
            Value::Bool(flag) => json!({ "type": "boolean", "value": flag }),
            Value::Number(number) => number_remote(*number),
            Value::String(text) => json!({ "type": "string", "value": &**text }),
            Value::BigInt(number) => {
                let text = format!("{}n", number.to_string_radix(10));
                json!({ "type": "bigint", "unserializableValue": text, "description": text })
            }
            Value::Symbol(key) => json!({
                "type": "symbol",
                "description": format!("Symbol({})", symbol_description(key).unwrap_or_default()),
                "objectId": self.hold(value.clone(), group),
            }),
            _ => {
                let (kind, subtype, class_name, description) = self.describe(&value);
                let mut remote = json!({
                    "type": kind,
                    "className": class_name,
                    "description": description,
                    "objectId": self.hold(value.clone(), group),
                });
                if let Some(subtype) = subtype {
                    remote["subtype"] = subtype.into();
                }
                if preview && kind == "object" {
                    remote["preview"] = self.object_preview(&value, subtype, &description);
                }
                remote
            }
        }
    }

    // the type, subtype, class name and description the protocol gives an object
    fn describe(&mut self, value: &Value) -> (&'static str, Option<&'static str>, String, String) {
        let described = self.describe_object(value);
        self.pending_exception = None;
        described
    }

    fn describe_object(
        &mut self,
        value: &Value,
    ) -> (&'static str, Option<&'static str>, String, String) {
        if is_callable(value) {
            let source = self
                .coerce_string(value.clone())
                .unwrap_or_else(|_| "function () { [native code] }".to_string());
            return ("function", None, "Function".to_string(), source);
        }
        let class_name = self.class_name(value);
        let (subtype, description) = match value {
            Value::Window => (None, "Window".to_string()),
            Value::Array(array) => (Some("array"), format!("Array({})", array.borrow().len())),
            Value::Element(_) | Value::Document => (Some("node"), self.node_description(value)),
            Value::Error(_) => (Some("error"), self.error_description(value)),
            Value::Promise(_) => (Some("promise"), "Promise".to_string()),
            Value::TypedArray(array) => (
                Some("typedarray"),
                format!("{class_name}({})", typed::element_count(&array.borrow())),
            ),
            Value::Bytes(bytes) => (
                Some("arraybuffer"),
                format!("ArrayBuffer({})", bytes.borrow().len()),
            ),
            Value::Object(object) => {
                let class = object.borrow().class.clone();
                match class {
                    ObjectClass::Error => (Some("error"), self.error_description(value)),
                    ObjectClass::Map(entries) => (
                        Some("map"),
                        format!("Map({})", entries.borrow().values().len()),
                    ),
                    ObjectClass::Set(entries) => (
                        Some("set"),
                        format!("Set({})", entries.borrow().values().len()),
                    ),
                    ObjectClass::WeakMap(_) => (Some("weakmap"), class_name.clone()),
                    ObjectClass::WeakSet(_) => (Some("weakset"), class_name.clone()),
                    ObjectClass::Date(_) | ObjectClass::RegExp(_) => {
                        let subtype = match class {
                            ObjectClass::Date(_) => "date",
                            _ => "regexp",
                        };
                        let text = self.coerce_string(value.clone()).unwrap_or_default();
                        (Some(subtype), text)
                    }
                    ObjectClass::Proxy(_) => (Some("proxy"), "Proxy(Object)".to_string()),
                    ObjectClass::Generator(_) | ObjectClass::AsyncGenerator(_) => {
                        (Some("generator"), class_name.clone())
                    }
                    _ => (None, class_name.clone()),
                }
            }
            _ => (None, class_name.clone()),
        };
        ("object", subtype, class_name, description)
    }

    fn class_name(&mut self, value: &Value) -> String {
        if matches!(value, Value::Window) {
            return "Window".to_string();
        }
        let named = self
            .get_property(value, "constructor")
            .ok()
            .filter(is_callable)
            .and_then(
                |constructor| match self.get_property(&constructor, "name") {
                    Ok(Value::String(name)) if !name.is_empty() => Some(name.to_string()),
                    _ => None,
                },
            );
        self.pending_exception = None;
        named.unwrap_or_else(|| {
            let tag = self.object_to_string(value).unwrap_or_default();
            self.pending_exception = None;
            tag.strip_prefix("[object ")
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or("Object")
                .to_string()
        })
    }

    // div#id.first.second, as the console names an element
    fn node_description(&mut self, value: &Value) -> String {
        if matches!(value, Value::Document) {
            return "#document".to_string();
        }
        let read = |runtime: &mut Self, key: &str| match runtime.get_property(value, key) {
            Ok(Value::String(text)) => text.to_string(),
            _ => String::new(),
        };
        let name = match read(self, "localName") {
            name if name.is_empty() => read(self, "nodeName"),
            name => name,
        };
        let id = read(self, "id");
        let classes = read(self, "className");
        let mut description = name;
        if !id.is_empty() {
            description.push('#');
            description.push_str(&id);
        }
        for class in classes.split_whitespace() {
            description.push('.');
            description.push_str(class);
        }
        description
    }

    fn error_description(&mut self, value: &Value) -> String {
        match self.get_property(value, "stack") {
            Ok(Value::String(stack)) if !stack.is_empty() => stack.to_string(),
            _ => self.inspect(value, 0, &mut Vec::new()),
        }
    }

    fn object_preview(&mut self, value: &Value, subtype: Option<&str>, description: &str) -> Json {
        let limit = match subtype {
            Some("array" | "typedarray") => PREVIEW_INDEXES,
            _ => PREVIEW_PROPERTIES,
        };
        let mut properties = Vec::new();
        let mut overflow = false;
        let listed = !matches!(
            subtype,
            Some("node" | "error" | "date" | "regexp" | "promise" | "proxy")
        );
        let keys = match listed {
            true => self.own_property_keys(value, false).unwrap_or_default(),
            false => Vec::new(),
        };
        for key in keys {
            if is_private_key(&key) {
                continue;
            }
            let Ok(Some(property)) = self.own_property_of(value, &key) else {
                continue;
            };
            if !property.enumerable {
                continue;
            }
            if properties.len() == limit {
                overflow = true;
                break;
            }
            properties.push(match &property.slot {
                Slot::Data(value) => self.property_preview(&key, &value.resolve()),
                Slot::Accessor { .. } => json!({ "name": key, "type": "accessor" }),
            });
        }
        self.pending_exception = None;
        let mut preview = json!({
            "type": "object",
            "description": description,
            "overflow": overflow,
            "properties": properties,
        });
        if let Some(subtype) = subtype {
            preview["subtype"] = subtype.into();
        }
        preview
    }

    fn property_preview(&mut self, name: &str, value: &Value) -> Json {
        let (kind, subtype, text) = match value {
            Value::Undefined | Value::Uninitialized => ("undefined", None, "undefined".to_string()),
            Value::Null => ("object", Some("null"), "null".to_string()),
            Value::Bool(flag) => ("boolean", None, flag.to_string()),
            Value::Number(number) => ("number", None, number_to_string(*number)),
            Value::String(text) => ("string", None, text.to_string()),
            Value::BigInt(number) => ("bigint", None, format!("{}n", number.to_string_radix(10))),
            Value::Symbol(key) => (
                "symbol",
                None,
                format!("Symbol({})", symbol_description(key).unwrap_or_default()),
            ),
            value if is_callable(value) => ("function", None, String::new()),
            value => {
                let (kind, subtype, _, description) = self.describe(value);
                (kind, subtype, description)
            }
        };
        let mut preview = json!({ "name": name, "type": kind, "value": text });
        if let Some(subtype) = subtype {
            preview["subtype"] = subtype.into();
        }
        preview
    }
}

// a body as text when it is text, otherwise in base64
pub(crate) fn encoded_body(bytes: &[u8], mime: &str) -> (String, bool) {
    let mime = mime.split(';').next().unwrap_or_default().trim();
    let text = mime.starts_with("text/")
        || ["json", "javascript", "ecmascript", "xml", "svg"]
            .iter()
            .any(|kind| mime.contains(kind));
    match std::str::from_utf8(bytes) {
        Ok(body) if text => (body.to_string(), false),
        _ => (base64(bytes), true),
    }
}

fn number_remote(number: f64) -> Json {
    let unserializable = match number {
        number if number.is_nan() => Some("NaN"),
        number if number == f64::INFINITY => Some("Infinity"),
        number if number == f64::NEG_INFINITY => Some("-Infinity"),
        number if number == 0.0 && number.is_sign_negative() => Some("-0"),
        _ => None,
    };
    match unserializable {
        Some(text) => json!({ "type": "number", "unserializableValue": text, "description": text }),
        None => {
            json!({ "type": "number", "value": number, "description": number_to_string(number) })
        }
    }
}
