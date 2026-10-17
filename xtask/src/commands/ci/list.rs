//! The `--list` table, rendered from the same rows the runner plans from.
//!
//! It deliberately does not repeat the commands: those are generated from the
//! steps by `--dry-run`, and a hand-copied third version of what `ci.yml` says
//! is a version that can be wrong. What only prose can carry — the trap, the
//! matrix leg a host does not cover — is the `caveat` on each job's row.

use comfy_table::presets::NOTHING;
use comfy_table::{ContentArrangement, Table};

use super::jobs::Job;

/// Rendered to a fixed width rather than the terminal's, so the output is the
/// same wherever it is pasted — an issue, a PR body, this repo's docs.
const WIDTH: u16 = 100;

const FOOTER: &str = "\
Env CI always sets: CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0 RUSTFLAGS=-D warnings

Other PR workflows, not part of `cargo xtask ci`:
  Nix CI      nix fmt -- --check flake.nix devenv.nix packaging/linux/package.nix \\
                packaging/linux/nixos-module.nix
              nix flake check --all-systems --no-build --show-trace
  devenv CI   nix fmt -- --check devenv.nix
              devenv --no-tui shell -- true
  Build       unsigned installers; only when touching xtask/packaging

Exact commands: cargo xtask ci --dry-run
Full map:       .claude/rules/ci.md
";

/// Shown with an unknown job name.
pub(crate) const JOB_NAMES_HELP: &str = "\
Jobs: rustfmt typos publish-closure shell clippy msrv rustdoc tests cargo-deny clippy-windows
Also: i18n wire
The focused suites are not CI jobs of their own; they fail the test jobs.
`cargo xtask ci --list` prints what each name runs.";

/// The whole `--list` output.
pub(crate) fn render() -> String {
    format!(
        "{}\n\nFocused suites — not CI jobs of their own; they fail the test jobs.\n\n{}\n\n{FOOTER}",
        table("CI job (ci.yml)", Job::default_run()),
        table("Suite", Job::focused()),
    )
}

fn table(heading: &str, jobs: impl Iterator<Item = Job>) -> String {
    let mut table = Table::new();
    table
        .load_style(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_width(WIDTH)
        .set_header([heading, "Runs on", "Notes"]);
    for job in jobs {
        table.add_row([job.name(), &job.runs_on(), job.caveat()]);
    }
    // Without a right border every row is padded out to the table width;
    // `trim_fmt` is what stops that reaching a terminal or a pasted doc.
    table.trim_fmt()
}

#[cfg(test)]
mod tests;
