use std::process::ExitCode;

use anyhow::Result;

// `Result<ExitCode, anyhow::Error>` is itself a `Termination`: the status
// travels back from the subcommand, and an error still prints the anyhow
// chain before exiting with a failure status.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<ExitCode> {
    openlogi_cli::run().await
}
