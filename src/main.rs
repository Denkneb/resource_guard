use std::process::ExitCode;

fn main() -> ExitCode {
    resource_guard::cli::run_from_environment()
}
