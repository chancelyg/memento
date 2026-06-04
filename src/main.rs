//! memento binary entry point.
//!
//! Thin shim over the `memento` library: initialise tracing, run the app,
//! and translate the result into a process exit code.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // Load a local `.env` (if present) into the process environment before any
    // config is read, so a packaged binary picks up `.env` next to it. Existing
    // environment variables take precedence; a missing file is not an error.
    let _ = dotenvy::dotenv();

    memento::init_tracing();

    match memento::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal startup error");
            ExitCode::FAILURE
        }
    }
}
