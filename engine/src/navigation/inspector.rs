// devtools protocol sessions on a browser's page: the Page domain here, the Runtime domain
// routed to the page's script runtime, and the events both owe each session

use super::Browser;
use crate::script::Answer;
use serde_json::{Value as Json, json};
use std::sync::mpsc::Sender;

const FRAME_ID: &str = "ceri-main-frame";

#[derive(Default)]
pub(super) struct Inspection {
    sessions: Vec<Session>,
    waiting: Vec<Waiting>,
}

impl Inspection {
    pub(super) fn attached(&self) -> bool {
        !self.sessions.is_empty()
    }
}

struct Session {
    id: u64,
    outgoing: Sender<String>,
    runtime: bool,
    page: bool,
    // the execution context this session last heard of, and the first console entry it has not
    context: Option<u64>,
    seen: u64,
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
            context: None,
            seen: 0,
        });
    }

    pub fn detach_inspector(&mut self, session: u64) {
        self.inspection.sessions.retain(|held| held.id != session);
        self.inspection
            .waiting
            .retain(|held| held.session != session);
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
            "Runtime.runIfWaitingForDebugger" => Some(Ok(json!({}))),
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
                    Some(Ok(
                        json!({ "frameId": FRAME_ID, "loaderId": self.loader() }),
                    ))
                }
                None => Some(Err((-32602, "Invalid parameters: url".to_string()))),
            },
            method if method.starts_with("Runtime.") => match &mut self.page {
                Some(page) => match page.runtime.inspector_call(method, params) {
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
                },
                None => Some(Err((
                    -32000,
                    "Cannot find default execution context".into(),
                ))),
            },
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

    // sends each session what changed since the last call: a new document's context, console
    // entries, and answers whose promises settled
    pub fn flush_inspector(&mut self) {
        if !self.inspection.attached() {
            return;
        }
        let frame = self.frame();
        let origin = frame["securityOrigin"].clone();
        let Some(page) = &mut self.page else {
            return;
        };
        let context = page.runtime.inspector_context();
        let inspection = &mut self.inspection;
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
                            "origin": origin,
                            "name": "",
                            "uniqueId": context.to_string(),
                            "auxData": { "isDefault": true, "type": "default", "frameId": FRAME_ID },
                        }}),
                    );
                    session.seen = 0;
                }
            }
            if session.runtime {
                let (events, next) = page.runtime.console_events(session.seen);
                session.seen = next;
                for event in events {
                    let _ = session.outgoing.send(event.to_string());
                }
            }
        }
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

    fn loader(&self) -> String {
        self.current_document_id.unwrap_or_default().to_string()
    }

    fn frame(&self) -> Json {
        let (url, origin) = match &self.current_url {
            Some(url) => (url.to_string(), url.origin()),
            None => ("about:blank".to_string(), "null"),
        };
        json!({
            "id": FRAME_ID,
            "loaderId": self.loader(),
            "url": url,
            "domainAndRegistry": "",
            "securityOrigin": origin,
            "mimeType": "text/html",
            "secureContextType": "Secure",
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::ResourceBundle;
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
        let sent = call(&mut browser, &messages, 7, "DOM.enable", json!({}));
        assert_eq!(
            sent[0]["error"],
            json!({ "code": -32601, "message": "'DOM.enable' wasn't found" })
        );
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
}
