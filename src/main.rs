#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(not(all(target_os = "windows", feature = "sandbox")))]
fn main() {
    if let Err(error) = ceri::run(std::ptr::null_mut()) {
        eprintln!("ceri: {error}");
        std::process::exit(1);
    }
}

#[cfg(all(target_os = "windows", feature = "sandbox"))]
fn main() {
    eprintln!("ceri: the sandboxed Windows build must be launched from its CEF bundle");
    std::process::exit(1);
}
