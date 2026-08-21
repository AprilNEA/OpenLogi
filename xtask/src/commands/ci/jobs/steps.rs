//! What each job actually runs. The facts about a job — its name, its hosts —
//! are in the table one level up; this is only the behaviour.

use std::path::Path;

use anyhow::Result;
use xshell::{Shell, cmd};

use super::{Job, Plan};
use crate::commands::ci::{Host, Step};
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

/// The crates that must keep compiling with no OS underneath them.
///
/// Listed rather than excluded, unlike [`RUSTDOC_EXCLUDES`]: portability is a
/// property a crate earns and then has to keep, so a new crate is *not*
/// portable by default. Adding a name here is the claim; this job is what makes
/// it a fact.
///
/// `openlogi-core` qualifies only with its `fs` feature off — that feature is
/// the config file, and a config file needs a filesystem. Hence
/// `--no-default-features`, which is why this job checks it in its own pass.
const WASM_PORTABLE_CRATES: [&str; 1] = ["openlogi-hidpp"];

/// The crates that are portable once their host-facing feature is off.
const WASM_PORTABLE_NO_DEFAULT_CRATES: [&str; 1] = ["openlogi-core"];

/// The GPUI crates `cargo doc` skips — documenting them drags the whole
/// graphics toolchain into the job. Excluding by name rather than listing the
/// covered crates keeps a new crate documented by default.
const RUSTDOC_EXCLUDES: [&str; 4] = [
    "openlogi-ui",
    "openlogi-desktop",
    "openlogi-overlay",
    "openlogi-agent",
];

const CLIPPY_ARGS: [&str; 6] = [
    "clippy",
    "--workspace",
    "--all-targets",
    "--",
    "-D",
    "warnings",
];

/// One way to invoke a tool: the binary that has to be on PATH, the program to
/// run, and the arguments that come before the tool's own.
struct Invocation {
    probe: &'static str,
    program: &'static str,
    prefix: &'static [&'static str],
    /// Printed when this is not the first choice.
    note: Option<&'static str>,
}

/// The first invocation this host can actually perform.
fn first_available(candidates: &'static [Invocation]) -> Option<&'static Invocation> {
    candidates
        .iter()
        .find(|candidate| command_exists(candidate.probe))
}

/// cargo-deny, however this host can reach it.
const CARGO_DENY: [Invocation; 2] = [
    Invocation {
        probe: "cargo-deny",
        program: "cargo",
        prefix: &["deny"],
        note: None,
    },
    Invocation {
        probe: "nix",
        program: "nix",
        prefix: &["run", "nixpkgs#cargo-deny", "--"],
        note: Some("cargo-deny is not installed; running it through nix"),
    },
];

/// `cargo-clippy clippy`, not `cargo clippy`: cargo resolves an external
/// subcommand from `$CARGO_HOME/bin` before PATH, so on a machine with rustup
/// installed `cargo clippy` runs rustup's clippy against this shell's cargo —
/// a different compiler, and an outright failure when rustup's toolchain has
/// no windows-gnu std.
const WINDOWS_CLIPPY: [Invocation; 2] = [
    Invocation {
        probe: "cargo-clippy",
        program: "cargo-clippy",
        prefix: &["clippy"],
        note: None,
    },
    Invocation {
        probe: "cargo",
        program: "cargo",
        prefix: &["clippy"],
        note: Some("cargo-clippy is not on PATH; `cargo clippy` may resolve rustup's"),
    },
];

pub(super) fn plan(job: Job, sh: &Shell, host: Host) -> Result<Plan> {
    match job {
        Job::Rustfmt => Ok(Plan::run(
            job,
            [Step::new("cargo").args(["fmt", "--all", "--", "--check"])],
        )),
        Job::Shell => shell(job, sh),
        Job::Clippy => Ok(clippy(job, host)),
        Job::Msrv => msrv(job, sh, host),
        Job::Rustdoc => Ok(rustdoc(job)),
        Job::TestsLinux => Ok(Plan::run(
            job,
            [Step::new("cargo").args(["test", "--workspace", "--exclude", "openlogi-desktop"])],
        )),
        Job::TestsMacos => Ok(tests_macos(job)),
        Job::CargoDeny => Ok(cargo_deny(job)),
        Job::ClippyWindows => clippy_windows(job, sh, host),
        Job::Wasm => wasm(job, sh),
        Job::I18n => Ok(Plan::run(
            job,
            [Step::new("cargo").args(["test", "-p", "openlogi-desktop", "i18n"])],
        )),
        Job::Wire => Ok(Plan::run(
            job,
            [Step::new("cargo").args(["test", "-p", "openlogi-ipc", "--test", "wire_format"])],
        )),
    }
}

/// shellcheck and shfmt over every tracked shell script.
///
/// shfmt decides what counts as one — by extension, and by shebang for the
/// extensionless scripts — so a new script is covered the day it lands, and
/// `.devenv/`'s generated shells stay out because they are not tracked.
fn shell(job: Job, sh: &Shell) -> Result<Plan> {
    if !command_exists("shellcheck") || !command_exists("shfmt") {
        return Ok(Plan::skip(
            job,
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
        job,
        [
            Step::new("shellcheck").args(&scripts),
            // No printer flags: passing one would make shfmt ignore
            // `.editorconfig`, where this repo's formatting options live.
            Step::new("shfmt").args(["-d"]).args(&scripts),
        ],
    )
    .note(format!("{} tracked shell scripts", scripts.len())))
}

fn clippy(job: Job, host: Host) -> Plan {
    let plan = Plan::run(job, [Step::new("cargo").args(CLIPPY_ARGS)]);
    match host {
        Host::Linux => plan.note("matches CI job 'clippy' (ubuntu-latest)"),
        Host::Windows => {
            plan.note("host Windows clippy; CI's 'clippy' job is ubuntu — also run clippy-windows")
        }
        _ => plan.note(format!(
            "host {host} clippy. CI's 'clippy' job is ubuntu-latest and compiles linux cfg — this is not that job"
        )),
    }
}

/// `cargo check` at the declared floor.
///
/// `rust-toolchain.toml` pins the channel to `stable` and rustup honours that
/// file over an installed toolchain, so this job means nothing unless
/// `RUSTUP_TOOLCHAIN` outranks it — which is why CI sets it too.
fn msrv(job: Job, sh: &Shell, host: Host) -> Result<Plan> {
    let floor = workspace_package(&sh.current_dir())?.rust_version;
    let label = format!("MSRV (cargo check, {host}), rustc {floor}");
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
        return Ok(Plan::run(job, [check().env("RUSTUP_TOOLCHAIN", &floor)]).label(label));
    }

    // Without rustup — a Nix toolchain, say — the floor is only reachable if
    // the pinned compiler already is that version.
    let installed = cmd!(sh, "rustc -vV").quiet().read().unwrap_or_default();
    if installed.lines().any(|line| {
        // `rust-version = "1.98"` is satisfied by any 1.98.x compiler.
        line.strip_prefix("release: ")
            .is_some_and(|version| version == floor || version.starts_with(&format!("{floor}.")))
    }) {
        return Ok(Plan::run(job, [check()]).label(label).note(format!(
            "RUSTUP_TOOLCHAIN={floor} unavailable; rustc already is {floor}"
        )));
    }

    Ok(Plan::skip(
        job,
        format!(
            "install the floor: rustup toolchain install {floor} (then rerun). \
             rust-toolchain.toml pins stable, so a floating toolchain is not this job"
        ),
    ))
}

fn rustdoc(job: Job) -> Plan {
    let excludes = RUSTDOC_EXCLUDES
        .iter()
        .flat_map(|crate_name| ["--exclude", crate_name]);
    Plan::run(
        job,
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

fn tests_macos(job: Job) -> Plan {
    let arch = std::env::consts::ARCH;
    Plan::run(
        job,
        [Step::new("cargo").args(["test", "--workspace", "--all-targets"])],
    )
    .label(format!("tests (macos, {arch})"))
    .note(format!(
        "CI also has a macos-15-intel x86_64 leg — this host only covers {arch}"
    ))
}

/// The dependency policy, rooted at the CLI: exactly the crates published to
/// crates.io. cargo-deny picks its roots from the manifest it is given, and a
/// virtual workspace root would drag the git-pinned gpui tree into the graph.
fn cargo_deny(job: Job) -> Plan {
    const ARGS: [&str; 4] = [
        "--all-features",
        "--manifest-path",
        "crates/openlogi/Cargo.toml",
        "check",
    ];

    let Some(invocation) = first_available(&CARGO_DENY) else {
        return Plan::skip(
            job,
            "install cargo-deny (cargo install cargo-deny --locked) or nix",
        );
    };
    let plan = Plan::run(
        job,
        [Step::new(invocation.program)
            .args(invocation.prefix)
            .args(ARGS)],
    );
    match invocation.note {
        Some(note) => plan.note(note),
        None => plan,
    }
}

/// Check the portable crates for `wasm32-unknown-unknown`.
///
/// Not a build anyone ships: no wasm artifact exists. The target is chosen
/// precisely because it has no OS under it, so a crate that quietly grows a
/// host-bound dependency — a filesystem, a randomness source, a thread — stops
/// compiling here and nowhere else.
///
/// It covers only the crates that pass today. Widening the list is how a crate
/// declares itself portable, and the day one of them regresses this job is what
/// says so.
fn wasm(job: Job, sh: &Shell) -> Result<Plan> {
    let sysroot = cmd!(sh, "rustc --print sysroot").quiet().read()?;
    if !Path::new(sysroot.trim())
        .join("lib/rustlib/wasm32-unknown-unknown")
        .is_dir()
    {
        return Ok(Plan::skip(
            job,
            "missing wasm32-unknown-unknown std (devenv, or: rustup target add wasm32-unknown-unknown)",
        ));
    }

    let crates = WASM_PORTABLE_CRATES
        .iter()
        .flat_map(|crate_name| ["-p", crate_name]);
    let no_default = WASM_PORTABLE_NO_DEFAULT_CRATES
        .iter()
        .flat_map(|crate_name| ["-p", crate_name]);
    Ok(Plan::run(
        job,
        [
            Step::new("cargo")
                .args(["check"])
                .args(crates)
                .args(["--target", "wasm32-unknown-unknown"]),
            Step::new("cargo").args(["check"]).args(no_default).args([
                "--no-default-features",
                "--target",
                "wasm32-unknown-unknown",
            ]),
        ],
    ))
}

fn clippy_windows(job: Job, sh: &Shell, host: Host) -> Result<Plan> {
    if host == Host::Windows {
        return Ok(Plan::run(job, [Step::new("cargo").args(CLIPPY_ARGS)]));
    }

    let sysroot = cmd!(sh, "rustc --print sysroot").quiet().read()?;
    if !Path::new(sysroot.trim())
        .join("lib/rustlib/x86_64-pc-windows-gnu")
        .is_dir()
    {
        return Ok(Plan::skip(
            job,
            "missing x86_64-pc-windows-gnu std (devenv, or: rustup target add x86_64-pc-windows-gnu)",
        )
        .label("clippy (windows) proxy"));
    }

    let Some(invocation) = first_available(&WINDOWS_CLIPPY) else {
        return Ok(Plan::skip(job, "no clippy on PATH").label("clippy (windows) proxy"));
    };

    let crates = WINDOWS_LINT_CRATES
        .iter()
        .flat_map(|crate_name| ["-p", crate_name]);
    let plan = Plan::run(
        job,
        [Step::new(invocation.program)
            .args(invocation.prefix)
            .args(["--target", "x86_64-pc-windows-gnu"])
            .args(crates)
            .args(["--all-targets", "--", "-D", "warnings"])],
    )
    .label("clippy (windows) proxy")
    .note("CI runs the whole workspace on windows-latest; this is the ring-free cross lint, not that job");
    Ok(match invocation.note {
        Some(note) => plan.note(note),
        None => plan,
    })
}
