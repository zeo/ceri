pub mod assets;
pub mod launch;

mod commands;
mod custom;
#[cfg(not(target_os = "windows"))]
mod dbus;
mod desktop;
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
pub use files::FileScope;
pub use tray::{MenuItem, Tray, TrayConfig, TrayEvent};
