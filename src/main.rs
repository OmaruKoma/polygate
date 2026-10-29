#![forbid(unsafe_code)]

/// Thin entry point: argument parsing, logging and command dispatch live in
/// `polygate::cli` so they can be tested without spawning a process.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let code = polygate::cli::run().await;
    std::process::ExitCode::from(code as u8)
}
