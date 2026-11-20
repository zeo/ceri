// devtools protocol sessions on a browser's page: the Page domain here, the Runtime and Debugger
// domains routed to the page's script runtime, the DOM, Overlay, CSS and Network domains in their own
// modules, and the events they owe each session

mod dom;
mod network;
mod overlay;
mod styles;

use super::{AppUrl, Browser, NavigationError, Size};
use crate::css::ColorScheme;
use crate::dom::NodeId;
use crate::script::{Answer, PauseSource, ScriptRuntime, SharedDebug, frame_id};
use serde_json::{Value as Json, json};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

// a protocol error's code and message
type Failure = (i64, String);

#[derive(Default)]
pub(super) struct Inspection {
    sessions: Vec<Session>,
    waiting: Vec<Waiting>,
    highlight: Option<overlay::Highlight>,
    picking: Option<overlay::Picking>,
    sheets: styles::Sheets,
    network: network::Kept,
    // the debugger's sessions and breakpoints, which outlive each page's runtime
    pub(super) debug: SharedDebug,
}

impl Inspection {
    pub(super) fn attached(&self) -> bool {
        !self.sessions.is_empty()
    }

    // what a frame shows over the page for the frontend
    pub(super) fn fills(&self) -> Option<&[overlay::Fill]> {
        let fills = &self.highlight.as_ref()?.fills;
        (!fills.is_empty()).then_some(fills.as_slice())
    }
}

struct Session {
    id: u64,
    outgoing: Sender<String>,
    runtime: bool,
    page: bool,
    css: bool,
    network: bool,
    // the execution context this session last heard of, and the first console entry it has not
    context: Option<u64>,
    seen: u64,
    dom: dom::Mirror,
    // the document whose lifecycle the session was told of, while it asks for lifecycle events
    lifecycle: Option<Option<u64>>,
    // the named worlds the session made, each a context over the page's own realm
    worlds: Vec<String>,
    // scripts for each new document, with their identifiers and worlds
    scripts: Vec<(u64, String, String)>,
}

// an answer held until a promise settles in the context that made it
struct Waiting {
    context: u64,
    token: u64,
    session: u64,
    id: u64,
}

impl Browser {
    pub fn attach_inspector(&mut self, session: u64, outgoing: Sender<String>) {
        if let Some(page) = &mut self.page {
            page.runtime.capture_console();
        }
        self.inspection.sessions.push(Session {
            id: session,
            outgoing,
            runtime: false,
            page: false,
            css: false,
            network: false,
            context: None,
            seen: 0,
            dom: dom::Mirror::default(),
            lifecycle: None,
            worlds: Vec::new(),
            scripts: Vec::new(),
        });
    }

    // answers a call the window's host handled for a session
    pub(crate) fn answer_inspector(&self, session: u64, id: u64, answer: Result<Json, Failure>) {
        let response = match answer {
            Ok(result) => json!({ "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        };
        self.send(session, response);
    }

    pub(crate) fn document_scroll(&self) -> (f32, f32) {
        self.page
            .as_ref()
            .map_or((0.0, 0.0), |page| page.runtime.document_scroll_offset())
    }

    // where a page paused in its debugger reads the window's devtools messages
    pub(crate) fn set_pause_source(&mut self, source: Rc<dyn PauseSource>) {
        self.inspection.debug.borrow_mut().set_source(source);
    }

    pub fn detach_inspector(&mut self, session: u64) {
        match &mut self.page {
            Some(page) => page.runtime.debugger_detached(session),
            None => self.inspection.debug.borrow_mut().forget(session),
        }
        self.inspection.sessions.retain(|held| held.id != session);
        self.inspection
            .waiting
            .retain(|held| held.session != session);
        let inspection = &mut self.inspection;
        if inspection
            .picking
            .as_ref()
            .is_some_and(|held| held.session == session)
        {
            inspection.picking = None;
        }
        if inspection.sessions.is_empty() {
            inspection.highlight = None;
            if let Some(page) = &mut self.page {
                page.runtime.forced_pseudo_classes.clear();
            }
        }
        self.stop_recording();
    }

    // while a session picks a node, the pointer moves the highlight instead of reaching the
    // page; true when the pointer was taken
    pub fn pick_hover(&mut self, viewport: Size, x: f32, y: f32) -> Result<bool, NavigationError> {
        let Some(picking) = &self.inspection.picking else {
            return Ok(false);
        };
        let (session, Some(config)) = (picking.session, picking.config.clone()) else {
            return Ok(true);
        };
        let node = self.element_at(viewport, x, y)?;
        let highlighted = self
            .inspection
            .highlight
            .as_ref()
            .and_then(|held| held.node());
        if let Some(node) = node.filter(|node| Some(*node) != highlighted) {
            self.inspection.highlight = Some(overlay::following(node, config));
            if let Some(page) = &mut self.page {
                overlay::refresh(&mut self.inspection.highlight, page);
            }
            self.report_pick(session, node, false);
        }
        Ok(true)
    }

    pub fn pick_press(&mut self, viewport: Size, x: f32, y: f32) -> Result<bool, NavigationError> {
        let Some(picking) = &mut self.inspection.picking else {
            return Ok(false);
        };
        picking.pressed = true;
        let session = picking.session;
        if let Some(node) = self.element_at(viewport, x, y)? {
            self.report_pick(session, node, true);
        }
        Ok(true)
    }

    // the release and click after a picking press stay from the page too, and the click ends
    // the pick
    pub fn pick_release(&mut self, clicked: bool) -> bool {
        if !self
            .inspection
            .picking
            .as_ref()
            .is_some_and(|held| held.pressed)
        {
            return false;
        }
        if clicked {
            self.inspection.picking = None;
            self.inspection.highlight = None;
        }
        true
    }

    fn element_at(
        &self,
        viewport: Size,
        x: f32,
        y: f32,
    ) -> Result<Option<NodeId>, NavigationError> {
        let node = self.layout(viewport)?.hit_test(x, y);
        let Some(page) = &self.page else {
            return Ok(None);
        };
        let document = page.document.borrow();
        Ok(node.map(|node| match document.is_text(node) {
            true => document.parent(node).unwrap_or(node),
            false => node,
        }))
    }

    fn report_pick(&mut self, session: u64, node: NodeId, pressed: bool) {
        let url = self.document_url();
        let Self {
            inspection, page, ..
        } = self;
        let held = inspection
            .sessions
            .iter_mut()
            .find(|held| held.id == session);
        if let (Some(held), Some(page)) = (held, page.as_ref()) {
            overlay::report(held, page, &url, node, pressed);
        }
    }

    // answers one protocol message, and gives back the url it asks the window to load
    pub fn inspect(&mut self, session: u64, message: &str) -> Option<String> {
        let Ok(message) = serde_json::from_str::<Json>(message) else {
            self.send(session, error(0, -32700, "Message must be a valid JSON"));
            return None;
        };
        let Some(id) = message["id"].as_u64() else {
            self.send(
                session,
                error(0, -32600, "Message must have integer 'id' property"),
            );
            return None;
        };
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let mut navigate = None;
        let answer = match method {
            "Runtime.enable" | "Runtime.disable" | "Page.enable" | "Page.disable" => {
                if let Some(held) = self.session(session) {
                    match method {
                        "Runtime.enable" if !held.runtime => {
                            held.runtime = true;
                            held.context = None;
                        }
                        "Runtime.disable" => held.runtime = false,
                        "Page.enable" => held.page = true,
                        "Page.disable" => held.page = false,
                        _ => {}
                    }
                }
                Some(Ok(json!({})))
            }
            // a page has no child targets, keeps no browser log apart from its console, and is
            // focused for its sessions already
            "Runtime.runIfWaitingForDebugger"
            | "Target.setAutoAttach"
            | "Log.enable"
            | "Log.disable"
            | "Inspector.enable"
            | "Inspector.disable"
            | "Emulation.setFocusEmulationEnabled" => Some(Ok(json!({}))),
            "Emulation.setEmulatedMedia" => Some(self.emulate_media(params)),
            // the engine's phase profiler, which collects from the moment it is enabled
            "Performance.enable" | "Performance.disable" => {
                crate::profile::enable(method == "Performance.enable");
                Some(Ok(json!({})))
            }
            "Performance.getMetrics" => Some(Ok(json!({ "metrics": performance_metrics() }))),
            "Page.setLifecycleEventsEnabled" => {
                let enabled = params["enabled"] == true;
                if let Some(held) = self.session(session) {
                    held.lifecycle = enabled.then_some(None);
                }
                Some(Ok(json!({})))
            }
            "Page.createIsolatedWorld" => {
                let name = params["worldName"].as_str().unwrap_or_default().to_string();
                let origin = self.frame()["securityOrigin"].clone();
                let context = self
                    .page
                    .as_ref()
                    .map(|page| page.runtime.inspector_context());
                match (self.session(session), context) {
                    (Some(held), Some(context)) => {
                        let world = add_world(held, &name);
                        if held.runtime {
                            announce_world(held, context, world, &name, &origin);
                        }
                        Some(Ok(
                            json!({ "executionContextId": world_id(context, world) }),
                        ))
                    }
                    _ => Some(Err((-32000, "Could not create isolated world".into()))),
                }
            }
            "Page.addScriptToEvaluateOnNewDocument" => {
                let source = params["source"].as_str().unwrap_or_default().to_string();
                let world = params["worldName"].as_str().unwrap_or_default().to_string();
                let immediately = params["runImmediately"] == true && world.is_empty();
                let identifier = NEXT_SCRIPT.fetch_add(1, Ordering::Relaxed);
                if let Some(held) = self.session(session) {
                    if !world.is_empty() {
                        add_world(held, &world);
                    }
                    held.scripts.push((identifier, source.clone(), world));
                }
                if immediately && let Some(page) = &mut self.page {
                    let _ = page.runtime.execute_reporting(&source);
                }
                Some(Ok(json!({ "identifier": identifier.to_string() })))
            }
            "Page.removeScriptToEvaluateOnNewDocument" => {
                let identifier = params["identifier"].as_str().unwrap_or_default();
                if let Some(held) = self.session(session) {
                    held.scripts.retain(|(id, ..)| id.to_string() != identifier);
                }
                Some(Ok(json!({})))
            }
            "Page.getResourceTree" => Some(Ok(json!({
                "frameTree": { "frame": self.frame(), "resources": [] },
            }))),
            "Page.getFrameTree" => Some(Ok(json!({ "frameTree": { "frame": self.frame() } }))),
            "Page.reload" => {
                navigate = self.current_url.as_ref().map(ToString::to_string);
                Some(Ok(json!({})))
            }
            "Page.navigate" => match params["url"].as_str() {
                Some(url) => {
                    navigate = Some(url.to_string());
                    // the document the navigation makes, as its frameNavigated will name it
                    let loader = self.next_document_id.to_string();
                    Some(Ok(json!({ "frameId": frame_id(), "loaderId": loader })))
                }
                None => Some(Err((-32602, "Invalid parameters: url".to_string()))),
            },
            method
                if method.starts_with("Runtime.")
                    || method.starts_with("Debugger.")
                    || method == "Page.getResourceContent" =>
            {
                if method == "Debugger.enable" {
                    // the context the scripts belong to is announced before them
                    self.flush_inspector();
                    if let Some(held) = self.session(session) {
                        let outgoing = held.outgoing.clone();
                        self.inspection.debug.borrow_mut().enable(session, outgoing);
                    }
                }
                self.answer_runtime(session, id, method, params)
            }
            method if method.starts_with("Overlay.") => {
                let Self {
                    inspection, page, ..
                } = self;
                let Inspection {
                    sessions,
                    highlight,
                    picking,
                    ..
                } = inspection;
                let held = sessions.iter().find(|held| held.id == session);
                let answer = match (held, page) {
                    (Some(held), Some(page)) => overlay::answer(
                        highlight, picking, session, &held.dom, page, method, params,
                    ),
                    _ => Some(Err((-32000, "Document is not available".into()))),
                };
                Some(answer.unwrap_or_else(|| Err((-32601, format!("'{method}' wasn't found")))))
            }
            method if method.starts_with("DOM.") => {
                // a new document resets what the session holds before the call reads it
                self.flush_inspector();
                let url = self.document_url();
                let mut events = Vec::new();
                let Self {
                    inspection, page, ..
                } = self;
                let held = inspection
                    .sessions
                    .iter_mut()
                    .find(|held| held.id == session);
                let answer = match (held, page) {
                    (Some(held), Some(page)) => {
                        dom::answer(&mut held.dom, page, &url, method, params, &mut events)
                    }
                    _ => Some(Err((-32000, "Document is not available".into()))),
                };
                for event in events {
                    self.send(session, event);
                }
                Some(answer.unwrap_or_else(|| Err((-32601, format!("'{method}' wasn't found")))))
            }
            method if method.starts_with("CSS.") => {
                self.flush_inspector();
                Some(self.answer_css(session, method, params))
            }
            method if method.starts_with("Network.") => {
                self.flush_inspector();
                Some(self.answer_network(session, method, params))
            }
            _ => Some(Err((-32601, format!("'{method}' wasn't found")))),
        };
        // the events a call causes reach the session before its answer, as in chromium
        self.flush_inspector();
        if let Some(answer) = answer {
            let response = match answer {
                Ok(result) => json!({ "id": id, "result": result }),
                Err((code, message)) => error(id, code, &message),
            };
            self.send(session, response);
        }
        navigate
    }

    fn answer_runtime(
        &mut self,
        session: u64,
        id: u64,
        method: &str,
        params: &Json,
    ) -> Option<Result<Json, Failure>> {
        let Some(page) = &mut self.page else {
            return Some(Err((
                -32000,
                "Cannot find default execution context".into(),
            )));
        };
        match page.runtime.inspector_call(session, method, params) {
            Answer::Now(answer) => Some(answer.map_err(|message| (-32000, message))),
            Answer::Later(token) => {
                self.inspection.waiting.push(Waiting {
                    context: page.runtime.inspector_context(),
                    token,
                    session,
                    id,
                });
                None
            }
            Answer::Unknown => Some(Err((-32601, format!("'{method}' wasn't found")))),
        }
    }

    // tells the sessions of a document about to run its scripts, so its frame and context reach
    // them before the scripts it parses and any pause in those, and runs their scripts for it
    pub(super) fn announce_document(&mut self, url: &AppUrl, runtime: &mut ScriptRuntime) {
        let frame = self.frame_at(Some(url), self.next_document_id);
        let context = runtime.inspector_context();
        for session in &mut self.inspection.sessions {
            enter_context(session, context, &frame);
        }
        let sources: Vec<String> = self
            .inspection
            .sessions
            .iter()
            .flat_map(|session| &session.scripts)
            .filter(|(_, _, world)| world.is_empty())
            .map(|(_, source, _)| source.clone())
            .collect();
        for source in sources {
            let _ = runtime.execute_reporting(&source);
        }
    }

    // prefers-color-scheme and prefers-reduced-motion follow the session; a media type or any
    // other preference the engine can't show is refused rather than ignored
    fn emulate_media(&mut self, params: &Json) -> Result<Json, Failure> {
        let unsupported = |what: &str| Err((-32000, format!("Emulating {what} is not supported")));
        if !matches!(params["media"].as_str(), None | Some("" | "screen")) {
            return unsupported("a media type");
        }
        let mut preferences = self.user_preferences();
        for feature in params["features"].as_array().into_iter().flatten() {
            let value = feature["value"].as_str().unwrap_or_default();
            match (feature["name"].as_str().unwrap_or_default(), value) {
                (_, "") => {}
                ("prefers-color-scheme", "light") => preferences.color_scheme = ColorScheme::Light,
                ("prefers-color-scheme", "dark") => preferences.color_scheme = ColorScheme::Dark,
                ("prefers-reduced-motion", "reduce") => preferences.reduced_motion = true,
                ("prefers-reduced-motion", "no-preference") => preferences.reduced_motion = false,
                ("forced-colors", "none") | ("prefers-contrast", "no-preference") => {}
                (name, _) => return unsupported(name),
            }
        }
        self.set_user_preferences(preferences);
        Ok(json!({}))
    }

    // sends each session what changed since the last call: a new document's context, console
    // entries, and answers whose promises settled
    pub fn flush_inspector(&mut self) {
        if !self.inspection.attached() {
            return;
        }
        let frame = self.frame();
        let url = self.document_url();
        let Some(page) = &mut self.page else {
            return;
        };
        let context = page.runtime.inspector_context();
        let inspection = &mut self.inspection;
        let paused = inspection.debug.borrow_mut().take_waiting();
        for (session, id, token) in paused {
            inspection.waiting.push(Waiting {
                context,
                token,
                session,
                id,
            });
        }
        overlay::refresh(&mut inspection.highlight, page);
        for settled in page.runtime.inspector_settled() {
            let (token, answer) = settled;
            if let Some(index) = inspection
                .waiting
                .iter()
                .position(|held| held.context == context && held.token == token)
            {
                let held = inspection.waiting.remove(index);
                let response = match answer {
                    Ok(result) => json!({ "id": held.id, "result": result }),
                    Err(message) => error(held.id, -32000, &message),
                };
                send_to(&inspection.sessions, held.session, response);
            }
        }
        inspection.waiting.retain(|held| {
            let alive = held.context == context;
            if !alive {
                let response = error(held.id, -32000, "Execution context was destroyed.");
                send_to(&inspection.sessions, held.session, response);
            }
            alive
        });
        for session in &mut inspection.sessions {
            if session.context != Some(context) {
                enter_context(session, context, &frame);
            }
            let mut events = Vec::new();
            session
                .dom
                .reconcile(&page.document.borrow(), &url, &mut events);
            for event in events {
                let _ = session.outgoing.send(event.to_string());
            }
            if session.runtime {
                let (events, next) = page.runtime.console_events(session.seen);
                session.seen = next;
                for event in events {
                    let _ = session.outgoing.send(event.to_string());
                }
            }
            // a document is whole once the call that loaded it returns, so its events go at once
            if let Some(told) = session.lifecycle
                && told != Some(context)
            {
                session.lifecycle = Some(Some(context));
                let timestamp = crate::netlog::now();
                for name in [
                    "init",
                    "commit",
                    "DOMContentLoaded",
                    "load",
                    "networkAlmostIdle",
                    "networkIdle",
                ] {
                    post(
                        session,
                        "Page.lifecycleEvent",
                        json!({ "frameId": frame_id(), "loaderId": frame["loaderId"], "name": name, "timestamp": timestamp }),
                    );
                }
            }
        }
        if self.inspection.sessions.iter().any(|held| held.css) {
            self.sync_sheets();
        }
        self.flush_network();
    }

    fn session(&mut self, id: u64) -> Option<&mut Session> {
        self.inspection
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
    }

    fn send(&self, session: u64, message: Json) {
        send_to(&self.inspection.sessions, session, message);
    }

    fn document_url(&self) -> String {
        self.current_url
            .as_ref()
            .map_or_else(|| "about:blank".to_string(), ToString::to_string)
    }

    fn frame(&self) -> Json {
        self.frame_at(
            self.current_url.as_ref(),
            self.current_document_id.unwrap_or_default(),
        )
    }

    fn frame_at(&self, url: Option<&AppUrl>, loader: u64) -> Json {
        json!({
            "id": frame_id(),
            "loaderId": loader.to_string(),
            "url": url.map_or_else(|| "about:blank".to_string(), ToString::to_string),
            "domainAndRegistry": "",
            "securityOrigin": url.map_or("null", |url| url.origin()),
            "mimeType": "text/html",
            "secureContextType": "Secure",
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        })
    }
}

// the session's view moves to a new document: its frame, its context and an empty DOM
fn enter_context(session: &mut Session, context: u64, frame: &Json) {
    let old = session.context.replace(context);
    if session.page && old.is_some() {
        post(
            session,
            "Page.frameNavigated",
            json!({ "frame": frame, "type": "Navigation" }),
        );
    }
    if session.runtime {
        if let Some(old) = old {
            post(
                session,
                "Runtime.executionContextDestroyed",
                json!({ "executionContextId": old, "executionContextUniqueId": old.to_string() }),
            );
            post(session, "Runtime.executionContextsCleared", json!({}));
        }
        post(
            session,
            "Runtime.executionContextCreated",
            json!({ "context": {
                "id": context,
                "origin": frame["securityOrigin"],
                "name": "",
                "uniqueId": context.to_string(),
                "auxData": { "isDefault": true, "type": "default", "frameId": frame_id() },
            }}),
        );
        for (index, name) in session.worlds.iter().enumerate() {
            announce_world(session, context, index, name, &frame["securityOrigin"]);
        }
        session.seen = 0;
    }
    if session.dom.active() {
        post(session, "DOM.documentUpdated", json!({}));
    }
    session.dom = dom::Mirror::default();
}

static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(1);

fn add_world(session: &mut Session, name: &str) -> usize {
    match session.worlds.iter().position(|held| held == name) {
        Some(index) => index,
        None => {
            session.worlds.push(name.to_string());
            session.worlds.len() - 1
        }
    }
}

// a world's id sits above any document's own context id, so each document's worlds are told
// apart and calls into them run in that document's realm
fn world_id(context: u64, world: usize) -> u64 {
    (1 << 40) | (context << 8) | (world as u64 + 1)
}

fn announce_world(session: &Session, context: u64, world: usize, name: &str, origin: &Json) {
    let id = world_id(context, world);
    post(
        session,
        "Runtime.executionContextCreated",
        json!({ "context": {
            "id": id,
            "origin": origin,
            "name": name,
            "uniqueId": id.to_string(),
            "auxData": { "isDefault": false, "type": "isolated", "frameId": frame_id() },
        }}),
    );
}

fn post(session: &Session, method: &str, params: Json) {
    let event = json!({ "method": method, "params": params });
    let _ = session.outgoing.send(event.to_string());
}

fn send_to(sessions: &[Session], id: u64, message: Json) {
    if let Some(session) = sessions.iter().find(|session| session.id == id) {
        let _ = session.outgoing.send(message.to_string());
    }
}

fn error(id: u64, code: i64, message: &str) -> Json {
    json!({ "id": id, "error": { "code": code, "message": message } })
}

// the profiler's phases as devtools metrics: the names tools already read, from the phases
// that match them, then every phase under its own name with the seconds of its own, the
// seconds with what it called, and its calls
fn performance_metrics() -> Vec<Json> {
    let phases = crate::profile::phases();
    let of = |name: &str| {
        phases
            .iter()
            .find(|phase| phase.0 == name)
            .map_or((0.0, 0), |&(_, _, total, calls)| (total, calls))
    };
    let style = phases
        .iter()
        .filter(|phase| phase.0.starts_with("style: "))
        .fold((0.0, 0), |sum, &(_, own, _, calls)| {
            (sum.0 + own, sum.1 + calls)
        });
    let metric = |name: String, value: f64| json!({ "name": name, "value": value });
    let mut metrics = vec![
        metric("LayoutCount".to_string(), of("layout").1 as f64),
        metric("LayoutDuration".to_string(), of("layout").0),
        metric("RecalcStyleCount".to_string(), style.1 as f64),
        metric("RecalcStyleDuration".to_string(), style.0),
        metric("ScriptDuration".to_string(), of("script: tick").0),
    ];
    for (name, own, total, calls) in phases {
        metrics.push(metric(format!("{name} self"), own));
        metrics.push(metric(format!("{name} total"), total));
        metrics.push(metric(format!("{name} calls"), calls as f64));
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::ResourceBundle;
    use crate::script::Incoming;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver};

    fn inspected(page: &str) -> (Browser, Receiver<String>) {
        let mut resources = ResourceBundle::default();
        resources
            .insert("/index.html", "text/html; charset=utf-8", page.as_bytes())
            .expect("page should be accepted");
        let mut browser = Browser::new(resources);
        let (outgoing, messages) = mpsc::channel();
        browser.attach_inspector(1, outgoing);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("the page should load");
        (browser, messages)
    }

    fn call(
        browser: &mut Browser,
        messages: &Receiver<String>,
        id: u64,
        method: &str,
        params: Json,
    ) -> Vec<Json> {
        let message = json!({ "id": id, "method": method, "params": params });
        assert_eq!(browser.inspect(1, &message.to_string()), None);
        received(messages)
    }

    fn received(messages: &Receiver<String>) -> Vec<Json> {
        messages
            .try_iter()
            .map(|text| serde_json::from_str(&text).expect("messages should be JSON"))
            .collect()
    }

    fn result(messages: &[Json], id: u64) -> &Json {
        let answer = messages
            .iter()
            .find(|message| message["id"] == id)
            .unwrap_or_else(|| panic!("no answer to {id} in {messages:?}"));
        &answer["result"]
    }

    #[test]
    fn performance_metrics_report_the_phases_of_the_frames_since_enabling() {
        let (mut browser, messages) = inspected("<p>measured</p>");
        call(&mut browser, &messages, 1, "Performance.enable", json!({}));
        let viewport = crate::layout::Size {
            width: 200.0,
            height: 100.0,
        };
        browser.paint(viewport).expect("the page should paint");
        let sent = call(
            &mut browser,
            &messages,
            2,
            "Performance.getMetrics",
            json!({}),
        );
        let metrics = result(&sent, 2)["metrics"].as_array().unwrap().clone();
        let value = |name: &str| {
            metrics
                .iter()
                .find(|metric| metric["name"] == name)
                .unwrap_or_else(|| panic!("no {name} among {metrics:?}"))["value"]
                .as_f64()
                .unwrap()
        };
        assert_eq!(value("frame: paint calls"), 1.0);
        assert_eq!(value("LayoutCount"), 1.0);
        assert!(value("LayoutDuration") > 0.0);
        assert!(value("paint: whole frame total") >= value("paint: whole frame self"));
        assert_eq!(value("paint: whole frame calls"), 1.0);
        call(&mut browser, &messages, 3, "Performance.disable", json!({}));
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Performance.getMetrics",
            json!({}),
        );
        assert_eq!(result(&sent, 4)["metrics"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn enabling_runtime_announces_the_context_and_replays_the_console() {
        let (mut browser, messages) =
            inspected("<script>console.log('hi', {a: 1}); console.warn('careful')</script>");
        let sent = call(&mut browser, &messages, 1, "Runtime.enable", json!({}));
        let methods: Vec<_> = sent
            .iter()
            .map(|message| message["method"].clone())
            .collect();
        assert_eq!(
            methods[..3],
            [
                "Runtime.executionContextCreated",
                "Runtime.consoleAPICalled",
                "Runtime.consoleAPICalled"
            ]
        );
        assert_eq!(sent[3]["id"], 1);
        let context = &sent[0]["params"]["context"];
        assert_eq!(context["origin"], "https://app.ceri.invalid");
        let log = &sent[1]["params"];
        assert_eq!(log["type"], "log");
        assert_eq!(log["executionContextId"], context["id"]);
        assert_eq!(log["args"][0], json!({ "type": "string", "value": "hi" }));
        assert_eq!(log["args"][1]["type"], "object");
        assert_eq!(log["args"][1]["className"], "Object");
        assert_eq!(sent[2]["params"]["type"], "warning");

        let sent = call(
            &mut browser,
            &messages,
            2,
            "Runtime.evaluate",
            json!({ "expression": "console.error('later')" }),
        );
        assert_eq!(sent[0]["method"], "Runtime.consoleAPICalled");
        assert_eq!(sent[0]["params"]["type"], "error");
        assert_eq!(result(&sent, 2)["result"]["type"], "undefined");
    }

    #[test]
    fn evaluation_runs_expressions_statements_and_awaits() {
        let (mut browser, messages) = inspected("<script>window.answer = 41</script>");
        let mut evaluate =
            |id: u64, params: Json| call(&mut browser, &messages, id, "Runtime.evaluate", params);
        let sent = evaluate(1, json!({ "expression": "answer + 1" }));
        assert_eq!(
            result(&sent, 1)["result"],
            json!({ "type": "number", "value": 42.0, "description": "42" })
        );
        let sent = evaluate(2, json!({ "expression": "{ a: 1 } // a block" }));
        assert_eq!(result(&sent, 2)["result"]["type"], "object");
        let sent = evaluate(3, json!({ "expression": "let x = 5; x * 2" }));
        assert_eq!(result(&sent, 3)["result"]["value"], 10.0);
        let sent = evaluate(
            4,
            json!({ "expression": "await Promise.resolve(7)", "awaitPromise": true }),
        );
        assert_eq!(result(&sent, 4)["result"]["value"], 7.0);
        let sent = evaluate(
            5,
            json!({
                "expression": "new Promise(done => setTimeout(() => done({ late: [1, 2] }), 10))",
                "awaitPromise": true,
                "returnByValue": true,
            }),
        );
        assert!(sent.is_empty(), "{sent:?}");
        browser.advance_time(20).expect("the timer should run");
        browser.flush_inspector();
        let sent = received(&messages);
        assert_eq!(
            result(&sent, 5)["result"]["value"],
            json!({ "late": [1, 2] })
        );
        let mut evaluate =
            |id: u64, params: Json| call(&mut browser, &messages, id, "Runtime.evaluate", params);
        let sent = evaluate(6, json!({ "expression": "throw new TypeError('boom')" }));
        let details = &result(&sent, 6)["exceptionDetails"];
        assert_eq!(details["exception"]["subtype"], "error");
        let description = details["exception"]["description"].as_str().unwrap();
        assert!(description.starts_with("TypeError: boom"), "{details}");
        let sent = evaluate(
            7,
            json!({ "expression": "answer = 0", "throwOnSideEffect": true }),
        );
        assert!(result(&sent, 7)["exceptionDetails"].is_object());
        let sent = evaluate(8, json!({ "expression": "answer" }));
        assert_eq!(result(&sent, 8)["result"]["value"], 41.0);
        let sent = evaluate(9, json!({ "expression": "[-0, NaN, 2n]" }));
        assert_eq!(result(&sent, 9)["result"]["subtype"], "array");
        assert_eq!(result(&sent, 9)["result"]["description"], "Array(3)");
        let sent = evaluate(10, json!({ "expression": "-0" }));
        assert_eq!(result(&sent, 10)["result"]["unserializableValue"], "-0");
    }

    #[test]
    fn objects_are_inspected_and_called_on_by_id() {
        let (mut browser, messages) = inspected("<p id=note class='a b'>x</p>");
        let sent = call(
            &mut browser,
            &messages,
            1,
            "Runtime.evaluate",
            json!({ "expression": "({ a: 1, get b() { return 2 } })" }),
        );
        let object = result(&sent, 1)["result"]["objectId"].clone();
        let sent = call(
            &mut browser,
            &messages,
            2,
            "Runtime.getProperties",
            json!({ "objectId": object, "ownProperties": true }),
        );
        let answer = result(&sent, 2);
        let names: Vec<_> = answer["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|property| property["name"].clone())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(answer["result"][0]["value"]["value"], 1.0);
        assert_eq!(answer["result"][1]["get"]["type"], "function");
        assert_eq!(answer["internalProperties"][0]["name"], "[[Prototype]]");
        let sent = call(
            &mut browser,
            &messages,
            3,
            "Runtime.callFunctionOn",
            json!({
                "objectId": object,
                "functionDeclaration": "function (step) { return this.a + step }",
                "arguments": [{ "value": 10 }],
                "returnByValue": true,
            }),
        );
        assert_eq!(result(&sent, 3)["result"]["value"], 11.0);
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Runtime.evaluate",
            json!({ "expression": "document.getElementById('note')" }),
        );
        assert_eq!(result(&sent, 4)["result"]["subtype"], "node");
        assert_eq!(result(&sent, 4)["result"]["description"], "p#note.a.b");
        call(
            &mut browser,
            &messages,
            5,
            "Runtime.releaseObject",
            json!({ "objectId": object }),
        );
        let sent = call(
            &mut browser,
            &messages,
            6,
            "Runtime.getProperties",
            json!({ "objectId": object }),
        );
        assert_eq!(sent[0]["error"]["code"], -32000);
        let sent = call(
            &mut browser,
            &messages,
            7,
            "Runtime.queryObjects",
            json!({}),
        );
        assert_eq!(
            sent[0]["error"],
            json!({ "code": -32601, "message": "'Runtime.queryObjects' wasn't found" })
        );
    }

    const THROWING: &str = "<script>
function made() {
  return new Error(\"x\");
}
var error = made();
function inner() {
  throw new TypeError(\"boom\");
}
function outer() {
  inner();
}
setTimeout(function later() {
  throw 1;
}, 5);
</script>";

    #[test]
    fn an_error_s_stack_names_the_frames_it_was_made_in() {
        let (mut browser, messages) = inspected(THROWING);
        let stack = json!({ "expression": "error.stack" });
        let sent = call(&mut browser, &messages, 1, "Runtime.evaluate", stack);
        // a frame is at the start of its statement, counted from one as v8 prints it
        assert_eq!(
            result(&sent, 1)["result"]["value"],
            "Error: x\n    at made (https://app.ceri.invalid/:3:3)\n    at https://app.ceri.invalid/:5:1"
        );
    }

    #[test]
    fn an_uncaught_exception_reports_where_it_was_thrown() {
        let (mut browser, messages) = inspected(THROWING);
        let page = "https://app.ceri.invalid/";
        let sent = call(&mut browser, &messages, 1, "Runtime.enable", json!({}));
        assert!(named(&sent, "Runtime.exceptionThrown").is_empty());
        let outer = json!({ "expression": "outer()" });
        let sent = call(&mut browser, &messages, 2, "Runtime.evaluate", outer);
        let details = &result(&sent, 2)["exceptionDetails"];
        assert_eq!(details["lineNumber"], 6, "{details}");
        assert_eq!(details["columnNumber"], 2);
        assert_eq!(details["url"], page);
        let frames = details["stackTrace"]["callFrames"].as_array().unwrap();
        assert_eq!(frames.len(), 3, "{details}");
        assert_eq!(frames[0]["functionName"], "inner");
        assert_eq!(frames[0]["scriptId"], details["scriptId"]);
        assert_eq!(frames[1]["functionName"], "outer");
        assert_eq!(frames[1]["url"], page);
        assert_eq!(
            (&frames[1]["lineNumber"], &frames[1]["columnNumber"]),
            (&json!(9), &json!(2))
        );
        let description = details["exception"]["description"].as_str().unwrap();
        let expected =
            format!("TypeError: boom\n    at inner ({page}:7:3)\n    at outer ({page}:10:3)\n");
        assert!(description.starts_with(&expected), "{description}");

        // a value that is no error still has the place it was thrown
        browser.advance_time(10).expect("the timer should run");
        browser.flush_inspector();
        let sent = received(&messages);
        let thrown = named(&sent, "Runtime.exceptionThrown");
        let details = &thrown[0]["exceptionDetails"];
        assert_eq!(details["exception"]["value"], 1.0, "{details}");
        assert_eq!(details["lineNumber"], 12);
        assert_eq!(details["columnNumber"], 2);
        let frames = details["stackTrace"]["callFrames"].as_array().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["functionName"], "later");
    }

    #[test]
    fn a_session_attached_after_the_scripts_ran_still_places_an_exception() {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                THROWING.as_bytes(),
            )
            .expect("page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("the page should load");
        let (outgoing, messages) = mpsc::channel();
        browser.attach_inspector(1, outgoing);
        let outer = json!({ "expression": "outer()" });
        let sent = call(&mut browser, &messages, 1, "Runtime.evaluate", outer);
        let details = &result(&sent, 1)["exceptionDetails"];
        assert_eq!(details["url"], "https://app.ceri.invalid/", "{details}");
        assert_eq!(details["lineNumber"], 6);
        assert_eq!(details["columnNumber"], 2);
    }

    #[test]
    fn a_reload_replaces_the_context_and_reports_uncaught_errors() {
        let (mut browser, messages) =
            inspected("<script>setTimeout(() => { throw new Error('later') }, 5)</script>");
        call(&mut browser, &messages, 1, "Page.enable", json!({}));
        let sent = call(&mut browser, &messages, 2, "Runtime.enable", json!({}));
        let first = sent[0]["params"]["context"]["id"].clone();
        browser.advance_time(10).expect("the timer should run");
        browser.flush_inspector();
        let sent = received(&messages);
        assert_eq!(sent[0]["method"], "Runtime.exceptionThrown");
        let description = sent[0]["params"]["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap();
        assert!(description.starts_with("Error: later"), "{description}");
        let reload = json!({ "id": 3, "method": "Page.reload", "params": {} });
        let url = browser
            .inspect(1, &reload.to_string())
            .expect("a reload should navigate");
        assert_eq!(url, "https://app.ceri.invalid/");
        browser.navigate(&url).expect("the page should load again");
        browser.flush_inspector();
        let sent = received(&messages);
        let methods: Vec<_> = sent
            .iter()
            .map(|message| message["method"].clone())
            .collect();
        assert_eq!(
            methods[..5],
            [
                Json::Null,
                json!("Page.frameNavigated"),
                json!("Runtime.executionContextDestroyed"),
                json!("Runtime.executionContextsCleared"),
                json!("Runtime.executionContextCreated")
            ]
        );
        assert_eq!(sent[2]["params"]["executionContextId"], first);
        assert_ne!(sent[4]["params"]["context"]["id"], first);
    }

    fn named<'a>(messages: &'a [Json], method: &str) -> Vec<&'a Json> {
        messages
            .iter()
            .filter(|message| message["method"] == method)
            .map(|message| &message["params"])
            .collect()
    }

    fn evaluate(
        browser: &mut Browser,
        messages: &Receiver<String>,
        id: u64,
        expression: &str,
    ) -> Vec<Json> {
        call(
            browser,
            messages,
            id,
            "Runtime.evaluate",
            json!({ "expression": expression }),
        )
    }

    #[test]
    fn the_document_mirror_follows_the_page_changes() {
        let (mut browser, messages) =
            inspected("<div id=list><p class=a>one</p>\n<p>two</p></div><span>x</span>");
        let sent = call(&mut browser, &messages, 1, "DOM.getDocument", json!({}));
        let root = &result(&sent, 1)["root"];
        assert_eq!(root["nodeType"], 9);
        assert_eq!(root["documentURL"], "https://app.ceri.invalid/");
        let html = &root["children"][0];
        assert_eq!(html["nodeName"], "HTML");
        let body = &html["children"][1];
        assert_eq!(
            (body["nodeName"].as_str(), body["childNodeCount"].as_u64()),
            (Some("BODY"), Some(2))
        );
        assert!(body["children"].is_null());
        let body = body["nodeId"].clone();

        let sent = call(
            &mut browser,
            &messages,
            2,
            "DOM.requestChildNodes",
            json!({ "nodeId": body, "depth": -1 }),
        );
        let set = named(&sent, "DOM.setChildNodes");
        assert_eq!(set[0]["parentId"], body);
        let div = &set[0]["nodes"][0];
        assert_eq!(div["attributes"], json!(["id", "list"]));
        // the line break between the paragraphs is left out
        assert_eq!(div["childNodeCount"], 2);
        let first = &div["children"][0];
        assert_eq!(first["children"][0]["nodeValue"], "one");
        let (first, second) = (
            first["nodeId"].clone(),
            div["children"][1]["nodeId"].clone(),
        );
        let text = div["children"][1]["children"][0]["nodeId"].clone();
        let span = set[0]["nodes"][1]["nodeId"].clone();
        assert_eq!(sent.last().unwrap()["id"], 2);

        let sent = evaluate(
            &mut browser,
            &messages,
            3,
            "const [one, two] = document.querySelectorAll('p'); one.title = 't'; one.removeAttribute('class');
             two.firstChild.data = 'deux'; document.querySelector('span').remove();
             document.getElementById('list').append(document.createElement('em'))",
        );
        assert_eq!(
            named(&sent, "DOM.attributeModified")[0],
            &json!({ "nodeId": first, "name": "title", "value": "t" })
        );
        assert_eq!(
            named(&sent, "DOM.attributeRemoved")[0],
            &json!({ "nodeId": first, "name": "class" })
        );
        assert_eq!(
            named(&sent, "DOM.characterDataModified")[0],
            &json!({ "nodeId": text, "characterData": "deux" })
        );
        assert_eq!(
            named(&sent, "DOM.childNodeRemoved")[0],
            &json!({ "parentNodeId": body, "nodeId": span })
        );
        let inserted = named(&sent, "DOM.childNodeInserted");
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0]["previousNodeId"], second);
        assert_eq!(inserted[0]["node"]["nodeName"], "EM");

        let sent = call(
            &mut browser,
            &messages,
            4,
            "DOM.requestChildNodes",
            json!({ "nodeId": 0 }),
        );
        assert_eq!(
            sent[0]["error"]["message"],
            "Could not find node with given id"
        );
        let sent = call(&mut browser, &messages, 5, "DOM.undo", json!({}));
        assert_eq!(sent[0]["error"]["code"], -32601);

        browser
            .navigate("https://app.ceri.invalid/")
            .expect("the page should load again");
        browser.flush_inspector();
        assert_eq!(named(&received(&messages), "DOM.documentUpdated").len(), 1);
    }

    #[test]
    fn nodes_are_pushed_inspected_edited_searched_and_measured() {
        let (mut browser, messages) = inspected(
            "<style>body { margin: 0 } #box { width: 100px; height: 50px; padding: 5px; margin: 10px; border: 2px solid }</style>
             <section><div id=box><em>deep</em></div></section>",
        );
        call(&mut browser, &messages, 1, "DOM.getDocument", json!({}));
        let sent = evaluate(&mut browser, &messages, 2, "document.querySelector('em')");
        let object = result(&sent, 2)["result"]["objectId"].clone();
        let sent = call(
            &mut browser,
            &messages,
            3,
            "DOM.requestNode",
            json!({ "objectId": object }),
        );
        let em = result(&sent, 3)["nodeId"].clone();
        assert_ne!(em, 0);
        let pushed: Vec<_> = named(&sent, "DOM.setChildNodes")
            .iter()
            .flat_map(|params| {
                params["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|node| node["nodeId"].clone())
            })
            .collect();
        assert!(pushed.contains(&em), "{sent:?}");

        call(
            &mut browser,
            &messages,
            4,
            "DOM.setInspectedNode",
            json!({ "nodeId": em }),
        );
        let sent = call(
            &mut browser,
            &messages,
            5,
            "Runtime.evaluate",
            json!({ "expression": "$0.tagName + typeof $", "includeCommandLineAPI": true }),
        );
        assert_eq!(result(&sent, 5)["result"]["value"], "EMfunction");
        let sent = evaluate(&mut browser, &messages, 6, "typeof $0");
        assert_eq!(result(&sent, 6)["result"]["value"], "undefined");

        let sent = call(
            &mut browser,
            &messages,
            7,
            "DOM.setAttributesAsText",
            json!({ "nodeId": em, "text": "data-x=\"1\" title=hi" }),
        );
        assert_eq!(named(&sent, "DOM.attributeModified").len(), 2);
        let sent = call(
            &mut browser,
            &messages,
            8,
            "DOM.getOuterHTML",
            json!({ "nodeId": em }),
        );
        assert_eq!(
            result(&sent, 8)["outerHTML"],
            "<em data-x=\"1\" title=\"hi\">deep</em>"
        );
        let sent = call(
            &mut browser,
            &messages,
            9,
            "DOM.setAttributesAsText",
            json!({ "nodeId": em, "text": "", "name": "title" }),
        );
        assert_eq!(
            named(&sent, "DOM.attributeRemoved")[0],
            &json!({ "nodeId": em, "name": "title" })
        );

        let sent = call(
            &mut browser,
            &messages,
            10,
            "DOM.performSearch",
            json!({ "query": "DATA-X" }),
        );
        let search = result(&sent, 10).clone();
        assert_eq!(search["resultCount"], 1);
        let sent = call(
            &mut browser,
            &messages,
            11,
            "DOM.getSearchResults",
            json!({ "searchId": search["searchId"], "fromIndex": 0, "toIndex": 1 }),
        );
        assert_eq!(result(&sent, 11)["nodeIds"], json!([em]));
        let sent = call(
            &mut browser,
            &messages,
            12,
            "DOM.performSearch",
            json!({ "query": "section > div" }),
        );
        assert_eq!(result(&sent, 12)["resultCount"], 1);

        let sent = call(
            &mut browser,
            &messages,
            13,
            "DOM.querySelector",
            json!({ "nodeId": 1, "selector": "#box" }),
        );
        let boxed = result(&sent, 13)["nodeId"].clone();
        let sent = call(
            &mut browser,
            &messages,
            14,
            "DOM.getBoxModel",
            json!({ "nodeId": boxed }),
        );
        let model = &result(&sent, 14)["model"];
        assert_eq!(
            (model["width"].as_i64(), model["height"].as_i64()),
            (Some(114), Some(64))
        );
        assert_eq!(
            model["border"],
            json!([10.0, 10.0, 124.0, 10.0, 124.0, 74.0, 10.0, 74.0])
        );
        assert_eq!(
            model["content"],
            json!([17.0, 17.0, 117.0, 17.0, 117.0, 67.0, 17.0, 67.0])
        );
        assert_eq!(
            model["margin"],
            json!([0.0, 0.0, 134.0, 0.0, 134.0, 84.0, 0.0, 84.0])
        );
        let sent = call(
            &mut browser,
            &messages,
            15,
            "DOM.getNodeForLocation",
            json!({ "x": 100, "y": 60 }),
        );
        assert_eq!(result(&sent, 15)["nodeId"], boxed);
    }

    fn pixel(browser: &Browser, x: usize, y: usize) -> [u8; 3] {
        let viewport = Size {
            width: 300.0,
            height: 200.0,
        };
        let canvas = browser.paint(viewport).expect("the page should paint");
        let start = (y * canvas.width() as usize + x) * 4;
        canvas.rgba_pixels()[start..start + 3].try_into().unwrap()
    }

    #[test]
    fn highlights_are_drawn_over_the_frame_and_the_pointer_picks_nodes() {
        let (mut browser, messages) = inspected(
            "<style>body { margin: 0; background: #fff } #box { width: 100px; height: 50px; padding: 5px; margin: 10px; border: 2px solid #000 }</style>
             <div id=box></div>",
        );
        call(&mut browser, &messages, 1, "DOM.getDocument", json!({}));
        let sent = call(
            &mut browser,
            &messages,
            2,
            "DOM.querySelector",
            json!({ "nodeId": 1, "selector": "#box" }),
        );
        let boxed = result(&sent, 2)["nodeId"].clone();
        let config = json!({
            "contentColor": { "r": 255, "g": 0, "b": 0, "a": 1 },
            "marginColor": { "r": 0, "g": 0, "b": 255 },
        });
        assert_eq!(pixel(&browser, 60, 40), [255, 255, 255]);
        call(
            &mut browser,
            &messages,
            3,
            "Overlay.highlightNode",
            json!({ "nodeId": boxed, "highlightConfig": config }),
        );
        assert_eq!(pixel(&browser, 60, 40), [255, 0, 0]);
        assert_eq!(pixel(&browser, 5, 5), [0, 0, 255]);
        // the padding has no colour of its own here, so it shows the page
        assert_eq!(pixel(&browser, 14, 40), [255, 255, 255]);
        assert_eq!(pixel(&browser, 200, 150), [255, 255, 255]);
        call(
            &mut browser,
            &messages,
            4,
            "Overlay.hideHighlight",
            json!({}),
        );
        assert_eq!(pixel(&browser, 60, 40), [255, 255, 255]);

        let viewport = Size {
            width: 300.0,
            height: 200.0,
        };
        assert!(!browser.pick_hover(viewport, 60.0, 40.0).unwrap());
        let sent = call(
            &mut browser,
            &messages,
            5,
            "Overlay.setInspectMode",
            json!({ "mode": "searchForNode", "highlightConfig": config }),
        );
        assert_eq!(result(&sent, 5), &json!({}));
        assert!(browser.pick_hover(viewport, 60.0, 40.0).unwrap());
        let sent = received(&messages);
        assert_eq!(
            named(&sent, "Overlay.nodeHighlightRequested")[0]["nodeId"],
            boxed
        );
        assert_eq!(pixel(&browser, 60, 40), [255, 0, 0]);
        assert!(browser.pick_press(viewport, 60.0, 40.0).unwrap());
        let sent = received(&messages);
        assert_eq!(
            named(&sent, "Overlay.inspectNodeRequested")[0]["backendNodeId"],
            boxed
        );
        assert!(browser.pick_release(false));
        assert!(browser.pick_release(true));
        assert!(!browser.pick_hover(viewport, 60.0, 40.0).unwrap());
        assert_eq!(pixel(&browser, 60, 40), [255, 255, 255]);

        let sent = call(
            &mut browser,
            &messages,
            6,
            "Overlay.setInspectMode",
            json!({ "mode": "showDistances" }),
        );
        assert_eq!(sent[0]["error"]["code"], -32000);
    }

    #[test]
    fn styles_are_matched_with_their_places_edited_and_forced() {
        let (mut browser, messages) = inspected(
            "<style>/* top */ p { color: red }\n#note:hover, .x { margin: 1px; /* padding: 2px; */ }\np::before { content: 'a' }</style>\
             <div style='font-size: 20px'><p id=note style='width: 5px'>x</p></div>",
        );
        call(&mut browser, &messages, 1, "DOM.getDocument", json!({}));
        let sent = call(&mut browser, &messages, 2, "CSS.enable", json!({}));
        let added = named(&sent, "CSS.styleSheetAdded");
        assert_eq!(added.len(), 1);
        let header = &added[0]["header"];
        assert_eq!(
            (
                header["isInline"].as_bool(),
                header["sourceURL"].as_str(),
                header["endLine"].as_u64()
            ),
            (Some(true), Some("https://app.ceri.invalid/"), Some(2))
        );
        let sheet = header["styleSheetId"].clone();
        let sent = call(
            &mut browser,
            &messages,
            3,
            "DOM.querySelector",
            json!({ "nodeId": 1, "selector": "#note" }),
        );
        let note = result(&sent, 3)["nodeId"].clone();
        let matched = |browser: &mut Browser, id: u64| {
            let params = json!({ "nodeId": note });
            let sent = call(
                browser,
                &messages,
                id,
                "CSS.getMatchedStylesForNode",
                params,
            );
            result(&sent, id).clone()
        };
        let range = |line: u64, start: u64, end: u64| json!({ "startLine": line, "startColumn": start, "endLine": line, "endColumn": end });

        let styles = matched(&mut browser, 4);
        let rules = styles["matchedCSSRules"].as_array().unwrap();
        assert_eq!(rules[0]["rule"]["origin"], "user-agent");
        assert!(rules[0]["rule"]["styleSheetId"].is_null());
        let rule = &rules.last().unwrap()["rule"];
        assert_eq!(rule["styleSheetId"], sheet);
        assert_eq!(rule["selectorList"]["text"], "p");
        assert_eq!(
            rule["selectorList"]["selectors"][0]["range"],
            range(0, 10, 11)
        );
        assert_eq!(rule["style"]["cssText"], " color: red ");
        assert_eq!(
            styles["inlineStyle"]["styleSheetId"],
            format!("style-{note}")
        );
        assert_eq!(styles["inlineStyle"]["cssText"], "width: 5px");
        let inherited = styles["inherited"].as_array().unwrap();
        assert_eq!(inherited.len(), 3);
        assert_eq!(
            inherited[0]["inlineStyle"]["cssProperties"][0]["name"],
            "font-size"
        );
        let pseudo = styles["pseudoElements"].as_array().unwrap();
        assert_eq!(pseudo.len(), 1);
        assert_eq!(pseudo[0]["pseudoType"], "before");
        assert_eq!(
            pseudo[0]["matches"][0]["rule"]["selectorList"]["text"],
            "p::before"
        );

        call(
            &mut browser,
            &messages,
            5,
            "CSS.forcePseudoState",
            json!({ "nodeId": note, "forcedPseudoClasses": ["hover"] }),
        );
        let styles = matched(&mut browser, 6);
        let entry = styles["matchedCSSRules"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(entry["matchingSelectors"], json!([0]));
        let rule = &entry["rule"];
        assert_eq!(
            rule["selectorList"]["selectors"][1]["range"],
            range(1, 13, 15)
        );
        let style = rule["style"].clone();
        assert_eq!(style["range"], range(1, 17, 51));
        let properties = style["cssProperties"].as_array().unwrap();
        assert_eq!(
            (
                properties[0]["text"].as_str(),
                properties[0]["parsedOk"].as_bool()
            ),
            (Some("margin: 1px;"), Some(true))
        );
        assert_eq!(properties[1]["disabled"], true);
        assert_eq!(properties[1]["range"], range(1, 31, 50));

        let computed = |browser: &mut Browser, id: u64, name: &str| {
            let params = json!({ "nodeId": note });
            let sent = call(
                browser,
                &messages,
                id,
                "CSS.getComputedStyleForNode",
                params,
            );
            let style = result(&sent, id)["computedStyle"].as_array().unwrap();
            let property = style.iter().find(|property| property["name"] == name);
            property.map(|property| property["value"].clone())
        };
        assert_eq!(computed(&mut browser, 7, "margin-top"), Some(json!("1px")));
        let edit = |text: &str| json!({ "edits": [{ "styleSheetId": sheet, "range": style["range"], "text": text }] });
        let sent = call(
            &mut browser,
            &messages,
            8,
            "CSS.setStyleTexts",
            edit(" margin: 3px; padding: 2px; "),
        );
        assert_eq!(
            result(&sent, 8)["styles"][0]["cssProperties"][1]["name"],
            "padding"
        );
        assert_eq!(computed(&mut browser, 9, "margin-top"), Some(json!("3px")));
        assert_eq!(
            computed(&mut browser, 10, "padding-top"),
            Some(json!("2px"))
        );
        let sent = call(
            &mut browser,
            &messages,
            11,
            "CSS.setStyleTexts",
            edit("} *"),
        );
        assert_eq!(sent[0]["error"]["code"], -32000);
        call(
            &mut browser,
            &messages,
            12,
            "CSS.forcePseudoState",
            json!({ "nodeId": note, "forcedPseudoClasses": [] }),
        );
        assert_eq!(
            computed(&mut browser, 13, "margin-top"),
            Some(json!("20px"))
        );

        let sent = call(
            &mut browser,
            &messages,
            14,
            "CSS.setStyleTexts",
            json!({ "edits": [{ "styleSheetId": format!("style-{note}"), "range": range(0, 0, 10), "text": "width: 7px" }] }),
        );
        assert_eq!(
            named(&sent, "DOM.attributeModified")[0]["value"],
            "width: 7px"
        );
        assert_eq!(computed(&mut browser, 15, "width"), Some(json!("7px")));

        let sent = call(
            &mut browser,
            &messages,
            16,
            "CSS.setRuleSelector",
            json!({ "styleSheetId": sheet, "range": range(0, 10, 11), "selector": "p, em" }),
        );
        assert_eq!(result(&sent, 16)["selectorList"]["text"], "p, em");
        let sent = call(
            &mut browser,
            &messages,
            17,
            "CSS.getStyleSheetText",
            json!({ "styleSheetId": sheet }),
        );
        let text = result(&sent, 17)["text"].as_str().unwrap();
        assert!(
            text.starts_with(
                "/* top */ p, em { color: red }\n#note:hover, .x { margin: 3px; padding: 2px; }"
            ),
            "{text}"
        );

        let sent = evaluate(
            &mut browser,
            &messages,
            18,
            "document.querySelector('style').append('em { color: blue }')",
        );
        assert_eq!(
            named(&sent, "CSS.styleSheetRemoved")[0]["styleSheetId"],
            sheet
        );
        assert_ne!(
            named(&sent, "CSS.styleSheetAdded")[0]["header"]["styleSheetId"],
            sheet
        );
    }

    #[test]
    fn requests_are_recorded_with_their_answers_while_the_network_is_enabled() {
        let mut resources = ResourceBundle::default();
        let page = "<link rel=stylesheet href=/app.css><img src=/dot.png><script>\
            fetch('/data.json', { method: 'POST', body: 'q=1' });\
            fetch('/data.json');\
            const xhr = new XMLHttpRequest(); xhr.open('GET', '/missing.json', false); xhr.send();\
            </script>";
        for (path, mime, body) in [
            ("/index.html", "text/html; charset=utf-8", page.as_bytes()),
            ("/app.css", "text/css", b"p { color: red }".as_slice()),
            ("/data.json", "application/json", br#"{"a":1}"#.as_slice()),
            (
                "/dot.png",
                "image/png",
                [0x89, b'P', b'N', b'G', 0, 0xff].as_slice(),
            ),
        ] {
            resources.insert(path, mime, body).expect("resource");
        }
        let mut browser = Browser::new(resources);
        let (outgoing, messages) = mpsc::channel();
        browser.attach_inspector(1, outgoing);
        browser.navigate("https://app.ceri.invalid/").expect("load");
        browser.flush_inspector();
        received(&messages);

        let sent = call(&mut browser, &messages, 1, "Network.enable", json!({}));
        assert!(named(&sent, "Network.requestWillBeSent").is_empty());
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("reload");
        browser
            .advance_time(0)
            .expect("the image should be delivered");
        browser.flush_inspector();
        let sent = received(&messages);
        let requests = named(&sent, "Network.requestWillBeSent");
        let seen: Vec<_> = requests
            .iter()
            .map(|params| {
                (
                    params["type"].as_str().unwrap(),
                    params["request"]["method"].as_str().unwrap(),
                    params["request"]["url"].as_str().unwrap(),
                )
            })
            .collect();
        let origin = "https://app.ceri.invalid";
        assert_eq!(seen[0], ("Document", "GET", &*format!("{origin}/")));
        for expected in [
            ("Stylesheet", "GET", format!("{origin}/app.css")),
            ("Image", "GET", format!("{origin}/dot.png")),
            ("Fetch", "POST", format!("{origin}/data.json")),
            ("Fetch", "GET", format!("{origin}/data.json")),
            ("XHR", "GET", format!("{origin}/missing.json")),
        ] {
            assert!(
                seen.contains(&(expected.0, expected.1, &*expected.2)),
                "{expected:?} in {seen:?}"
            );
        }
        let loader = browser.frame()["loaderId"].clone();
        assert!(requests.iter().all(|params| params["loaderId"] == loader));
        let id_of = |kind: &str, method: &str| {
            let index = seen
                .iter()
                .position(|seen| seen.0 == kind && seen.1 == method)
                .unwrap();
            requests[index]["requestId"].clone()
        };
        let responses = named(&sent, "Network.responseReceived");
        let status_of = |id: &Json| {
            let params = responses.iter().find(|params| params["requestId"] == *id);
            params.map(|params| params["response"]["status"].clone())
        };
        assert_eq!(status_of(&id_of("Document", "GET")), Some(json!(200)));
        assert_eq!(status_of(&id_of("XHR", "GET")), Some(json!(404)));
        // the bundle answers only GET, and there is no provider for the POST
        assert_eq!(status_of(&id_of("Fetch", "POST")), Some(json!(404)));
        let finished = named(&sent, "Network.loadingFinished");
        assert_eq!(finished.len(), requests.len());

        let body = |browser: &mut Browser, id: u64, request: Json| {
            let params = json!({ "requestId": request });
            let sent = call(browser, &messages, id, "Network.getResponseBody", params);
            result(&sent, id).clone()
        };
        assert_eq!(
            body(&mut browser, 2, id_of("Stylesheet", "GET")),
            json!({ "body": "p { color: red }", "base64Encoded": false })
        );
        assert_eq!(
            body(&mut browser, 3, id_of("Image", "GET")),
            json!({ "body": "iVBORwD/", "base64Encoded": true })
        );
        let params = json!({ "requestId": id_of("Fetch", "POST") });
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Network.getRequestPostData",
            params,
        );
        assert_eq!(result(&sent, 4)["postData"], "q=1");
        let params = json!({ "requestId": id_of("Fetch", "GET") });
        let sent = call(
            &mut browser,
            &messages,
            5,
            "Network.getRequestPostData",
            params,
        );
        assert_eq!(sent[0]["error"]["code"], -32000);

        call(&mut browser, &messages, 6, "Network.disable", json!({}));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("reload");
        browser.flush_inspector();
        assert!(named(&received(&messages), "Network.requestWillBeSent").is_empty());
        let params = json!({ "requestId": id_of("Stylesheet", "GET") });
        let sent = call(
            &mut browser,
            &messages,
            7,
            "Network.getResponseBody",
            params,
        );
        assert_eq!(sent[0]["error"]["code"], -32000);
    }

    const DEBUGGED: &str = "<!doctype html>\n<script>\nvar a = 1;\nfunction f(x) {\n  let y = x + 1;\n\n  return y;\n}\n</script>\n<script src=/app.js></script>";

    fn debugged() -> (Browser, Receiver<String>) {
        let mut resources = ResourceBundle::default();
        for (path, mime, body) in [
            ("/index.html", "text/html; charset=utf-8", DEBUGGED),
            ("/app.js", "text/javascript", "window.b = f(1);\n"),
        ] {
            resources
                .insert(path, mime, body.as_bytes())
                .expect("resource");
        }
        let mut browser = Browser::new(resources);
        let (outgoing, messages) = mpsc::channel();
        browser.attach_inspector(1, outgoing);
        browser.navigate("https://app.ceri.invalid/").expect("load");
        browser.flush_inspector();
        received(&messages);
        (browser, messages)
    }

    // a window's queue as a test lays it out: what a paused page reads, in order
    #[derive(Default)]
    struct Scripted {
        incoming: RefCell<VecDeque<Incoming>>,
        deferred: RefCell<Vec<String>>,
    }

    impl Scripted {
        fn queue(&self, id: u64, method: &str, params: Json) {
            let text = json!({ "id": id, "method": method, "params": params }).to_string();
            self.incoming
                .borrow_mut()
                .push_back(Incoming::Message { session: 1, text });
        }
    }

    impl PauseSource for Scripted {
        fn next(&self) -> Incoming {
            self.incoming
                .borrow_mut()
                .pop_front()
                .unwrap_or(Incoming::Closed)
        }

        fn poll(&self) -> Option<Incoming> {
            None
        }

        fn defer(&self, _: u64, text: String) {
            self.deferred.borrow_mut().push(text);
        }
    }

    fn located(frame: &Json) -> (u64, u64) {
        let location = &frame["location"];
        (
            location["lineNumber"].as_u64().unwrap(),
            location["columnNumber"].as_u64().unwrap(),
        )
    }

    #[test]
    fn scripts_are_reported_where_the_page_wrote_them_and_breakpoints_land_on_statements() {
        let (mut browser, messages) = debugged();
        call(&mut browser, &messages, 9, "Runtime.enable", json!({}));
        let sent = call(&mut browser, &messages, 1, "Debugger.enable", json!({}));
        let parsed = named(&sent, "Debugger.scriptParsed");
        let page = "https://app.ceri.invalid/";
        assert_eq!(parsed.len(), 2, "{sent:?}");
        assert_eq!(parsed[0]["url"], page);
        assert_eq!([&parsed[0]["startLine"], &parsed[0]["startColumn"]], [1, 8]);
        assert_eq!([&parsed[0]["endLine"], &parsed[0]["endColumn"]], [8, 0]);
        assert_eq!(parsed[1]["url"], "https://app.ceri.invalid/app.js");
        assert_eq!(parsed[1]["startLine"], 0);
        let context = browser.page.as_ref().unwrap().runtime.inspector_context();
        assert!(
            parsed
                .iter()
                .all(|params| params["executionContextId"] == context)
        );
        assert!(result(&sent, 1)["debuggerId"].is_string());
        let script = parsed[0]["scriptId"].clone();

        let sent = call(
            &mut browser,
            &messages,
            2,
            "Debugger.getScriptSource",
            json!({ "scriptId": script }),
        );
        let text = &DEBUGGED[24..DEBUGGED.find("</script>").unwrap()];
        assert_eq!(result(&sent, 2)["scriptSource"], text);
        let page_content = json!({ "frameId": frame_id(), "url": page });
        let sent = call(
            &mut browser,
            &messages,
            6,
            "Page.getResourceContent",
            page_content,
        );
        assert_eq!(
            *result(&sent, 6),
            json!({ "content": DEBUGGED, "base64Encoded": false })
        );

        let range = json!({
            "start": { "scriptId": script, "lineNumber": 3, "columnNumber": 0 },
            "end": { "scriptId": script, "lineNumber": 7, "columnNumber": 0 },
        });
        let sent = call(
            &mut browser,
            &messages,
            3,
            "Debugger.getPossibleBreakpoints",
            range,
        );
        let places: Vec<_> = result(&sent, 3)["locations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|location| {
                (
                    location["lineNumber"].as_u64().unwrap(),
                    location["columnNumber"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(places, [(3, 0), (4, 2), (6, 2)]);

        let by_url = json!({ "url": page, "lineNumber": 5 });
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Debugger.setBreakpointByUrl",
            by_url.clone(),
        );
        let set = result(&sent, 4);
        assert_eq!(set["breakpointId"], format!("1:5:0:{page}"));
        assert_eq!(located(&json!({ "location": set["locations"][0] })), (6, 2));
        let sent = call(
            &mut browser,
            &messages,
            5,
            "Debugger.setBreakpointByUrl",
            by_url,
        );
        assert_eq!(sent[0]["error"]["code"], -32000);

        // a reload parses the scripts again and the breakpoint follows them
        browser.navigate(page).expect("reload");
        browser.flush_inspector();
        let sent = received(&messages);
        let methods: Vec<_> = sent
            .iter()
            .filter_map(|message| message["method"].as_str())
            .collect();
        let first_parse = methods
            .iter()
            .position(|method| *method == "Debugger.scriptParsed")
            .unwrap();
        assert!(methods[..first_parse].contains(&"Runtime.executionContextCreated"));
        let resolved = named(&sent, "Debugger.breakpointResolved");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            located(&json!({ "location": resolved[0]["location"] })),
            (6, 2)
        );
    }

    #[test]
    fn a_breakpoint_pauses_and_the_paused_frames_answer_until_a_resume() {
        let (mut browser, messages) = debugged();
        let source = Rc::new(Scripted::default());
        browser.set_pause_source(source.clone());
        call(&mut browser, &messages, 1, "Runtime.enable", json!({}));
        call(&mut browser, &messages, 2, "Debugger.enable", json!({}));
        let page = "https://app.ceri.invalid/";
        let at = json!({ "url": page, "lineNumber": 4 });
        call(
            &mut browser,
            &messages,
            3,
            "Debugger.setBreakpointByUrl",
            at,
        );

        source.queue(10, "Runtime.evaluate", json!({ "expression": "1" }));
        source.queue(11, "DOM.getDocument", json!({}));
        let frame = json!({ "callFrameId": "0", "expression": "x + a", "returnByValue": true });
        source.queue(12, "Debugger.evaluateOnCallFrame", frame);
        source.queue(13, "Debugger.stepOver", json!({}));
        let frame = json!({ "callFrameId": "0", "expression": "y", "returnByValue": true });
        source.queue(14, "Debugger.evaluateOnCallFrame", frame);
        source.queue(15, "Debugger.resume", json!({}));
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Runtime.evaluate",
            json!({ "expression": "f(41)", "returnByValue": true }),
        );

        let paused = named(&sent, "Debugger.paused");
        assert_eq!(paused.len(), 2, "{sent:?}");
        assert_eq!(named(&sent, "Debugger.resumed").len(), 2);
        let top = &paused[0]["callFrames"][0];
        assert_eq!(top["functionName"], "f");
        assert_eq!(located(top), (4, 2));
        assert_eq!(paused[0]["hitBreakpoints"][0], format!("1:4:0:{page}"));
        let kinds: Vec<_> = top["scopeChain"]
            .as_array()
            .unwrap()
            .iter()
            .map(|scope| scope["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["local", "global"]);
        assert_eq!(paused[0]["callFrames"].as_array().unwrap().len(), 2);
        assert_eq!(located(&paused[1]["callFrames"][0]), (6, 2));
        assert_eq!(paused[1]["hitBreakpoints"], json!([]));
        assert_eq!(result(&sent, 10)["result"]["value"], 1.0);
        assert_eq!(result(&sent, 12)["result"]["value"], 42.0);
        assert_eq!(result(&sent, 14)["result"]["value"], 42.0);
        assert_eq!(result(&sent, 4)["result"]["value"], 42.0);
        let deferred = source.deferred.borrow();
        assert_eq!(deferred.len(), 1);
        assert!(deferred[0].contains("DOM.getDocument"));
    }

    #[test]
    fn debugger_statements_and_uncaught_throws_pause_while_breakpoints_are_active() {
        let (mut browser, messages) = debugged();
        let source = Rc::new(Scripted::default());
        browser.set_pause_source(source.clone());
        call(&mut browser, &messages, 1, "Debugger.enable", json!({}));

        let sent = call(
            &mut browser,
            &messages,
            2,
            "Runtime.evaluate",
            json!({ "expression": "(function g() { debugger; return 1; })()" }),
        );
        let paused = named(&sent, "Debugger.paused");
        assert_eq!(paused.len(), 1, "{sent:?}");
        assert_eq!(paused[0]["reason"], "other");
        assert_eq!(paused[0]["callFrames"][0]["functionName"], "g");
        let scopes = paused[0]["callFrames"][0]["scopeChain"].as_array().unwrap();
        let kinds: Vec<_> = scopes.iter().map(|scope| &scope["type"]).collect();
        assert_eq!(kinds, ["local", "global"]);
        // the console's own source is announced when a pause first shows it
        let script = paused[0]["callFrames"][0]["location"]["scriptId"].clone();
        assert!(
            named(&sent, "Debugger.scriptParsed")
                .iter()
                .any(|params| params["scriptId"] == script)
        );
        let sent = call(
            &mut browser,
            &messages,
            8,
            "Debugger.getScriptSource",
            json!({ "scriptId": script }),
        );
        let written = "(function g() { debugger; return 1; })()";
        assert_eq!(result(&sent, 8)["scriptSource"], written);

        call(
            &mut browser,
            &messages,
            3,
            "Debugger.setBreakpointsActive",
            json!({ "active": false }),
        );
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Runtime.evaluate",
            json!({ "expression": "(function () { debugger; return 1; })()" }),
        );
        assert!(named(&sent, "Debugger.paused").is_empty());

        call(
            &mut browser,
            &messages,
            5,
            "Debugger.setPauseOnExceptions",
            json!({ "state": "uncaught" }),
        );
        let throwing = "(function () { try { throw 1 } catch (e) {} throw new Error('boom') })()";
        let sent = call(
            &mut browser,
            &messages,
            6,
            "Runtime.evaluate",
            json!({ "expression": throwing }),
        );
        let paused = named(&sent, "Debugger.paused");
        assert_eq!(paused.len(), 1, "{sent:?}");
        assert_eq!(paused[0]["reason"], "exception");
        assert_eq!(paused[0]["data"]["uncaught"], true);
        assert!(
            paused[0]["data"]["description"]
                .as_str()
                .unwrap()
                .contains("boom")
        );
        assert!(result(&sent, 6)["exceptionDetails"].is_object());

        let sent = call(&mut browser, &messages, 7, "Debugger.resume", json!({}));
        assert_eq!(
            sent[0]["error"]["message"],
            "Can only perform operation while paused."
        );
    }

    #[test]
    fn automation_worlds_scripts_lifecycle_and_media_follow_the_page() {
        let (mut browser, messages) =
            inspected("<script>window.seen = window.early; document.title = 'worlds'</script>");
        call(&mut browser, &messages, 1, "Page.enable", json!({}));
        let sent = call(&mut browser, &messages, 2, "Runtime.enable", json!({}));
        let main = sent[0]["params"]["context"]["id"].clone();
        call(
            &mut browser,
            &messages,
            3,
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": true }),
        );
        let sent = call(
            &mut browser,
            &messages,
            4,
            "Page.createIsolatedWorld",
            json!({ "frameId": frame_id(), "worldName": "utility" }),
        );
        let created = named(&sent, "Runtime.executionContextCreated");
        let world = &created[0]["context"];
        assert_eq!(world["name"], "utility");
        assert_eq!(world["auxData"]["isDefault"], false);
        assert_eq!(world["auxData"]["type"], "isolated");
        assert_eq!(world["id"], result(&sent, 4)["executionContextId"]);
        assert_ne!(world["id"], main);

        let sent = call(
            &mut browser,
            &messages,
            5,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": "window.early = 'first'" }),
        );
        assert!(result(&sent, 5)["identifier"].is_string());
        let reload = json!({ "id": 6, "method": "Page.reload", "params": {} });
        let url = browser
            .inspect(1, &reload.to_string())
            .expect("a reload should navigate");
        browser.navigate(&url).expect("the page should load again");
        browser.flush_inspector();
        let sent = received(&messages);
        let lifecycle: Vec<_> = named(&sent, "Page.lifecycleEvent")
            .iter()
            .map(|event| event["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            lifecycle,
            [
                "init",
                "commit",
                "DOMContentLoaded",
                "load",
                "networkAlmostIdle",
                "networkIdle"
            ]
        );
        let worlds: Vec<_> = named(&sent, "Runtime.executionContextCreated")
            .iter()
            .map(|event| event["context"]["name"].clone())
            .collect();
        assert_eq!(worlds, [json!(""), json!("utility")]);
        let sent = evaluate(&mut browser, &messages, 7, "window.seen");
        assert_eq!(result(&sent, 7)["result"]["value"], "first");

        let sent = call(
            &mut browser,
            &messages,
            8,
            "Emulation.setEmulatedMedia",
            json!({ "media": "", "features": [
                { "name": "prefers-color-scheme", "value": "dark" },
                { "name": "prefers-reduced-motion", "value": "reduce" },
                { "name": "forced-colors", "value": "none" },
            ] }),
        );
        assert_eq!(result(&sent, 8), &json!({}));
        let sent = evaluate(
            &mut browser,
            &messages,
            9,
            "matchMedia('(prefers-color-scheme: dark)').matches && matchMedia('(prefers-reduced-motion: reduce)').matches",
        );
        assert_eq!(result(&sent, 9)["result"]["value"], true);
        let sent = call(
            &mut browser,
            &messages,
            10,
            "Emulation.setEmulatedMedia",
            json!({ "media": "print" }),
        );
        assert_eq!(sent[0]["error"]["code"], -32000);
    }
}
