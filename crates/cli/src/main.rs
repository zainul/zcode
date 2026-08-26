#![forbid(unsafe_code)]

use std::process::ExitCode;

/// No async runtime: the engine loop, the provider clients (`reqwest::blocking`)
/// and the TUI are all synchronous (DQ4), so a runtime would add startup cost,
/// idle memory and binary size for nothing — and a blocking HTTP client cannot
/// legally be dropped inside one. Concurrency, where it exists, is plain
/// `std::thread` plus channels.
fn main() -> ExitCode {
    match zcode::cli::run() {
        Ok(code) => code,
        Err(e) => {
            // A clean one-line message, never a stack trace (NFR-REL-01).
            eprintln!("zcode: {e}");
            ExitCode::FAILURE
        }
    }
}
