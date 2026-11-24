// the devtools protocol's Debugger domain for a page: the scripts it compiled and where each came
// from, breakpoints on their statements, and a pause that holds the engine thread while the
// sessions look at the stack
// it reads the interpreter's frames, boxed there
#![allow(clippy::vec_box)]

use super::compiler::{Code, Op};
use super::inspector::frame_id;
use super::object::new_object;
use super::operations::same_value;
use super::vm::{Frame, ScopeRef};
use super::*;
use serde_json::{Value as Json, json};
use std::fmt::Write;
use std::rc::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;

// how many ops run between looks for a pause request, so a busy loop can still be paused
const POLL_STEPS: u32 = 1 << 16;
const PAUSED_ONLY: &str = "Can only perform operation while paused.";
// how many frames an error's stack names, as v8 keeps
const STACK_FRAMES: usize = 10;

static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(1);

// where a paused page reads its devtools messages; whatever else reaches the window waits until
// the page runs again
pub(crate) trait PauseSource {
    fn next(&self) -> Incoming;
    fn poll(&self) -> Option<Incoming>;
    // a message the paused page can't answer, kept for after it resumes
    fn defer(&self, session: u64, text: String);
}

pub(crate) enum Incoming {
    Message { session: u64, text: String },
    Detached(u64),
    Closed,
}

// what outlives a page: the sessions with the debugger on, their breakpoints and settings
#[derive(Default)]
pub(crate) struct DebugState {
    sessions: Vec<(u64, Sender<String>)>,
    breakpoints: Vec<Breakpoint>,
    pause_on: PauseOn,
    inactive: bool,
    skipping: bool,
    source: Option<Rc<dyn PauseSource>>,
    // runtime calls answered later, made while paused: session, message id and token
    waiting: Vec<(u64, u64, u64)>,
}

pub(crate) type SharedDebug = Rc<RefCell<DebugState>>;

impl DebugState {
    pub(crate) fn enable(&mut self, session: u64, outgoing: Sender<String>) {
        if !self.sessions.iter().any(|(id, _)| *id == session) {
            self.sessions.push((session, outgoing));
        }
    }

    pub(crate) fn forget(&mut self, session: u64) {
        self.sessions.retain(|(id, _)| *id != session);
        self.breakpoints.retain(|held| held.session != session);
        self.waiting.retain(|(held, _, _)| *held != session);
        if self.sessions.is_empty() {
            *self = Self {
                source: self.source.take(),
                ..Self::default()
            };
        }
    }

    pub(crate) fn set_source(&mut self, source: Rc<dyn PauseSource>) {
        self.source = Some(source);
    }

    pub(crate) fn take_waiting(&mut self) -> Vec<(u64, u64, u64)> {
        std::mem::take(&mut self.waiting)
    }

    fn post(&self, session: u64, message: &Json) {
        if let Some((_, outgoing)) = self.sessions.iter().find(|(id, _)| *id == session) {
            let _ = outgoing.send(message.to_string());
        }
    }

    fn broadcast(&self, message: &Json) {
        let text = message.to_string();
        for (_, outgoing) in &self.sessions {
            let _ = outgoing.send(text.clone());
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq)]
enum PauseOn {
    #[default]
    None,
    Caught,
    Uncaught,
    All,
}

struct Breakpoint {
    id: String,
    session: u64,
    target: Target,
    line: u32,
    column: u32,
    condition: String,
}

enum Target {
    Url(String),
    Script(String),
    // a url pattern or hash, which no script here is matched against
    Unmatched,
}

#[derive(Clone, Copy)]
enum Step {
    Into,
    Over,
    Out,
}

struct Script {
    id: String,
    url: String,
    text: Rc<str>,
    // where the text starts in its document, for a script inline in a page
    line: u32,
    column: u32,
    lines: Vec<usize>,
    code: Option<Rc<Code>>,
    module: bool,
}

impl Script {
    // a byte offset in the text as the protocol's line and UTF-16 column in the document
    fn location(&self, offset: usize) -> (u32, u32) {
        locate(&self.text, &self.lines, (self.line, self.column), offset)
    }

    fn offset(&self, line: u32, column: u32) -> Option<usize> {
        let relative = line.checked_sub(self.line)? as usize;
        let column = match relative {
            0 => column.saturating_sub(self.column),
            _ => column,
        };
        let start = *self.lines.get(relative)?;
        let end = self
            .lines
            .get(relative + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut units = 0;
        for (index, character) in self.text[start..end].char_indices() {
            if units >= column {
                return Some(start + index);
            }
            units += character.len_utf16() as u32;
        }
        Some(end)
    }

    // every statement start in the script's code, as its offset, code and op index
    fn statements(&self) -> Vec<(usize, Rc<Code>, u32)> {
        let mut found = Vec::new();
        if let Some(code) = &self.code {
            each_code(code, &mut |code| {
                for (pc, offset) in &code.positions {
                    found.push((*offset as usize, code.clone(), *pc));
                }
            });
        }
        found.sort_by_key(|(offset, _, pc)| (*offset, *pc));
        found
    }
}

// where a script's text came from, for a stack to name without a devtools session
struct Origin {
    // held so the text's address stays its own
    text: Weak<str>,
    url: String,
    line: u32,
    column: u32,
    lines: Vec<usize>,
}

// a frame's code and the op it is at
pub(super) type Site = (Rc<Code>, usize);

#[derive(Clone)]
pub(super) struct Snapshot {
    code: Rc<Code>,
    pc: usize,
    scope: Option<ScopeRef>,
    this: Value,
    catches: bool,
}

#[derive(Default)]
pub(super) struct Debugger {
    shared: Option<SharedDebug>,
    scripts: Vec<Script>,
    // the url and document position the next compiled script came from
    origin: Option<(String, u32, u32)>,
    // the scripts still alive, by the address of their text
    origins: HashMap<usize, Origin>,
    // every running frame, outermost first, as of its last op, kept whether or not a session
    // is debugging
    pub(super) calls: Vec<Site>,
    // the last value thrown and not caught since, with the frames it was thrown in
    thrown: Option<(Value, Vec<Site>)>,
    // the breakpoints in compiled ops, by the code's address and op index; the code is held so
    // its address stays its own
    resolved: HashMap<(usize, u32), (Rc<Code>, Vec<String>)>,
    // every running frame, outermost first, as of its last op
    pub(super) mirror: Vec<Snapshot>,
    pub(super) active: bool,
    breaking: bool,
    pause_on: PauseOn,
    paused: bool,
    stepping: Option<(Step, usize)>,
    pause_next: bool,
    continuing: bool,
    polled: u32,
    last_thrown: Option<Value>,
}

impl ScriptRuntime {
    pub(crate) fn set_debugger(&mut self, shared: SharedDebug) {
        self.debugger.shared = Some(shared);
        self.refresh_debugger();
    }

    // the url and place in its page of the script about to run
    pub(crate) fn set_script_origin(&mut self, url: String, line: u32, column: u32) {
        self.debugger.origin = Some((url, line, column));
    }

    pub(crate) fn clear_script_origin(&mut self) {
        self.debugger.origin = None;
    }

    fn refresh_debugger(&mut self) {
        let debugger = &mut self.debugger;
        let Some(shared) = debugger.shared.clone() else {
            return;
        };
        let shared = shared.borrow();
        debugger.active = !shared.sessions.is_empty();
        debugger.breaking = debugger.active && !shared.inactive && !shared.skipping;
        debugger.pause_on = match shared.skipping {
            true => PauseOn::None,
            false => shared.pause_on,
        };
        if !debugger.active {
            debugger.stepping = None;
            debugger.pause_next = false;
        }
    }

    fn broadcast(&self, message: Json) {
        if let Some(shared) = &self.debugger.shared {
            shared.borrow().broadcast(&message);
        }
    }

    // a script compiled from a whole source, given the url it falls back on when the page set
    // no origin for it
    pub(super) fn register_script(&mut self, code: &Rc<Code>, url: &str, module: bool) {
        let (url, line, column) = self
            .debugger
            .origin
            .take()
            .unwrap_or_else(|| (url.to_string(), 0, 0));
        let origins = &mut self.debugger.origins;
        origins.retain(|_, origin| origin.text.strong_count() > 0);
        origins.insert(
            text_address(&code.text),
            Origin {
                text: Rc::downgrade(&code.text),
                url: url.clone(),
                line,
                column,
                lines: line_starts(&code.text),
            },
        );
        if !self.inspector.capturing {
            return;
        }
        self.add_script(
            code.text.clone(),
            Some(code.clone()),
            url,
            line,
            column,
            module,
        );
    }

    fn add_script(
        &mut self,
        text: Rc<str>,
        code: Option<Rc<Code>>,
        url: String,
        line: u32,
        column: u32,
        module: bool,
    ) -> usize {
        let script = Script {
            id: NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed).to_string(),
            url,
            lines: line_starts(&text),
            text,
            line,
            column,
            code,
            module,
        };
        let parsed = self.script_parsed(&script);
        self.debugger.scripts.push(script);
        let index = self.debugger.scripts.len() - 1;
        if let Some(shared) = self.debugger.shared.clone() {
            shared.borrow().broadcast(&parsed);
            for breakpoint in &shared.borrow().breakpoints {
                if let Some(location) = self.resolve_in(index, breakpoint) {
                    let event = json!({
                        "method": "Debugger.breakpointResolved",
                        "params": { "breakpointId": breakpoint.id, "location": location },
                    });
                    shared.borrow().post(breakpoint.session, &event);
                }
            }
        }
        index
    }

    fn script_parsed(&self, script: &Script) -> Json {
        let (end_line, end_column) = script.location(script.text.len());
        json!({
            "method": "Debugger.scriptParsed",
            "params": {
                "scriptId": script.id,
                "url": script.url,
                "startLine": script.line,
                "startColumn": script.column,
                "endLine": end_line,
                "endColumn": end_column,
                "executionContextId": self.inspector_context(),
                "hash": hash(&script.text),
                "executionContextAuxData": { "isDefault": true, "type": "default", "frameId": frame_id() },
                "isLiveEdit": false,
                "sourceMapURL": "",
                "hasSourceURL": false,
                "isModule": script.module,
                "length": utf16_length(&script.text),
                "scriptLanguage": "JavaScript",
                "embedderName": script.url,
            },
        })
    }

    // the script some running code came from, registered now when it was compiled from text
    // that isn't a whole script, as the Function constructor's is
    fn script_of(&mut self, code: &Rc<Code>) -> usize {
        match self
            .debugger
            .scripts
            .iter()
            .position(|script| Rc::ptr_eq(&script.text, &code.text))
        {
            Some(index) => index,
            None => {
                // a script that ran before any session was there still has where it came from
                let (url, line, column) = self
                    .debugger
                    .origins
                    .get(&text_address(&code.text))
                    .map_or((String::new(), 0, 0), |origin| {
                        (origin.url.clone(), origin.line, origin.column)
                    });
                self.add_script(code.text.clone(), None, url, line, column, false)
            }
        }
    }

    fn script_index(&self, params: &Json) -> Result<usize, String> {
        let id = params["scriptId"].as_str().unwrap_or_default();
        self.debugger
            .scripts
            .iter()
            .position(|script| script.id == id)
            .ok_or_else(|| format!("No script for id: {id}"))
    }

    // the running frames innermost first, one line each as v8 writes an error's stack
    pub(super) fn write_stack(&self, stack: &mut String) {
        for (code, pc) in self.debugger.calls.iter().rev().take(STACK_FRAMES) {
            let offset = statement_offset(code, *pc);
            let (url, (line, column)) = match self.debugger.origins.get(&text_address(&code.text)) {
                Some(origin) => (
                    origin.url.as_str(),
                    locate(
                        &code.text,
                        &origin.lines,
                        (origin.line, origin.column),
                        offset,
                    ),
                ),
                // text that is no whole script, an eval's or the Function constructor's
                None => {
                    let before = &code.text[..offset];
                    ("", locate(before, &line_starts(before), (0, 0), offset))
                }
            };
            let url = if url.is_empty() { "<anonymous>" } else { url };
            let (line, column) = (line + 1, column + 1);
            let _ = match code.name.is_empty() {
                true => write!(stack, "\n    at {url}:{line}:{column}"),
                false => write!(stack, "\n    at {} ({url}:{line}:{column})", code.name),
            };
        }
    }

    // a value thrown inside the running frames, on its way to a handler
    pub(super) fn note_thrown(&mut self, thrown: &Value) {
        let debugger = &mut self.debugger;
        // a value thrown out of a nested run is thrown again in the run around it
        if debugger
            .thrown
            .as_ref()
            .is_some_and(|(last, _)| same_value(last, thrown))
        {
            return;
        }
        let frames = debugger.calls.iter().rev().take(STACK_FRAMES);
        debugger.thrown = Some((thrown.clone(), frames.cloned().collect()));
    }

    pub(super) fn note_caught(&mut self) {
        self.debugger.thrown = None;
    }

    // the frames an uncaught value was thrown in, innermost first; none when it was not
    // thrown by running code
    pub(super) fn take_thrown_frames(&mut self, thrown: &Value) -> Vec<Site> {
        match self.debugger.thrown.take() {
            Some((last, frames)) if same_value(&last, thrown) => frames,
            _ => Vec::new(),
        }
    }

    // frames as the protocol's stack trace lists them
    pub(super) fn stack_frames_json(&mut self, frames: &[Site]) -> Vec<Json> {
        frames
            .iter()
            .map(|(code, pc)| {
                let script = self.script_of(code);
                let script = &self.debugger.scripts[script];
                let (line, column) = script.location(statement_offset(code, *pc));
                json!({
                    "functionName": &*code.name,
                    "scriptId": script.id,
                    "url": script.url,
                    "lineNumber": line,
                    "columnNumber": column,
                })
            })
            .collect()
    }

    fn location_json(&self, index: usize, offset: usize) -> Json {
        let script = &self.debugger.scripts[index];
        let (line, column) = script.location(offset);
        json!({ "scriptId": script.id, "lineNumber": line, "columnNumber": column })
    }

    // sets a breakpoint in one script at the first statement at or after its place
    fn resolve_in(&mut self, index: usize, breakpoint: &Breakpoint) -> Option<Json> {
        let script = &self.debugger.scripts[index];
        let matches = match &breakpoint.target {
            Target::Url(url) => !url.is_empty() && *url == script.url,
            Target::Script(id) => *id == script.id,
            Target::Unmatched => false,
        };
        if !matches {
            return None;
        }
        let from = script.offset(breakpoint.line, breakpoint.column)?;
        let statements = script.statements();
        let first = statements.iter().find(|(offset, _, _)| *offset >= from)?.0;
        for (_, code, pc) in statements.iter().filter(|(offset, _, _)| *offset == first) {
            self.add_resolved(code, *pc, breakpoint.id.clone());
        }
        Some(self.location_json(index, first))
    }

    fn add_resolved(&mut self, code: &Rc<Code>, pc: u32, id: String) {
        let key = (Rc::as_ptr(code) as usize, pc);
        let entry = self
            .debugger
            .resolved
            .entry(key)
            .or_insert_with(|| (code.clone(), Vec::new()));
        if entry.1.is_empty() {
            code.breakpoints.set(code.breakpoints.get() + 1);
        }
        if !entry.1.contains(&id) {
            entry.1.push(id);
        }
    }

    // sets every breakpoint again, after some went or a one-off target was reached
    fn resolve_all(&mut self) {
        for (code, _) in std::mem::take(&mut self.debugger.resolved).into_values() {
            code.breakpoints.set(code.breakpoints.get() - 1);
        }
        self.debugger.continuing = false;
        let Some(shared) = self.debugger.shared.clone() else {
            return;
        };
        let shared = shared.borrow();
        for breakpoint in &shared.breakpoints {
            for index in 0..self.debugger.scripts.len() {
                self.resolve_in(index, breakpoint);
            }
        }
    }

    pub(crate) fn debugger_detached(&mut self, session: u64) {
        if let Some(shared) = &self.debugger.shared {
            shared.borrow_mut().forget(session);
        }
        self.resolve_all();
        self.refresh_debugger();
    }

    pub(super) fn debugger_call(&mut self, session: u64, method: &str, params: &Json) -> Answer {
        let Some(shared) = self.debugger.shared.clone() else {
            return Answer::Unknown;
        };
        let answer = match method {
            "Debugger.enable" => {
                self.refresh_debugger();
                let parsed: Vec<Json> = self
                    .debugger
                    .scripts
                    .iter()
                    .map(|script| self.script_parsed(script))
                    .collect();
                for event in &parsed {
                    shared.borrow().post(session, event);
                }
                Ok(json!({ "debuggerId": format!("ceri-{}", self.inspector_context()) }))
            }
            "Debugger.disable" => {
                self.debugger_detached(session);
                Ok(json!({}))
            }
            "Debugger.setPauseOnExceptions" => {
                let pause_on = match params["state"].as_str() {
                    Some("none") => PauseOn::None,
                    Some("caught") => PauseOn::Caught,
                    Some("uncaught") => PauseOn::Uncaught,
                    Some("all") => PauseOn::All,
                    _ => return Answer::Now(Err("Unknown pause on exceptions mode".into())),
                };
                shared.borrow_mut().pause_on = pause_on;
                Ok(json!({}))
            }
            "Debugger.setBreakpointsActive" => {
                shared.borrow_mut().inactive = params["active"] == false;
                Ok(json!({}))
            }
            "Debugger.setSkipAllPauses" => {
                shared.borrow_mut().skipping = params["skip"] == true;
                Ok(json!({}))
            }
            "Debugger.setAsyncCallStackDepth"
            | "Debugger.setBlackboxPatterns"
            | "Debugger.setBlackboxExecutionContexts"
            | "Debugger.setBlackboxedRanges" => Ok(json!({})),
            "Debugger.getScriptSource" => self
                .script_index(params)
                .map(|index| json!({ "scriptSource": &*self.debugger.scripts[index].text })),
            "Debugger.searchInContent" => self.script_index(params).map(|index| {
                let script = &self.debugger.scripts[index];
                let query = params["query"].as_str().unwrap_or_default();
                let sensitive = params["caseSensitive"] == true;
                let found = |line: &str| match sensitive {
                    true => line.contains(query),
                    false => line.to_lowercase().contains(&query.to_lowercase()),
                };
                let matches: Vec<Json> = script
                    .text
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| !query.is_empty() && found(line))
                    .map(|(number, line)| json!({ "lineNumber": number, "lineContent": line }))
                    .collect();
                json!({ "result": matches })
            }),
            "Debugger.getPossibleBreakpoints" => self.possible_breakpoints(params),
            "Debugger.setBreakpointByUrl" => self.set_breakpoint_by_url(session, params),
            "Debugger.setBreakpoint" => self.set_breakpoint(session, params),
            "Debugger.removeBreakpoint" => {
                let id = params["breakpointId"].as_str().unwrap_or_default();
                shared.borrow_mut().breakpoints.retain(|held| held.id != id);
                self.resolve_all();
                Ok(json!({}))
            }
            "Debugger.pause" => {
                self.debugger.pause_next = self.debugger.active;
                Ok(json!({}))
            }
            "Debugger.resume"
            | "Debugger.stepOver"
            | "Debugger.stepInto"
            | "Debugger.stepOut"
            | "Debugger.continueToLocation"
            | "Debugger.evaluateOnCallFrame"
            | "Debugger.setVariableValue"
            | "Debugger.restartFrame" => Err(PAUSED_ONLY.to_string()),
            _ => return Answer::Unknown,
        };
        self.refresh_debugger();
        Answer::Now(answer)
    }

    fn possible_breakpoints(&mut self, params: &Json) -> Result<Json, String> {
        let start = &params["start"];
        let index = self.script_index(start)?;
        let script = &self.debugger.scripts[index];
        let point = |location: &Json| {
            let line = location["lineNumber"].as_u64().unwrap_or(0) as u32;
            let column = location["columnNumber"].as_u64().unwrap_or(0) as u32;
            match line < script.line {
                true => Some(0),
                false => script.offset(line, column),
            }
        };
        let from = point(start).unwrap_or(script.text.len());
        let to = match params.get("end") {
            Some(end) if end["scriptId"] == start["scriptId"] => {
                point(end).unwrap_or(script.text.len())
            }
            _ => script.text.len(),
        };
        let mut offsets: Vec<usize> = script
            .statements()
            .into_iter()
            .map(|(offset, _, _)| offset)
            .filter(|offset| (from..to).contains(offset))
            .collect();
        offsets.dedup();
        let locations: Vec<Json> = offsets
            .into_iter()
            .map(|offset| self.location_json(index, offset))
            .collect();
        Ok(json!({ "locations": locations }))
    }

    fn set_breakpoint_by_url(&mut self, session: u64, params: &Json) -> Result<Json, String> {
        let line = params["lineNumber"]
            .as_u64()
            .ok_or("Invalid parameters: lineNumber")? as u32;
        let column = params["columnNumber"].as_u64().unwrap_or(0) as u32;
        let (target, named) = if let Some(url) = params["url"].as_str() {
            (Target::Url(url.to_string()), url.to_string())
        } else if let Some(pattern) = params["urlRegex"].as_str() {
            (Target::Unmatched, format!("/{pattern}/"))
        } else if let Some(hash) = params["scriptHash"].as_str() {
            (Target::Unmatched, hash.to_string())
        } else {
            return Err("Either url or urlRegex must be specified.".into());
        };
        let breakpoint = Breakpoint {
            id: format!("1:{line}:{column}:{named}"),
            session,
            target,
            line,
            column,
            condition: params["condition"].as_str().unwrap_or_default().to_string(),
        };
        let locations: Vec<Json> = (0..self.debugger.scripts.len())
            .filter_map(|index| self.resolve_in(index, &breakpoint))
            .collect();
        let id = breakpoint.id.clone();
        self.add_breakpoint(breakpoint)?;
        Ok(json!({ "breakpointId": id, "locations": locations }))
    }

    fn set_breakpoint(&mut self, session: u64, params: &Json) -> Result<Json, String> {
        let location = &params["location"];
        let index = self.script_index(location)?;
        let line = location["lineNumber"].as_u64().unwrap_or(0) as u32;
        let column = location["columnNumber"].as_u64().unwrap_or(0) as u32;
        let script = &self.debugger.scripts[index].id;
        let breakpoint = Breakpoint {
            id: format!("{script}:{line}:{column}:"),
            session,
            target: Target::Script(script.clone()),
            line,
            column,
            condition: params["condition"].as_str().unwrap_or_default().to_string(),
        };
        let location = self
            .resolve_in(index, &breakpoint)
            .ok_or("Could not resolve breakpoint")?;
        let id = breakpoint.id.clone();
        self.add_breakpoint(breakpoint)?;
        Ok(json!({ "breakpointId": id, "actualLocation": location }))
    }

    fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), String> {
        let shared = self
            .debugger
            .shared
            .clone()
            .ok_or("Debugger agent is not enabled")?;
        let mut shared = shared.borrow_mut();
        if shared
            .breakpoints
            .iter()
            .any(|held| held.id == breakpoint.id)
        {
            return Err("Breakpoint at specified location already exists.".into());
        }
        shared.breakpoints.push(breakpoint);
        Ok(())
    }

    pub(super) fn debugger_paused(&self) -> bool {
        self.debugger.paused
    }

    fn mirror_frames(&mut self, frames: &[Box<Frame>], base: usize) {
        let mirror = &mut self.debugger.mirror;
        let keep = mirror
            .len()
            .min((base + frames.len()).saturating_sub(1))
            .max(base);
        mirror.truncate(keep);
        for frame in &frames[keep - base..] {
            mirror.push(Snapshot {
                code: frame.code.clone(),
                pc: frame.pc,
                scope: frame.scope.clone(),
                this: frame.this.clone(),
                catches: frame.catches(),
            });
        }
    }

    // before each op: pauses at a breakpoint, a debugger statement, the statement a step or a
    // pause request was waiting for
    pub(super) fn debug_step(&mut self, frames: &[Box<Frame>], base: usize) {
        self.mirror_frames(frames, base);
        if self.debugger.paused {
            return;
        }
        self.debugger.polled = self.debugger.polled.wrapping_add(1);
        if self.debugger.polled.is_multiple_of(POLL_STEPS) {
            self.poll_pause();
        }
        let frame = frames.last().expect("a frame is running");
        let (code, pc) = (&frame.code, frame.pc);
        let mut hit = Vec::new();
        let mut stop = false;
        if self.debugger.breaking && code.breakpoints.get() > 0 {
            let key = (Rc::as_ptr(code) as usize, pc as u32);
            if let Some((_, ids)) = self.debugger.resolved.get(&key) {
                let ids = ids.clone();
                stop = self.debugger.continuing && ids.iter().any(String::is_empty);
                for id in ids.into_iter().filter(|id| !id.is_empty()) {
                    if self.condition_holds(&id) {
                        hit.push(id);
                    }
                }
            }
        }
        let depth = self.debugger.mirror.len();
        let stepped = self.debugger.pause_next
            || match self.debugger.stepping {
                Some((Step::Into, _)) => true,
                Some((Step::Over, from)) => depth <= from,
                Some((Step::Out, from)) => depth < from,
                None => false,
            };
        let statement = stepped
            && code
                .positions
                .binary_search_by_key(&(pc as u32), |(at, _)| *at)
                .is_ok();
        let statement_op = self.debugger.breaking && matches!(code.ops.get(pc), Some(Op::Debugger));
        if stop || statement || statement_op || !hit.is_empty() {
            self.pause("other", None, hit);
        }
    }

    // a thrown value on its way to a handler, or out of every run when no frame catches it
    pub(super) fn debug_thrown(&mut self, frames: &[Box<Frame>], base: usize, thrown: &Value) {
        if self.debugger.paused {
            return;
        }
        self.mirror_frames(frames, base);
        let caught = self.debugger.mirror.iter().any(|snapshot| snapshot.catches);
        let wanted = match self.debugger.pause_on {
            PauseOn::None => false,
            PauseOn::Caught => caught,
            PauseOn::Uncaught => !caught,
            PauseOn::All => true,
        };
        // a value thrown out of a nested run is thrown again in the run around it
        if !wanted
            || self
                .debugger
                .last_thrown
                .as_ref()
                .is_some_and(|last| same_value(last, thrown))
        {
            return;
        }
        self.debugger.last_thrown = Some(thrown.clone());
        let mut data = self.remote_object(thrown, "backtrace", false);
        data["uncaught"] = (!caught).into();
        self.pause("exception", Some(data), Vec::new());
    }

    // the outermost run ended, and with it any step under way
    pub(super) fn debug_task_ended(&mut self) {
        self.debugger.stepping = None;
        self.debugger.last_thrown = None;
    }

    fn condition_holds(&mut self, id: &str) -> bool {
        let condition = self.debugger.shared.as_ref().and_then(|shared| {
            let shared = shared.borrow();
            let breakpoint = shared.breakpoints.iter().find(|held| held.id == id)?;
            Some(breakpoint.condition.clone())
        });
        let Some(condition) = condition else {
            return false;
        };
        if condition.trim().is_empty() {
            return true;
        }
        let Some(top) = self.debugger.mirror.last().cloned() else {
            return false;
        };
        self.debugger.paused = true;
        let outcome = self.evaluate_in_frame(&condition, &top);
        self.debugger.paused = false;
        match outcome {
            Ok(value) => value.truthy(),
            Err(error) => {
                self.take_thrown(error);
                false
            }
        }
    }

    fn poll_pause(&mut self) {
        let Some(source) = self.pause_source() else {
            return;
        };
        while let Some(incoming) = source.poll() {
            match incoming {
                Incoming::Message { session, text } => {
                    let message = serde_json::from_str::<Json>(&text).unwrap_or_default();
                    match (message["method"].as_str(), message["id"].as_u64()) {
                        (Some("Debugger.pause"), Some(id)) if self.debugging(session) => {
                            self.debugger.pause_next = true;
                            self.answer(session, id, Answer::Now(Ok(json!({}))));
                        }
                        _ => source.defer(session, text),
                    }
                }
                Incoming::Detached(session) => self.debugger_detached(session),
                Incoming::Closed => return,
            }
        }
    }

    fn pause_source(&self) -> Option<Rc<dyn PauseSource>> {
        self.debugger.shared.as_ref()?.borrow().source.clone()
    }

    fn debugging(&self, session: u64) -> bool {
        self.debugger.shared.as_ref().is_some_and(|shared| {
            shared
                .borrow()
                .sessions
                .iter()
                .any(|(id, _)| *id == session)
        })
    }

    fn answer(&self, session: u64, id: u64, answer: Answer) {
        let Some(shared) = &self.debugger.shared else {
            return;
        };
        let message = match answer {
            Answer::Now(Ok(result)) => json!({ "id": id, "result": result }),
            Answer::Now(Err(message)) => {
                json!({ "id": id, "error": { "code": -32000, "message": message } })
            }
            Answer::Later(token) => {
                shared.borrow_mut().waiting.push((session, id, token));
                return;
            }
            Answer::Unknown => {
                json!({ "id": id, "error": { "code": -32601, "message": "method wasn't found" } })
            }
        };
        shared.borrow().post(session, &message);
    }

    // holds the thread, answering the sessions from the stopped frames until one resumes
    fn pause(&mut self, reason: &str, data: Option<Json>, hit: Vec<String>) {
        let Some(source) = self.pause_source() else {
            return;
        };
        if self.debugger.continuing {
            self.resolve_all();
        }
        self.debugger.paused = true;
        self.debugger.pause_next = false;
        self.debugger.stepping = None;
        let frames: Vec<Json> = self
            .debugger
            .mirror
            .clone()
            .iter()
            .rev()
            .enumerate()
            .map(|(index, snapshot)| self.call_frame(index, snapshot))
            .collect();
        let mut params = json!({ "callFrames": frames, "reason": reason, "hitBreakpoints": hit });
        if let Some(data) = data {
            params["data"] = data;
        }
        self.broadcast(json!({ "method": "Debugger.paused", "params": params }));
        loop {
            match source.next() {
                Incoming::Message { session, text } => {
                    if self.answer_paused(&*source, session, text) {
                        break;
                    }
                }
                Incoming::Detached(session) => {
                    self.debugger_detached(session);
                    if !self.debugger.active {
                        break;
                    }
                }
                Incoming::Closed => break,
            }
        }
        self.inspector_call(
            0,
            "Runtime.releaseObjectGroup",
            &json!({ "objectGroup": "backtrace" }),
        );
        self.debugger.paused = false;
        self.broadcast(json!({ "method": "Debugger.resumed", "params": {} }));
    }

    // one message while paused; true when it resumes
    fn answer_paused(&mut self, source: &dyn PauseSource, session: u64, text: String) -> bool {
        let message = serde_json::from_str::<Json>(&text).unwrap_or_default();
        let (Some(id), Some(method)) = (message["id"].as_u64(), message["method"].as_str()) else {
            source.defer(session, text);
            return false;
        };
        let params = &message["params"];
        let step = match method {
            "Debugger.stepInto" => Some(Step::Into),
            "Debugger.stepOver" => Some(Step::Over),
            "Debugger.stepOut" => Some(Step::Out),
            _ => None,
        };
        let deferred = matches!(
            method,
            "Debugger.enable"
                | "Runtime.enable"
                | "Runtime.disable"
                | "Runtime.runIfWaitingForDebugger"
        );
        if deferred || !self.debugging(session) {
            source.defer(session, text);
            return false;
        }
        let (answer, resume) = match method {
            "Debugger.resume" => (Answer::Now(Ok(json!({}))), true),
            _ if step.is_some() => {
                self.debugger.stepping = step.map(|step| (step, self.debugger.mirror.len()));
                (Answer::Now(Ok(json!({}))), true)
            }
            "Debugger.continueToLocation" => {
                let answer = self.continue_to(params);
                let resume = answer.is_ok();
                (Answer::Now(answer), resume)
            }
            "Debugger.evaluateOnCallFrame" => (self.evaluate_on_call_frame(params), false),
            "Debugger.pause" => (Answer::Now(Ok(json!({}))), false),
            "Debugger.setVariableValue" | "Debugger.restartFrame" => (
                Answer::Now(Err(format!("{method} is not supported"))),
                false,
            ),
            "Overlay.setPausedInDebuggerMessage" => (Answer::Now(Ok(json!({}))), false),
            method
                if method.starts_with("Debugger.")
                    || method.starts_with("Runtime.")
                    || method == "Page.getResourceContent" =>
            {
                let answer = self.inspector_call(session, method, params);
                (answer, !self.debugger.active)
            }
            _ => {
                source.defer(session, text);
                return false;
            }
        };
        self.answer(session, id, answer);
        resume
    }

    fn continue_to(&mut self, params: &Json) -> Result<Json, String> {
        let location = &params["location"];
        let index = self.script_index(location)?;
        let breakpoint = Breakpoint {
            id: String::new(),
            session: 0,
            target: Target::Script(self.debugger.scripts[index].id.clone()),
            line: location["lineNumber"].as_u64().unwrap_or(0) as u32,
            column: location["columnNumber"].as_u64().unwrap_or(0) as u32,
            condition: String::new(),
        };
        self.resolve_in(index, &breakpoint)
            .ok_or("Could not resolve location")?;
        self.debugger.continuing = true;
        Ok(json!({}))
    }

    fn call_frame(&mut self, index: usize, snapshot: &Snapshot) -> Json {
        let script = self.script_of(&snapshot.code);
        let code = &snapshot.code;
        let location = self.location_json(script, statement_offset(code, snapshot.pc));
        let module = self.debugger.scripts[script].module;
        let url = self.debugger.scripts[script].url.clone();
        let scopes = self.scope_chain(snapshot, module);
        let this = self.remote_object(&snapshot.this, "backtrace", false);
        json!({
            "callFrameId": index.to_string(),
            "functionName": &*code.name,
            "location": location,
            "url": url,
            "scopeChain": scopes,
            "this": this,
            "canBeRestarted": false,
        })
    }

    // the frame's scopes innermost first, as objects holding what each scope's names hold now
    fn scope_chain(&mut self, snapshot: &Snapshot, module: bool) -> Vec<Json> {
        let mut chain = Vec::new();
        let mut local = false;
        let mut scope = snapshot.scope.clone();
        while let Some(current) = scope {
            let (layout, slots, parent) = {
                let current = current.borrow();
                (
                    current.layout.clone(),
                    current.slots.clone(),
                    current.parent.clone(),
                )
            };
            // the scope holding a named function expression's own name is not one v8 lists
            let own_name = snapshot
                .code
                .self_scope
                .as_ref()
                .is_some_and(|own| Rc::ptr_eq(own, &layout));
            let kind = if parent.is_none() {
                if module { "module" } else { "script" }
            } else if !local && Rc::ptr_eq(&layout, &snapshot.code.layout) {
                local = true;
                "local"
            } else if local {
                "closure"
            } else {
                "block"
            };
            let entries: Vec<(String, Value)> = layout
                .names
                .iter()
                .zip(slots)
                .filter(|(name, _)| !name.is_empty() && !name.starts_with('%'))
                .map(|(name, value)| (name.to_string(), value.resolve()))
                .collect();
            if (!entries.is_empty() && !own_name) || kind == "local" {
                // a scope's variables are its only properties, as a frontend lists them
                let object = new_object(entries.into_iter().collect());
                object.borrow_mut().set_prototype(None);
                let object = self.remote_object(&Value::Object(object), "backtrace", false);
                chain.push(json!({ "type": kind, "object": object }));
            }
            scope = parent;
        }
        let global = self.remote_object(&Value::Window, "backtrace", false);
        chain.push(json!({ "type": "global", "object": global }));
        chain
    }

    fn evaluate_on_call_frame(&mut self, params: &Json) -> Answer {
        let mirror = &self.debugger.mirror;
        let Some(snapshot) = params["callFrameId"]
            .as_str()
            .and_then(|id| id.parse::<usize>().ok())
            .and_then(|index| mirror.len().checked_sub(index + 1))
            .map(|index| mirror[index].clone())
        else {
            return Answer::Now(Err("Invalid call frame id".into()));
        };
        let group = params["objectGroup"]
            .as_str()
            .unwrap_or("console")
            .to_string();
        if params["throwOnSideEffect"] == true {
            let thrown = self.throw_error("EvalError", "Possible side-effect in debug-evaluate");
            let thrown = self.take_thrown(thrown);
            return Answer::Now(Ok(self.thrown_json(&thrown, &group)));
        }
        let installed = match params["includeCommandLineAPI"] == true {
            true => self.install_command_line(),
            false => Vec::new(),
        };
        let source = params["expression"].as_str().unwrap_or_default();
        let outcome = self.evaluate_in_frame(source, &snapshot);
        for name in installed {
            self.global.borrow_mut().values.remove(&name);
        }
        Answer::Now(Ok(match outcome {
            Ok(value) => self.result_json(
                &value,
                &group,
                params["returnByValue"] == true,
                params["generatePreview"] == true,
            ),
            Err(error) => {
                let thrown = self.take_thrown(error);
                self.thrown_json(&thrown, &group)
            }
        }))
    }

    // runs source as an expression, or failing that as statements, seeing the frame's names
    fn evaluate_in_frame(
        &mut self,
        source: &str,
        snapshot: &Snapshot,
    ) -> Result<Value, ScriptError> {
        let mut layers = Vec::new();
        let mut scope = snapshot.scope.clone();
        while let Some(current) = scope {
            let current = current.borrow();
            layers.push(current.layout.clone());
            scope = current.parent.clone();
        }
        layers.reverse();
        // the line break keeps a trailing line comment from swallowing the parenthesis
        let body = format!("return (\n{source}\n)");
        let (function, text) = parser::parse_function_source("", &body, false, false)
            .or_else(|_| parser::parse_function_source("", source, false, false))
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        let code = compiler::Compiler::new(&text)
            .compile_function_within(&function, &layers)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        let closure = self.new_closure(code, snapshot.scope.clone(), None);
        let this = match &snapshot.this {
            Value::Uninitialized => Value::Undefined,
            this => this.clone(),
        };
        self.call_function(closure, this, Vec::new())
    }
}

fn each_code(code: &Rc<Code>, visit: &mut impl FnMut(&Rc<Code>)) {
    visit(code);
    for child in &code.children {
        each_code(child, visit);
    }
    for class in &code.classes {
        each_code(&class.constructor, visit);
    }
}

// where in its text the statement an op belongs to starts
fn statement_offset(code: &Code, pc: usize) -> usize {
    let statement = code.positions.partition_point(|(at, _)| *at as usize <= pc);
    match statement {
        0 => code.source.as_ref().map_or(0, |(_, start, _)| *start),
        statement => code.positions[statement - 1].1 as usize,
    }
}

fn text_address(text: &Rc<str>) -> usize {
    Rc::as_ptr(text) as *const u8 as usize
}

// a byte offset in a text as the protocol's line and UTF-16 column, in the document the text
// starts in at the given line and column
fn locate(text: &str, lines: &[usize], start: (u32, u32), offset: usize) -> (u32, u32) {
    let line = lines.partition_point(|start| *start <= offset) - 1;
    let column = utf16_length(&text[lines[line]..offset]);
    match line {
        0 => (start.0, start.1 + column),
        _ => (start.0 + line as u32, column),
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    for (index, byte) in bytes.iter().enumerate() {
        let crlf = *byte == b'\r' && bytes.get(index + 1) == Some(&b'\n');
        if *byte == b'\n' || (*byte == b'\r' && !crlf) {
            starts.push(index + 1);
        }
    }
    starts
}

fn utf16_length(text: &str) -> u32 {
    text.chars()
        .map(|character| character.len_utf16() as u32)
        .sum()
}

fn hash(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(text: &str, line: u32, column: u32) -> Script {
        Script {
            id: "1".into(),
            url: String::new(),
            text: text.into(),
            line,
            column,
            lines: line_starts(text),
            code: None,
            module: false,
        }
    }

    #[test]
    fn places_count_lines_as_the_protocol_does_and_columns_in_utf16() {
        let inline = script("a\r\nb\rc\n\u{1f600}d", 4, 8);
        assert_eq!(inline.location(0), (4, 8));
        assert_eq!(inline.location(3), (5, 0));
        assert_eq!(inline.location(5), (6, 0));
        assert_eq!(inline.location(11), (7, 2));
        assert_eq!(inline.offset(4, 9), Some(1));
        assert_eq!(inline.offset(4, 2), Some(0));
        assert_eq!(inline.offset(7, 2), Some(11));
        assert_eq!(inline.offset(7, 40), Some(12));
        assert_eq!(inline.offset(3, 0), None);
        assert_eq!(inline.offset(8, 0), None);
    }
}
