//! memento binary entry point.
//!
//! Thin shim over the `memento` library: initialise tracing, run the app,
//! and translate the result into a process exit code.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    memento::init_tracing();

    match memento::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal startup error");
            ExitCode::FAILURE
        }
    }
}
