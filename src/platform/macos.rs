use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_methods,
    msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_app_kit::{
    NSApp, NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSEvent,
};
use std::{cell::Cell, sync::OnceLock};

#[derive(Clone, Copy)]
struct AppHooks {
    show_main_window: fn(),
    close_all_browsers: fn(),
}

static APP_HOOKS: OnceLock<AppHooks> = OnceLock::new();

define_class! {
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct CeriAppDelegate;

    unsafe impl NSObjectProtocol for CeriAppDelegate {}

    unsafe impl NSApplicationDelegate for CeriAppDelegate {
        #[unsafe(method(applicationShouldTerminate:))]
        unsafe fn application_should_terminate(
            &self,
            _sender: &NSApplication,
        ) -> NSApplicationTerminateReply {
            NSApplicationTerminateReply::TerminateNow
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        unsafe fn application_should_handle_reopen(
            &self,
            _sender: &NSApplication,
            _has_visible_windows: Bool,
        ) -> Bool {
            if let Some(hooks) = APP_HOOKS.get() {
                (hooks.show_main_window)();
            }
            Bool::NO
        }

        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        unsafe fn application_supports_secure_restorable_state(
            &self,
            _sender: &NSApplication,
        ) -> Bool {
            Bool::YES
        }
    }
}

impl CeriAppDelegate {
    fn new(main_thread: MainThreadMarker) -> Retained<Self> {
        let delegate = Self::alloc(main_thread).set_ivars(());
        unsafe { msg_send![super(delegate), init] }
    }
}

#[derive(Default)]
struct CeriApplicationIvars {
    handling_send_event: Cell<Bool>,
}

define_class! {
    #[unsafe(super(NSApplication))]
    #[ivars = CeriApplicationIvars]
    struct CeriApplication;

    impl CeriApplication {
        #[unsafe(method(sendEvent:))]
        unsafe fn send_event(&self, event: &NSEvent) {
            let was_sending_event = self.is_handling_send_event();
            if !was_sending_event {
                self.set_handling_send_event(true);
            }

            let _: () = msg_send![super(self), sendEvent: event];

            if !was_sending_event {
                self.set_handling_send_event(false);
            }
        }

        #[unsafe(method(terminate:))]
        unsafe fn terminate(&self, _sender: &AnyObject) {
            if let Some(hooks) = APP_HOOKS.get() {
                (hooks.close_all_browsers)();
            }
        }
    }

    unsafe impl CrAppControlProtocol for CeriApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        unsafe fn _set_handling_send_event(&self, handling_send_event: Bool) {
            self.ivars().handling_send_event.set(handling_send_event);
        }
    }

    unsafe impl CrAppProtocol for CeriApplication {
        #[unsafe(method(isHandlingSendEvent))]
        unsafe fn _is_handling_send_event(&self) -> Bool {
            self.ivars().handling_send_event.get()
        }
    }

    unsafe impl CefAppProtocol for CeriApplication {}
}

impl CeriApplication {
    extern_methods! {
        #[unsafe(method(sharedApplication))]
        fn shared_application() -> Retained<Self>;

        #[unsafe(method(setHandlingSendEvent:))]
        fn set_handling_send_event(&self, handling_send_event: bool);

        #[unsafe(method(isHandlingSendEvent))]
        fn is_handling_send_event(&self) -> bool;
    }
}

#[must_use = "keep the app delegate alive until after CEF shuts down"]
pub struct AppDelegateGuard {
    _delegate: Retained<CeriAppDelegate>,
}

pub fn prepare_application() -> Result<(), &'static str> {
    let _application = CeriApplication::shared_application();
    let main_thread = MainThreadMarker::new().ok_or("Ceri must start on the macOS main thread")?;
    if !NSApp(main_thread).isKindOfClass(CeriApplication::class()) {
        return Err("AppKit initialized NSApplication before Ceri prepared CEF");
    }
    Ok(())
}

pub fn retain_app_delegate(
    show_main_window: fn(),
    close_all_browsers: fn(),
) -> Result<AppDelegateGuard, &'static str> {
    let main_thread = MainThreadMarker::new().ok_or("Ceri must run on the macOS main thread")?;
    let application = NSApp(main_thread);
    if !application.isKindOfClass(CeriApplication::class()) {
        return Err("Ceri's CEF application was not prepared");
    }

    let delegate = CeriAppDelegate::new(main_thread);
    APP_HOOKS
        .set(AppHooks {
            show_main_window,
            close_all_browsers,
        })
        .map_err(|_| "Ceri's macOS app delegate may only be retained once")?;

    let delegate_protocol =
        ProtocolObject::<dyn NSApplicationDelegate>::from_retained(delegate.clone());
    application.setDelegate(Some(&delegate_protocol));

    Ok(AppDelegateGuard {
        _delegate: delegate,
    })
}
