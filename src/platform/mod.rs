#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{AppDelegateGuard, prepare_application, retain_app_delegate};

#[cfg(not(target_os = "macos"))]
#[must_use = "keep the app delegate alive until after CEF shuts down"]
pub struct AppDelegateGuard;

#[cfg(not(target_os = "macos"))]
pub fn prepare_application() -> Result<(), &'static str> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn retain_app_delegate(
    _show_main_window: fn(),
    _close_all_browsers: fn(),
) -> Result<AppDelegateGuard, &'static str> {
    Ok(AppDelegateGuard)
}
