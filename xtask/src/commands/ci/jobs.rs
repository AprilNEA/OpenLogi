//! The jobs in `ci.yml`, and what running each one costs on this host.
//!
//! A job's plan is built before anything runs, so `--dry-run` prints exactly
//! the commands a real run would execute.

use std::path::Path;

use anyhow::Result;
use strum::{EnumIter, IntoEnumIterator as _};
use xshell::{Shell, cmd};

use super::Step;
use crate::support::fs::command_exists;
use crate::support::manifest::workspace_package;

/// The crates carrying `cfg(target_os = "windows")` code that can be linted
/// from a Unix host, i.e. CI's `clippy (windows)` minus what cannot
/// cross-compile.
///
/// `clippy --target` is check-only (no linker needed), but a C-compiling build
/// dependency does need a cross C toolchain: `openlogi-{assets,cli}` and the
/// root `openlogi` pull ureq → ring, whose `curve25519.c` cannot cross-compile
/// from macOS without mingw. They have no Windows-specific code, so this is the
/// ring-free agent/leaf subset; CI covers the rest natively. The GUI crates are
/// out because GPUI has no Windows backend.
///
/// A crate missing here is a crate whose Windows paths nothing checks until CI
/// — which is how three `chunks_exact` sites in `openlogi-camera` survived a
/// whole lint sweep.
const WINDOWS_LINT_CRATES: [&str; 8] = [
    "openlogi-core",
    "openlogi-hidpp",
    "openlogi-hid",
    "openlogi-hook",
    "openlogi-inject",
    "openlogi-camera",
    "openlogi-agent",
    "openlogi-agent-core",
];

/// The GPUI crates `cargo doc` skips — documenting them drags the whole
/// graphics toolchain into the job. Excluding by name rather than listing the
/// covered crates keeps a new crate documented by default.
const RUSTDOC_EXCLUDES: [&str; 4] = [
    "openlogi-ui",
    "openlogi-desktop",
    "openlogi-overlay",
    "openlogi-agent",
];

/// A job in `ci.yml`, plus the focused suites that are not jobs of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, EnumIter)]
pub(crate) enum Job {
    Rustfmt,
    Shell,
    Clippy,
    Msrv,
    Rustdoc,
    /// The host's lane of the two test jobs.
    Tests,
    TestsLinux,
    TestsMacos,
    CargoDeny,
    ClippyWindows,
    /// Locale parity. Part of `tests (macos)`, and the suite Linux CI cannot
    /// run because it excludes `openlogi-desktop`.
    I18n,
    /// The bincode/tarpc golden wire format. Part of the test jobs.
    Wire,
}

/// What a job will do on this host.
pub(crate) struct Plan {
    /// What the summary calls this job: the CI `name:`, with the matrix leg
    /// this host covers filled in.
    pub(crate) label: String,
    /// How this host's run differs from CI's.
    pub(crate) notes: Vec<String>,
    pub(crate) action: Action,
}

pub(crate) enum Action {
    Run(Vec<Step>),
    /// Why this host cannot reproduce the job.
    Skip(String),
}

impl Job {
    /// The jobs a bare `cargo xtask ci` runs — every job in `ci.yml`, in
    /// workflow order. The focused suites are reachable by name only.
    pub(crate) const DEFAULT_RUN: [Self; 8] = [
        Self::Rustfmt,
        Self::Shell,
        Self::Clippy,
        Self::Msrv,
        Self::Rustdoc,
        Self::Tests,
        Self::CargoDeny,
        Self::ClippyWindows,
    ];

    /// Every name this job answers to: its CI `name:`, its workflow job id, and
    /// the short forms worth typing.
    pub(crate) fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Rustfmt => &["rustfmt", "fmt"],
            Self::Shell => &["shell"],
            Self::Clippy => &["clippy"],
            Self::Msrv => &["msrv"],
            Self::Rustdoc => &["rustdoc", "docs", "rustdoc (non-GUI crates)"],
            Self::Tests => &["tests", "test"],
            Self::TestsLinux => &["tests (linux)", "test-linux"],
            Self::TestsMacos => &["test-macos"],
            Self::CargoDeny => &["cargo-deny", "deny"],
            Self::ClippyWindows => &["clippy-windows", "clippy (windows)"],
            Self::I18n => &["i18n"],
            Self::Wire => &["wire", "wire_format"],
        }
    }

    /// Resolve a job named on the command line.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::iter()
            .find(|job| {
                job.aliases()
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
            })
            .or_else(|| {
                // CI renders these two with their matrix leg in the name, so an
                // exact alias cannot cover what someone copies out of a run.
                if name.starts_with("MSRV (cargo check") {
                    Some(Self::Msrv)
                } else if name.starts_with("tests (macos") {
                    Some(Self::TestsMacos)
                } else {
                    None
                }
            })
    }

    pub(crate) fn plan(self, sh: &Shell) -> Result<Plan> {
        match self {
            Self::Rustfmt => Ok(Plan::run(
                "rustfmt",
                [Step::new("cargo").args(["fmt", "--all", "--", "--check"])],
            )),
            Self::Shell => shell(sh),
            Self::Clippy => Ok(clippy()),
            Self::Msrv => msrv(sh),
            Self::Rustdoc => Ok(rustdoc()),
            Self::Tests => Ok(tests()),
            Self::TestsLinux => Ok(tests_linux()),
            Self::TestsMacos => Ok(tests_macos()),
            Self::CargoDeny => Ok(cargo_deny()),
            Self::ClippyWindows => clippy_windows(sh),
            Self::I18n => Ok(Plan::run(
                "i18n",
                [Step::new("cargo").args(["test", "-p", "openlogi-desktop", "i18n"])],
            )),
            Self::Wire => {
                Ok(Plan::run(
                    "wire_format",
                    [Step::new("cargo").args([
                        "test",
                        "-p",
                        "openlogi-ipc",
                        "--test",
                        "wire_format",
                    ])],
                ))
            }
        }
    }
}

impl Plan {
    fn run(label: impl Into<String>, steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            label: label.into(),
            notes: Vec::new(),
            action: Action::Run(steps.into_iter().collect()),
        }
    }

    fn skip(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            notes: Vec::new(),
            action: Action::Skip(reason.into()),
        }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// shellcheck and shfmt over every tracked shell script.
///
/// shfmt decides what counts as one — by extension, and by shebang for the
/// extensionless scripts — so a new script is covered the day it lands, and
/// `.devenv/`'s generated shells stay out because they are not tracked.
fn shell(sh: &Shell) -> Result<Plan> {
    if !command_exists("shellcheck") || !command_exists("shfmt") {
        return Ok(Plan::skip(
            "shell",
            "needs shellcheck and shfmt (both are in the devenv shell)",
        ));
    }

    let tracked = cmd!(sh, "git ls-files -z").quiet().read()?;
    let tracked: Vec<&str> = tracked
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();
    let scripts = cmd!(sh, "shfmt -f {tracked...}").quiet().read()?;
    let scripts: Vec<&str> = scripts.lines().collect();

    Ok(Plan::run(
        "shell",
        [
            Step::new("shellcheck").args(&scripts),
            // No printer flags: passing one would make shfmt ignore
            // `.editorconfig`, where this repo's formatting options live.
            Step::new("shfmt").args(["-d"]).args(&scripts),
        ],
    )
    .note(format!("{} tracked shell scripts", scripts.len())))
}

fn clippy() -> Plan {
    let plan = Plan::run(
        "clippy",
        [Step::new("cargo").args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])],
    );
    if cfg!(target_os = "linux") {
        plan.note("matches CI job 'clippy' (ubuntu-latest)")
    } else if cfg!(target_os = "windows") {
        plan.note("host Windows clippy; CI's 'clippy' job is ubuntu — also run clippy-windows")
    } else {
        plan.note(
            "host clippy. CI's 'clippy' job is ubuntu-latest and compiles linux cfg — this is not that job",
        )
    }
}

/// `cargo check` at the declared floor.
///
/// `rust-toolchain.toml` pins the channel to `stable` and rustup honours that
/// file over an installed toolchain, so this job means nothing unless
/// `RUSTUP_TOOLCHAIN` outranks it — which is why CI sets it too.
fn msrv(sh: &Shell) -> Result<Plan> {
    let label = format!("MSRV (cargo check, {})", std::env::consts::OS);
    let label = label.as_str();
    if cfg!(target_os = "windows") {
        return Ok(Plan::skip(
            label,
            "CI's msrv matrix is macos-latest + ubuntu-latest, not Windows",
        ));
    }

    let floor = workspace_package(&sh.current_dir())?.rust_version;
    let check = || Step::new("cargo").args(["check", "--workspace", "--all-targets"]);

    if command_exists("rustup")
        && cmd!(sh, "rustc -vV")
            .env("RUSTUP_TOOLCHAIN", &floor)
            .quiet()
            .ignore_stdout()
            .ignore_stderr()
            .run()
            .is_ok()
    {
        return Ok(Plan::run(
            format!("{label}, rustc {floor}"),
            [check().env("RUSTUP_TOOLCHAIN", &floor)],
        ));
    }

    // Without rustup — a Nix toolchain, say — the floor is only reachable if
    // the pinned compiler already is that version.
    let installed = cmd!(sh, "rustc -vV").quiet().read().unwrap_or_default();
    if installed.lines().any(|line| {
        // `rust-version = "1.98"` is satisfied by any 1.98.x compiler.
        line.strip_prefix("release: ")
            .is_some_and(|version| version == floor || version.starts_with(&format!("{floor}.")))
    }) {
        return Ok(
            Plan::run(format!("{label}, rustc {floor}"), [check()]).note(format!(
                "RUSTUP_TOOLCHAIN={floor} unavailable; rustc already is {floor}"
            )),
        );
    }

    Ok(Plan::skip(
        label,
        format!(
            "install the floor: rustup toolchain install {floor} (then rerun). \
             rust-toolchain.toml pins stable, so a floating toolchain is not this job"
        ),
    ))
}

fn rustdoc() -> Plan {
    let excludes = RUSTDOC_EXCLUDES
        .iter()
        .flat_map(|crate_name| ["--exclude", crate_name]);
    Plan::run(
        "rustdoc (non-GUI crates)",
        [Step::new("cargo")
            .args([
                "doc",
                "--workspace",
                "--no-deps",
                "--document-private-items",
            ])
            .args(excludes)
            .env("RUSTDOCFLAGS", "-D warnings")],
    )
}

/// The host's lane of the two test jobs.
fn tests() -> Plan {
    if cfg!(target_os = "linux") {
        tests_linux()
    } else if cfg!(target_os = "macos") {
        tests_macos()
    } else {
        Plan::skip(
            "tests",
            "CI has no Windows test job (clippy-windows only). To run them anyway: cargo test --workspace --all-targets",
        )
    }
}

fn tests_linux() -> Plan {
    if !cfg!(target_os = "linux") {
        return Plan::skip(
            "tests (linux)",
            format!(
                "needs Linux; this host is {}. Running the macOS tests is not this job",
                std::env::consts::OS
            ),
        );
    }
    Plan::run(
        "tests (linux)",
        [Step::new("cargo").args(["test", "--workspace", "--exclude", "openlogi-desktop"])],
    )
}

fn tests_macos() -> Plan {
    if !cfg!(target_os = "macos") {
        return Plan::skip(
            "tests (macos)",
            format!("needs macOS; this host is {}", std::env::consts::OS),
        );
    }
    let arch = std::env::consts::ARCH;
    Plan::run(
        format!("tests (macos, {arch})"),
        [Step::new("cargo").args(["test", "--workspace", "--all-targets"])],
    )
    .note(format!(
        "CI also has a macos-15-intel x86_64 leg — this host only covers {arch}"
    ))
}

/// The dependency policy, rooted at the CLI: exactly the crates published to
/// crates.io. cargo-deny picks its roots from the manifest it is given, and a
/// virtual workspace root would drag the git-pinned gpui tree into the graph.
fn cargo_deny() -> Plan {
    let args = [
        "--all-features",
        "--manifest-path",
        "crates/openlogi/Cargo.toml",
        "check",
    ];
    if command_exists("cargo-deny") {
        Plan::run("cargo-deny", [Step::new("cargo").args(["deny"]).args(args)])
    } else if command_exists("nix") {
        Plan::run(
            "cargo-deny",
            [Step::new("nix")
                .args(["run", "nixpkgs#cargo-deny", "--"])
                .args(args)],
        )
        .note("cargo-deny is not installed; running it through nix")
    } else {
        Plan::skip(
            "cargo-deny",
            "install cargo-deny (cargo install cargo-deny --locked) or nix",
        )
    }
}

fn clippy_windows(sh: &Shell) -> Result<Plan> {
    let label = "clippy (windows)";
    if cfg!(target_os = "windows") {
        return Ok(Plan::run(
            label,
            [Step::new("cargo").args([
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])],
        ));
    }

    let sysroot = cmd!(sh, "rustc --print sysroot").quiet().read()?;
    if !Path::new(sysroot.trim())
        .join("lib/rustlib/x86_64-pc-windows-gnu")
        .is_dir()
    {
        return Ok(Plan::skip(
            format!("{label} proxy"),
            "missing x86_64-pc-windows-gnu std (devenv, or: rustup target add x86_64-pc-windows-gnu)",
        ));
    }

    // `cargo-clippy clippy`, not `cargo clippy`: cargo resolves an external
    // subcommand from `$CARGO_HOME/bin` before PATH, so on a machine with
    // rustup installed `cargo clippy` runs rustup's clippy against this
    // shell's cargo — a different compiler, and an outright failure when
    // rustup's toolchain has no windows-gnu std.
    let program = if command_exists("cargo-clippy") {
        "cargo-clippy"
    } else {
        "cargo"
    };
    let crates = WINDOWS_LINT_CRATES
        .iter()
        .flat_map(|crate_name| ["-p", crate_name]);
    Ok(Plan::run(
        format!("{label} proxy"),
        [Step::new(program)
            .args(["clippy", "--target", "x86_64-pc-windows-gnu"])
            .args(crates)
            .args(["--all-targets", "--", "-D", "warnings"])],
    )
    .note("CI runs the whole workspace on windows-latest; this is the ring-free cross lint, not that job"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use strum::IntoEnumIterator as _;

    use super::Job;

    #[test]
    fn every_alias_resolves_to_its_own_job() {
        for job in Job::iter() {
            for alias in job.aliases() {
                assert_eq!(Job::from_name(alias), Some(job), "alias {alias}");
            }
        }
    }

    #[test]
    fn aliases_are_unique_across_jobs() {
        let mut owners: HashMap<&str, Job> = HashMap::new();
        for job in Job::iter() {
            for alias in job.aliases() {
                if let Some(other) = owners.insert(alias, job) {
                    panic!("alias {alias} is claimed by both {other:?} and {job:?}");
                }
            }
        }
    }

    #[test]
    fn matrix_leg_names_resolve() {
        // What someone copies out of a CI run's job list.
        for name in [
            "MSRV (cargo check, macos-latest)",
            "MSRV (cargo check, ubuntu-latest)",
        ] {
            assert_eq!(Job::from_name(name), Some(Job::Msrv), "name {name}");
        }
        for name in ["tests (macos, arm64)", "tests (macos, x86_64)"] {
            assert_eq!(Job::from_name(name), Some(Job::TestsMacos), "name {name}");
        }
    }

    #[test]
    fn unknown_job_names_do_not_resolve() {
        assert_eq!(Job::from_name("nightly"), None);
        assert_eq!(Job::from_name(""), None);
    }

    #[test]
    fn the_default_run_is_the_ci_jobs_only() {
        // The focused suites are not jobs in ci.yml; a bare run must not claim
        // to have covered a pipeline job by running them.
        for job in [Job::I18n, Job::Wire, Job::TestsLinux, Job::TestsMacos] {
            assert!(
                !Job::DEFAULT_RUN.contains(&job),
                "{job:?} is not a ci.yml job"
            );
        }
        for job in [
            Job::Rustfmt,
            Job::Shell,
            Job::Clippy,
            Job::Msrv,
            Job::Rustdoc,
            Job::Tests,
            Job::CargoDeny,
            Job::ClippyWindows,
        ] {
            assert!(Job::DEFAULT_RUN.contains(&job), "{job:?} is a ci.yml job");
        }
    }
}
