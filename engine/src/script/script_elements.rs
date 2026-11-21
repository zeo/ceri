// script elements that script inserts: an inline classic script runs at once, while one
// with src, or a module, runs as a task once loaded and then fires load or error; the
// parser's scripts were started already, so moving them never runs them again, and
// document.currentScript names the classic script running

use super::realm::Native;
use super::*;
use crate::dom::ScriptSource;

impl ScriptRuntime {
    pub(crate) fn set_current_script(&mut self, node: Option<NodeId>) {
        self.current_script = node;
    }

    pub(super) fn current_script_value(&self) -> Value {
        self.current_script
            .filter(|&node| self.document.borrow().contains(node))
            .map_or(Value::Null, |node| self.node_value(node))
    }

    // the script elements in what was just inserted, in tree order, each prepared once
    pub(super) fn prepare_inserted_scripts(
        &mut self,
        inserted: &[NodeId],
    ) -> Result<(), ScriptError> {
        let scripts: Vec<NodeId> = {
            let document = self.document.borrow();
            inserted
                .iter()
                .filter(|&&node| document.is_connected(node))
                .flat_map(|&node| document.subtree(node))
                .filter(|&node| {
                    document
                        .element(node)
                        .is_some_and(|element| element.name == "script")
                })
                .collect()
        };
        for script in scripts {
            self.prepare_script(script)?;
        }
        Ok(())
    }

    fn prepare_script(&mut self, node: NodeId) -> Result<(), ScriptError> {
        let source = {
            let document = self.document.borrow();
            if !document.is_connected(node) || document.script_started(node) {
                return Ok(());
            }
            document.script_source(node)
        };
        let Some(source) = source else {
            return Ok(());
        };
        // an empty inline script waits; it is not started, so it can run once it has text
        if let ScriptSource::Inline(text) | ScriptSource::InlineModule(text) = &source
            && text.is_empty()
        {
            return Ok(());
        }
        self.document.borrow_mut().mark_script_started(node);
        match source {
            ScriptSource::Inline(text) => {
                if self.permits_script(node, Some(&text), None) {
                    self.run_classic_inline(node, &text);
                }
                Ok(())
            }
            ScriptSource::ImportMap(text) => {
                if let Some(base) = self.base_url.clone() {
                    self.add_import_map(&text, &base);
                }
                Ok(())
            }
            _ => {
                self.schedule_native_task(Native::ScriptElement(node));
                Ok(())
            }
        }
    }

    fn permits_script(&mut self, node: NodeId, inline: Option<&str>, url: Option<&AppUrl>) -> bool {
        let nonce = self
            .document
            .borrow()
            .element(node)
            .and_then(|element| element.attr("nonce").map(str::to_string));
        let initiator = crate::csp::Initiator {
            nonce: nonce.as_deref(),
            parser_inserted: false,
        };
        let policies = self.content_security_policy();
        let check = match (inline, url) {
            (Some(text), _) => policies.inline(crate::csp::Directive::ScriptElem, text, initiator),
            (None, Some(url)) => policies.url(
                crate::csp::Directive::ScriptElem,
                &url.to_string(),
                initiator,
            ),
            (None, None) => Ok(()),
        };
        self.permits(check)
    }

    // runs inside the call that inserted it, so the microtasks wait for that script to end
    fn run_classic_inline(&mut self, node: NodeId, text: &str) {
        let outer = self.current_script.replace(node);
        let outcome = parser::parse_script(text)
            .and_then(|script| compiler::Compiler::new(text).compile_script(&script))
            .and_then(|code| {
                self.register_script(&code, "", false);
                self.run_script_code(code)
            });
        self.current_script = outer;
        if let Err(error) = outcome {
            self.report_exception(error);
        }
    }

    // the task that loads and runs a script with src, or a module, then reports how it went
    pub(super) fn run_script_element(&mut self, node: NodeId) -> Result<Value, ScriptError> {
        let source = self.document.borrow().script_source(node);
        let Some(source) = source else {
            return Ok(Value::Undefined);
        };
        let external = matches!(
            source,
            ScriptSource::External(_) | ScriptSource::ExternalModule(_)
        );
        let ran = self.load_and_run(node, source);
        if external {
            let event_type = if ran { "load" } else { "error" };
            let event = self.new_event("Event", event_type.to_string(), false, false, false, true);
            self.dispatch(&event, events::ListenerTarget::Node(node))?;
        }
        Ok(Value::Undefined)
    }

    // whether the script's code could be had; an exception it throws is reported, and the
    // script still counts as loaded
    fn load_and_run(&mut self, node: NodeId, source: ScriptSource) -> bool {
        let Some(base) = self.base_url.clone() else {
            return false;
        };
        let resources = self.resources.clone().unwrap_or_default();
        let provider = self.resource_provider.clone();
        let policies = self.content_security_policy();
        let load = |reference: &str, runtime: &mut Self| -> Option<(AppUrl, String)> {
            let url = base.resolve(reference).ok()?;
            if !runtime.permits_script(node, None, Some(&url)) {
                return None;
            }
            let resource =
                resolve_runtime_resource(&resources, provider.as_deref(), &url).ok()??;
            if !is_javascript(&resource) {
                runtime.report(format!(
                    "script {} has unsupported MIME type {}",
                    url.path(),
                    resource.mime_type()
                ));
                return None;
            }
            let text = String::from_utf8(resource.body().to_vec()).ok()?;
            Some((url, text))
        };
        let mut loader = RuntimeModuleLoader {
            resources: &resources,
            provider: provider.as_deref(),
            policies: &policies,
        };
        let outcome = match source {
            ScriptSource::External(reference) => {
                let Some((url, text)) = load(&reference, self) else {
                    return false;
                };
                self.set_script_origin(url.to_string(), 0, 0);
                let outer = self.current_script.replace(node);
                let outcome = self.execute_reporting(&text);
                self.current_script = outer;
                outcome
            }
            ScriptSource::ExternalModule(reference) => {
                let Some((url, text)) = load(&reference, self) else {
                    return false;
                };
                self.execute_module_reporting(&text, url, &mut loader)
            }
            ScriptSource::InlineModule(text) => {
                if !self.permits_script(node, Some(&text), None) {
                    return false;
                }
                self.execute_module_reporting(&text, base.clone(), &mut loader)
            }
            ScriptSource::Inline(_) | ScriptSource::ImportMap(_) => Ok(()),
        };
        self.report_refusals();
        if let Err(error) = outcome {
            self.report(format!("script failed: {error}"));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> (Rc<RefCell<crate::dom::Document>>, ScriptRuntime) {
        let document = Rc::new(RefCell::new(
            crate::dom::Document::parse(r#"<main><script id="page">window.pageRan = (window.pageRan || 0) + 1;</script></main><output id="out"></output>"#)
                .expect("document should parse"),
        ));
        let runtime = ScriptRuntime::new(document.clone());
        (document, runtime)
    }

    fn out(document: &Rc<RefCell<crate::dom::Document>>) -> String {
        let node = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(node)
            .unwrap_or_default()
    }

    #[test]
    fn inserted_inline_scripts_run_once_with_their_current_script() {
        let (document, mut runtime) = runtime();
        runtime
            .execute(
                r#"
                const seen = [];
                window.log = (entry) => seen.push(entry);
                const script = document.createElement("script");
                script.id = "made";
                script.textContent = "log('ran ' + document.currentScript.id); Promise.resolve().then(() => log('micro'));";
                document.body.appendChild(script);
                seen.push("after append");
                document.querySelector("main").appendChild(script);
                const page = document.getElementById("page");
                document.body.appendChild(page);
                const empty = document.createElement("script");
                document.body.appendChild(empty);
                const data = document.createElement("script");
                data.type = "application/json";
                data.textContent = "log('data')";
                document.body.appendChild(data);
                const holder = document.createElement("div");
                holder.innerHTML = "<script>log('from innerHTML')<\/script>";
                document.body.appendChild(holder);
                const inner = document.createElement("div");
                const nested = document.createElement("script");
                nested.textContent = "log('nested')";
                inner.appendChild(nested);
                seen.push("detached");
                document.body.appendChild(inner);
                setTimeout(() => {
                    document.getElementById("out").textContent = [...seen, String(window.pageRan), String(document.currentScript)].join("|");
                }, 0);
                "#,
            )
            .expect("script should run");
        runtime.advance_time(1).expect("timers should run");
        assert_eq!(
            out(&document),
            "ran made|after append|detached|nested|micro|undefined|null"
        );
    }
}
