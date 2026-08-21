//! The `--list` table: what each CI job runs, and who can run it.
//!
//! Prose, not generated output — it carries the *why* a plan cannot (the matrix
//! legs a host does not cover, the traps). A test below keeps every job named
//! here, so adding one to [`super::jobs::Job`] without a row fails.

/// Printed by `cargo xtask ci --list`.
pub(crate) const TABLE: &str = "\
CI job (ci.yml)              Local command                                      This host
---------------------------  -------------------------------------------------  ---------
rustfmt                      cargo fmt --all -- --check                         any
shell                        shellcheck + shfmt -d over every tracked shell     any
                             script. shfmt decides what is one (extension, or
                             shebang for the extensionless). Formatting options
                             come from .editorconfig — a printer flag would
                             make shfmt ignore that file.
clippy                       cargo clippy --workspace --all-targets -- -D warnings
                             CI runs this on ubuntu-latest (linux cfg). Host
                             clippy on macOS/Windows is a different compilation.
MSRV (cargo check, <os>)     RUSTUP_TOOLCHAIN=<rust-version> \\
                               cargo check --workspace --all-targets
                             rust-version is in the root Cargo.toml. CI sets
                             RUSTUP_TOOLCHAIN because rust-toolchain.toml is
                             `stable` and would otherwise silently check stable.
rustdoc (non-GUI crates)     RUSTDOCFLAGS=\"-D warnings\" cargo doc --workspace \\
                               --no-deps --document-private-items \\
                               --exclude openlogi-ui --exclude openlogi-desktop \\
                               --exclude openlogi-overlay --exclude openlogi-agent
tests (linux)                cargo test --workspace --exclude openlogi-desktop  Linux
tests (macos, <arch>)        cargo test --workspace --all-targets               macOS
                             CI matrix: arm64 (macos-latest) and x86_64
                             (macos-15-intel). Linux excludes openlogi-desktop,
                             so i18n tests do not run on Linux CI.
cargo-deny                   cargo deny --all-features \\
                               --manifest-path crates/openlogi/Cargo.toml check
clippy (windows)             cargo clippy --workspace --all-targets -- -D warnings
                             (windows-latest). Elsewhere: the ring-free cross
                             lint — not the full workspace.

Env CI always sets: CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0 RUSTFLAGS=-D warnings

Focused suites (not their own CI jobs; they fail the test jobs):
  i18n   cargo test -p openlogi-desktop i18n
  wire   cargo test -p openlogi-ipc --test wire_format

Other PR workflows (not in the default run):
  Nix CI      nix fmt -- --check flake.nix devenv.nix packaging/linux/package.nix \\
                packaging/linux/nixos-module.nix
              nix flake check --all-systems --no-build --show-trace
  devenv CI   nix fmt -- --check devenv.nix
              devenv --no-tui shell -- true
  Build       unsigned installers; only when touching xtask/packaging

Full map: .claude/rules/ci.md
";

/// Shown with an unknown job name.
pub(crate) const JOB_NAMES_HELP: &str = "\
Jobs: rustfmt shell clippy msrv rustdoc tests cargo-deny clippy-windows
Also: i18n wire   (focused suites that fail the test jobs)
`cargo xtask ci --list` prints what each one runs.";

#[cfg(test)]
mod tests {
    use super::super::jobs::Job;
    use super::{JOB_NAMES_HELP, TABLE};

    /// A new CI job that nothing documents is the drift this guards against:
    /// the table is what a reader consults instead of opening `ci.yml`.
    #[test]
    fn every_ci_job_is_documented() {
        let table = TABLE.to_lowercase();
        let help = JOB_NAMES_HELP.to_lowercase();
        for job in Job::DEFAULT_RUN {
            let named = |text: &str| {
                job.aliases()
                    .iter()
                    .any(|alias| text.contains(&alias.to_lowercase()))
            };
            assert!(named(&table), "{job:?} has no row in --list");
            assert!(named(&help), "{job:?} is missing from the job-name help");
        }
    }
}
