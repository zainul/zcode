#![forbid(unsafe_code)]

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(e) = ag::cli::run().await {
        eprintln!("ag: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
