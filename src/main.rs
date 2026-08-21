#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(all(
    feature = "cef-engine",
    not(all(target_os = "windows", feature = "sandbox"))
))]
fn main() {
    if let Err(error) = ceri::run(std::ptr::null_mut()) {
        eprintln!("ceri: {error}");
        std::process::exit(1);
    }
}

#[cfg(all(feature = "cef-engine", target_os = "windows", feature = "sandbox"))]
fn main() {
    eprintln!("ceri: the sandboxed Windows build must be launched from its CEF bundle");
    std::process::exit(1);
}

#[cfg(all(feature = "custom-engine", not(feature = "cef-engine")))]
fn main() {
    if let Err(error) = ceri::run_custom_engine() {
        eprintln!("ceri: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(feature = "cef-engine", feature = "custom-engine")))]
fn main() {
    compile_error!("ceri needs one of the cef-engine or custom-engine features");
}
