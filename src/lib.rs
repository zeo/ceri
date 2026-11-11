pub mod assets;
pub mod launch;

mod commands;
mod custom;
#[cfg(not(target_os = "windows"))]
mod dbus;
mod desktop;
mod dialog;
pub mod dirs;
mod files;
mod fonts;
mod http;
mod instance;
mod tray;

pub use ceri_engine::{
    APP_ORIGIN, Capabilities, CommandCall, CommandError, Grant, Monitor, NativeHandle,
    WindowAction, WindowConfig, WindowControl, WindowState,
};
pub use custom::{App, AppHandle, run};
pub use dialog::{FileDialog, FileFilter, MessageButtons, MessageDialog, MessageLevel};
pub use files::FileScope;
pub use tray::{MenuItem, Tray, TrayConfig, TrayEvent};

// the files ceri_build::embed compiled into this crate, for App::assets
#[macro_export]
macro_rules! embedded_assets {
    () => {{
        const FILES: $crate::assets::EmbeddedAssets =
            include!(concat!(env!("OUT_DIR"), "/ceri-assets.rs"));
        FILES
    }};
}
