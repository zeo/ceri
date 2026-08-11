use crate::runtime;
use cef::*;

#[unsafe(no_mangle)]
unsafe extern "C" fn RunWinMain(
    instance: sys::HINSTANCE,
    _command_line: *mut u16,
    _command_show: i32,
    sandbox_info: *mut u8,
    _version_info: *mut u8,
) -> i32 {
    if let Err(error) = runtime::prepare_cef() {
        eprintln!("ceri: {error}");
        return 1;
    }
    let main_args = MainArgs { instance };
    let args = args::Args::from(main_args);
    match runtime::run_with_args(args.as_main_args(), None, sandbox_info) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ceri: {error}");
            1
        }
    }
}
