use crate::assets::APP_ORIGIN;
use crate::browser::create_browser_process;
use crate::launch::LaunchConfig;
use cef::*;
use std::time::{SystemTime, UNIX_EPOCH};

const RUNTIME_INFO_REQUEST: &str = r#"{"method":"runtimeInfo","version":1}"#;
const MAX_BRIDGE_REQUEST: usize = 1024;

pub fn create_app(launch: Option<LaunchConfig>) -> App {
    CeriApp::new(launch)
}

wrap_app! {
    struct CeriApp {
        launch: Option<LaunchConfig>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            for switch in [
                "disable-background-networking",
                "disable-component-update",
                "disable-default-apps",
                "disable-extensions",
                "disable-sync",
                "no-first-run",
            ] {
                command_line.append_switch(Some(&CefString::from(switch)));
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            self.launch.clone().map(create_browser_process)
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(CeriRenderProcess::new(session_id()))
        }
    }
}

wrap_render_process_handler! {
    struct CeriRenderProcess {
        session: String,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            if let Some(context) = context.as_deref()
                && let Some(global) = context.global()
            {
                for name in ["cefQuery", "cefQueryCancel"] {
                    let name = CefString::from(name);
                    let mut handler = CeriRuntimeHandler::new(self.session.clone());
                    let mut function =
                        v8_value_create_function(Some(&name), Some(&mut handler));
                    global.set_value_bykey(
                        Some(&name),
                        function.as_mut(),
                        V8Propertyattribute::default(),
                    );
                }
            }
        }
    }
}

wrap_v8_handler! {
    struct CeriRuntimeHandler {
        session: String,
    }

    impl V8Handler {
        fn execute(
            &self,
            name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            retval: Option<&mut Option<V8Value>>,
            exception: Option<&mut CefString>,
        ) -> i32 {
            macro_rules! return_exception {
                ($message:expr) => {
                    if let Some(exception) = exception {
                        *exception = CefString::from($message);
                    }
                    return 1;
                };
            }

            let Some(name) = name else {
                return_exception!("missing function name");
            };
            let name = name.to_string();
            if name == "cefQueryCancel" {
                if let Some(retval) = retval {
                    *retval = v8_value_create_bool(0);
                }
                return 1;
            }
            if name != "cefQuery" {
                return 0;
            }

            let Some(arg) = arguments
                .filter(|arguments| arguments.len() == 1)
                .and_then(|arguments| arguments[0].as_ref())
                .filter(|arg| arg.is_object() != 0)
            else {
                return_exception!("invalid arguments; expecting a single object");
            };

            let key = CefString::from("request");
            let Some(request) = arg.value_bykey(Some(&key)) else {
                return_exception!("invalid arguments; request is required");
            };
            if request.is_string() == 0 {
                return_exception!("invalid arguments; request must be a string");
            }
            let request = CefString::from(&request.string_value()).to_string();

            let key = CefString::from("onSuccess");
            let success = arg.value_bykey(Some(&key));
            if success.as_ref().is_some_and(|callback| callback.is_function() == 0) {
                return_exception!("invalid arguments; onSuccess must be a function");
            }

            let key = CefString::from("onFailure");
            let failure = arg.value_bykey(Some(&key));
            if failure.as_ref().is_some_and(|callback| callback.is_function() == 0) {
                return_exception!("invalid arguments; onFailure must be a function");
            }

            let key = CefString::from("persistent");
            let persistent = arg.value_bykey(Some(&key));
            if persistent
                .as_ref()
                .is_some_and(|value| value.is_undefined() == 0 && value.is_bool() == 0)
            {
                return_exception!("invalid arguments; persistent must be a boolean");
            }

            let Some(mut context) = v8_context_get_current_context() else {
                return_exception!("the Ceri host bridge has no active context");
            };
            let trusted = context.frame().is_some_and(|frame| {
                frame.is_main() != 0 && is_app_url(&CefString::from(&frame.url()).to_string())
            });
            if !trusted {
                invoke_failure(
                    &mut context,
                    failure.as_ref(),
                    403,
                    "bridge unavailable for this document",
                );
            } else if persistent.as_ref().is_some_and(|value| value.bool_value() != 0) {
                invoke_failure(
                    &mut context,
                    failure.as_ref(),
                    400,
                    "persistent runtime queries are unsupported",
                );
            } else if request.len() > MAX_BRIDGE_REQUEST {
                invoke_failure(
                    &mut context,
                    failure.as_ref(),
                    413,
                    "bridge request exceeds 1024 bytes",
                );
            } else if request != RUNTIME_INFO_REQUEST {
                invoke_failure(&mut context, failure.as_ref(), 404, "unknown bridge method");
            } else if let Some(success) = success.as_ref() {
                let response = format!(
                    r#"{{"engine":"CEF 150.0.14","chromium":"150","recovery":"ready","session":"{}"}}"#,
                    self.session
                );
                let response = v8_value_create_string(Some(&CefString::from(response.as_str())));
                success.execute_function_with_context(
                    Some(&mut context),
                    None,
                    Some(&[response]),
                );
            }
            if let Some(retval) = retval {
                *retval = v8_value_create_int(1);
            }
            1
        }
    }
}

fn invoke_failure(
    context: &mut V8Context,
    callback: Option<&V8Value>,
    error_code: i32,
    error_message: &str,
) {
    let Some(callback) = callback else {
        return;
    };
    callback.execute_function_with_context(
        Some(context),
        None,
        Some(&[
            v8_value_create_int(error_code),
            v8_value_create_string(Some(&CefString::from(error_message))),
        ]),
    );
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{nanos:x}", std::process::id())
}

pub(crate) fn is_app_url(url: &str) -> bool {
    url == APP_ORIGIN
        || url
            .strip_prefix(APP_ORIGIN)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

pub(crate) fn is_internal_candidate(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://app.ceri.invalid") || lower.starts_with("http://app.ceri.invalid")
}
