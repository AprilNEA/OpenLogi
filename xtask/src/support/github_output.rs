//! Step outputs for GitHub Actions.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

/// The variable GitHub Actions names a step's output file in.
const OUTPUT_ENV: &str = "GITHUB_OUTPUT";

/// Append `outputs` to the step's output file as `key=value` lines. Outside
/// GitHub Actions, where the variable is unset, this does nothing.
pub(crate) fn write(outputs: &[(&str, &str)]) -> Result<()> {
    let Some(path) = std::env::var_os(OUTPUT_ENV) else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let mut file = fs_err::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening GitHub Actions output {}", path.display()))?;
    for (key, value) in outputs {
        ensure!(
            !key.contains(['=', '\n']) && !value.contains('\n'),
            "GitHub Actions output {key:?} must be one `key=value` line"
        );
        writeln!(file, "{key}={value}")?;
    }
    Ok(())
}
