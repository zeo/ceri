use std::process::ExitCode;

fn main() -> ExitCode {
    match ceri::run_helper() {
        Ok(exit_code) => match u8::try_from(exit_code) {
            Ok(exit_code) => ExitCode::from(exit_code),
            Err(_) => {
                eprintln!("ceri helper was launched without a CEF process type");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("ceri helper: {error}");
            ExitCode::FAILURE
        }
    }
}
