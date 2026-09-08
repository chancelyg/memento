//! memento binary entry point.
//!
//! Thin shim over the `memento` library: initialise tracing, run the app,
//! and translate the result into a process exit code.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return match memento::cli::execute(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    // Load only the selected environment file from this working directory.
    if let Err(error) = memento::config::load_environment_file() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }

    memento::init_tracing();

    match memento::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal startup error");
            ExitCode::FAILURE
        }
    }
}
