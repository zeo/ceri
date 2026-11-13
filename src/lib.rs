pub mod assets;
pub mod autostart;
pub mod launch;

mod audio;
mod chromium;
mod commands;
mod custom;
#[cfg(not(target_os = "windows"))]
mod dbus;
mod deep_link;
mod desktop;
mod dev;
mod dialog;
pub mod dirs;
mod files;
mod fonts;
mod http;
mod instance;
mod notification;
pub mod opener;
mod shortcut;
mod taskbar;
mod tray;
mod video;

pub use ceri_engine::{
    APP_ORIGIN, Capabilities, CommandCall, CommandError, Grant, Monitor, NativeHandle,
    WindowAction, WindowConfig, WindowControl, WindowState,
};
pub use custom::{App, AppHandle, run};
pub use dialog::{FileDialog, FileFilter, MessageButtons, MessageDialog, MessageLevel};
pub use files::FileScope;
pub use shortcut::{GlobalShortcuts, ShortcutState};
pub use taskbar::Progress;
pub use tray::{MenuItem, Tray, TrayConfig, TrayEvent};

#[doc(hidden)]
pub mod __command {
    pub use crate::commands::{Answer, AnswerResult, AnswerValue, Arguments};
}

// a handler for App::json_command over named arguments, read by their camelCase names from
// the object a page passes, as a Tauri command reads them; the body answers with a value or a
// Result whose error is a CommandError or a message, and |call, ...| also hands it the call
#[macro_export]
macro_rules! command {
    (|| $body:expr) => {
        $crate::command!(|_call| $body)
    };
    (|$($argument:ident : $kind:ty),+ $(,)?| $body:expr) => {
        $crate::command!(|_call, $($argument: $kind),+| $body)
    };
    (|$call:ident $(, $argument:ident : $kind:ty)* $(,)?| $body:expr) => {
        move |$call: &$crate::CommandCall| -> ::std::result::Result<::std::string::String, $crate::CommandError> {
            #[allow(unused_imports)]
            use $crate::__command::{AnswerResult as _, AnswerValue as _};
            #[allow(unused_mut, unused_variables)]
            let mut arguments = $crate::__command::Arguments::of($call)?;
            $(let $argument: $kind = arguments.take(stringify!($argument))?;)*
            $crate::__command::Answer($body).answer()
        }
    };
}

// the files ceri_build::embed compiled into this crate, for App::assets
#[macro_export]
macro_rules! embedded_assets {
    () => {{
        const FILES: $crate::assets::EmbeddedAssets =
            include!(concat!(env!("OUT_DIR"), "/ceri-assets.rs"));
        FILES
    }};
}
