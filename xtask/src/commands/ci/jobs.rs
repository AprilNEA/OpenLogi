//! The jobs in `ci.yml`: one row of facts each, and the plan each produces on
//! this host.
//!
//! A job's plan is built before anything runs, so `--dry-run` prints exactly
//! the commands a real run would execute.

mod steps;

use anyhow::Result;
use strum::{EnumIter, IntoEnumIterator as _};
use xshell::Shell;

use super::{Host, Step};

/// A job in `ci.yml`, plus the focused suites that are not jobs of their own.
///
/// Declaration order is workflow order — it is what a default run follows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, EnumIter)]
pub(crate) enum Job {
    Rustfmt,
    Shell,
    Clippy,
    Msrv,
    Rustdoc,
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

/// Everything about a job that is a fact rather than behaviour.
///
/// One row per job, returned from one match, so the compiler is what proves
/// every job has a full set of facts — and adding a job is one row rather than
/// an edit to four parallel matches.
#[derive(Clone, Copy)]
struct Spec {
    /// The CI `name:`, with any matrix leg left out. Also the summary's label,
    /// unless the plan fills that leg in.
    name: &'static str,
    /// The other names it answers to: the workflow job id, short forms.
    aliases: &'static [&'static str],
    /// CI renders the matrix leg into the name (`tests (macos, arm64)`), so a
    /// name copied out of a run can only match on a prefix.
    prefix: Option<&'static str>,
    /// The hosts that can run this job at all. Anywhere else it is skipped and
    /// the summary names it as not run.
    hosts: &'static [Host],
    /// Part of a bare `cargo xtask ci`. The focused suites are not.
    in_default_run: bool,
    /// What `--list` says about it: the trap, or how a local run differs from
    /// CI's. Empty when the command speaks for itself.
    caveat: &'static str,
}

/// The names that select more than one job, because `ci.yml` has more than one.
const GROUPS: [(&str, &[Job]); 2] = [
    ("tests", &[Job::TestsLinux, Job::TestsMacos]),
    ("test", &[Job::TestsLinux, Job::TestsMacos]),
];

impl Job {
    fn spec(self) -> Spec {
        match self {
            Self::Rustfmt => Spec {
                name: "rustfmt",
                aliases: &["fmt"],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "",
            },
            Self::Shell => Spec {
                name: "shell",
                aliases: &[],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "shellcheck and shfmt over every tracked shell script. shfmt decides what counts as one — by extension, and by shebang for the extensionless ones — and takes its formatting options from .editorconfig, which any printer flag would discard.",
            },
            Self::Clippy => Spec {
                name: "clippy",
                aliases: &[],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "CI runs it on ubuntu-latest, so it compiles linux cfg. Host clippy on macOS or Windows is a different compilation, not this job.",
            },
            Self::Msrv => Spec {
                name: "MSRV (cargo check)",
                aliases: &["msrv"],
                prefix: Some("MSRV (cargo check"),
                hosts: &[Host::Linux, Host::Macos],
                in_default_run: true,
                caveat: "rust-toolchain.toml pins the channel to stable and rustup honours that over an installed toolchain, so CI and this runner both set RUSTUP_TOOLCHAIN to the rust-version floor — without it the check silently runs stable.",
            },
            Self::Rustdoc => Spec {
                name: "rustdoc (non-GUI crates)",
                aliases: &["rustdoc", "docs"],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "Everything but the GPUI crates, which would drag the whole graphics toolchain into the job. A broken intra-doc link is neither a compile error nor a clippy lint, so nothing else catches one.",
            },
            Self::TestsLinux => Spec {
                name: "tests (linux)",
                aliases: &["test-linux"],
                prefix: None,
                hosts: &[Host::Linux],
                in_default_run: true,
                caveat: "Excludes openlogi-desktop, so the i18n locale-parity tests never run on Linux CI.",
            },
            Self::TestsMacos => Spec {
                name: "tests (macos)",
                aliases: &["test-macos"],
                prefix: Some("tests (macos"),
                hosts: &[Host::Macos],
                in_default_run: true,
                caveat: "CI's matrix is arm64 (macos-latest) and x86_64 (macos-15-intel); a host only ever covers its own arch.",
            },
            Self::CargoDeny => Spec {
                name: "cargo-deny",
                aliases: &["deny"],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "Rooted at crates/openlogi — exactly the crates published to crates.io. Falls back to `nix run nixpkgs#cargo-deny` when the binary is not installed.",
            },
            Self::ClippyWindows => Spec {
                name: "clippy (windows)",
                aliases: &["clippy-windows"],
                prefix: None,
                // Everywhere: natively on Windows, and elsewhere as the
                // cross-lint proxy.
                hosts: Host::ANY,
                in_default_run: true,
                caveat: "CI lints the whole workspace natively on windows-latest. Anywhere else this is the ring-free cross lint over the crates that carry Windows code — a proxy, not that job.",
            },
            Self::I18n => Spec {
                name: "i18n",
                aliases: &[],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: false,
                caveat: "Locale parity. Part of tests (macos), and the suite Linux CI cannot run because it excludes openlogi-desktop.",
            },
            Self::Wire => Spec {
                name: "wire_format",
                aliases: &["wire"],
                prefix: None,
                hosts: Host::ANY,
                in_default_run: false,
                caveat: "The bincode/tarpc golden wire format. Part of the test jobs.",
            },
        }
    }

    /// The jobs a bare `cargo xtask ci` runs — every job in `ci.yml`, in
    /// workflow order. Both test jobs are in it: on a host that cannot run one
    /// of them, a named skip is the honest report, and silence is not.
    pub(crate) fn default_run() -> impl Iterator<Item = Self> {
        Self::iter().filter(|job| job.spec().in_default_run)
    }

    /// The suites that are not jobs of their own — the other half of
    /// [`Job::default_run`].
    pub(crate) fn focused() -> impl Iterator<Item = Self> {
        Self::iter().filter(|job| !job.spec().in_default_run)
    }

    /// The CI `name:`, before a plan fills in the matrix leg this host covers.
    pub(crate) fn name(self) -> &'static str {
        self.spec().name
    }

    /// The hosts CI gives this job, as `--list` names them.
    pub(crate) fn runs_on(self) -> String {
        let hosts = self.spec().hosts;
        if hosts == Host::ANY {
            return "any".to_owned();
        }
        hosts
            .iter()
            .map(Host::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The trap worth knowing, or how a local run differs from CI's.
    pub(crate) fn caveat(self) -> &'static str {
        self.spec().caveat
    }

    /// Every name this job answers to, the matrix-leg prefix included. Only the
    /// drift tests enumerate them; resolution goes through
    /// [`Job::answers_to`].
    #[cfg(test)]
    pub(crate) fn names(self) -> impl Iterator<Item = &'static str> {
        let spec = self.spec();
        std::iter::once(spec.name)
            .chain(spec.aliases.iter().copied())
            .chain(spec.prefix)
    }

    /// The jobs a name on the command line selects.
    pub(crate) fn resolve(name: &str) -> Option<Vec<Self>> {
        if let Some((_, jobs)) = GROUPS
            .iter()
            .find(|(group, _)| group.eq_ignore_ascii_case(name))
        {
            return Some((*jobs).to_vec());
        }
        Self::iter()
            .find(|job| job.answers_to(name))
            .map(|job| vec![job])
    }

    fn answers_to(self, name: &str) -> bool {
        let spec = self.spec();
        spec.name.eq_ignore_ascii_case(name)
            || spec
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
            || spec.prefix.is_some_and(|prefix| name.starts_with(prefix))
    }

    pub(crate) fn plan(self, sh: &Shell, host: Host) -> Result<Plan> {
        let spec = self.spec();
        if !spec.hosts.contains(&host) {
            let runs_on: Vec<String> = spec.hosts.iter().map(Host::to_string).collect();
            return Ok(Plan::skip(
                self,
                format!(
                    "CI runs it on {}; this host is {host}",
                    runs_on.join(" and ")
                ),
            ));
        }
        steps::plan(self, sh, host)
    }
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

impl Plan {
    fn run(job: Job, steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            label: job.spec().name.to_owned(),
            notes: Vec::new(),
            action: Action::Run(steps.into_iter().collect()),
        }
    }

    fn skip(job: Job, reason: impl Into<String>) -> Self {
        Self {
            label: job.spec().name.to_owned(),
            notes: Vec::new(),
            action: Action::Skip(reason.into()),
        }
    }

    /// Fill in the matrix leg this host covers, or say that the command is a
    /// proxy for the CI job rather than the job itself.
    fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[cfg(test)]
mod tests;
