//! Reproduce the jobs in `.github/workflows/ci.yml` that this host can run.
//!
//! Commands are copied from that workflow — change a `run:` there and the
//! matching plan in [`jobs`] and the table in `.claude/rules/ci.md` change with
//! it. A job this host cannot run is **skipped, not passed**: the summary names
//! it so a PR's Testing section can too.

mod jobs;
mod list;

use std::ffi::{OsStr, OsString};
use std::fmt;

use anyhow::{Result, bail};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::fs::repo_root;
use jobs::{Action, Job};

/// The operating systems `ci.yml` names in its `runs-on:`, plus whatever else
/// someone might be sitting at.
///
/// A runtime value rather than `cfg!(target_os = …)` so that which host a job
/// needs is data a test can read, instead of a branch that only exists in a
/// build for that host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Host {
    Linux,
    Macos,
    Windows,
    /// Something CI has no runner for. Jobs that name their hosts skip here.
    Other,
}

impl Host {
    /// For a job whose command does not depend on the operating system.
    pub(crate) const ANY: &'static [Self] = &[Self::Linux, Self::Macos, Self::Windows, Self::Other];

    pub(crate) fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Self::Linux,
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
            // Only ever the host this binary is running on, so naming it is
            // more useful than "other".
            Self::Other => std::env::consts::OS,
        })
    }
}

/// The env CI sets for every job. A caller that already set one keeps theirs.
const CI_ENV: [(&str, &str); 3] = [
    ("CARGO_TERM_COLOR", "always"),
    ("CARGO_INCREMENTAL", "0"),
    ("RUSTFLAGS", "-D warnings"),
];

#[derive(Parser)]
pub(crate) struct Args {
    /// Print the job → command table and exit.
    #[arg(long)]
    list: bool,
    /// Print what each job would run without running it.
    #[arg(long)]
    dry_run: bool,
    /// Jobs to run — a CI `name:` or a job id. Default: every job this host can
    /// reproduce.
    #[arg(value_name = "JOB")]
    jobs: Vec<String>,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    if args.list {
        println!("{}", list::render());
        return Ok(());
    }

    let sh = Shell::new()?;
    sh.change_dir(repo_root()?);
    for (key, value) in CI_ENV {
        if std::env::var_os(key).is_none() {
            sh.set_var(key, value);
        }
    }

    let selected: Vec<Job> = if args.jobs.is_empty() {
        Job::default_run().collect()
    } else {
        args.jobs
            .iter()
            .map(|name| {
                Job::resolve(name).ok_or_else(|| {
                    anyhow::anyhow!("unknown job: {name}\n\n{}", list::JOB_NAMES_HELP)
                })
            })
            .collect::<Result<Vec<_>>>()?
            .concat()
    };

    let host = Host::current();
    let mut summary = Summary::default();
    for job in selected {
        summary.run(&sh, job, host, args.dry_run)?;
    }
    summary.finish()
}

/// One external command a job runs, with the env CI gives it.
pub(crate) struct Step {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(&'static str, String)>,
}

impl Step {
    pub(crate) fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub(crate) fn env(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.env.push((key, value.into()));
        self
    }

    fn run(&self, sh: &Shell) -> Result<()> {
        let program = &self.program;
        let args = &self.args;
        // `quiet`: the plan is printed before the run, so xshell echoing it
        // again would double every command.
        let mut command = cmd!(sh, "{program} {args...}").quiet();
        for (key, value) in &self.env {
            command = command.env(key, value);
        }
        command.run()?;
        Ok(())
    }
}

impl fmt::Display for Step {
    /// The command as a reader would type it — printed in full, including the
    /// `shell` job's file list, so a failing job can be rerun by copying the
    /// line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in &self.env {
            write!(f, "{key}=\"{value}\" ")?;
        }
        write!(f, "{}", self.program.to_string_lossy())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

/// What every job did, in the order they ran.
#[derive(Default)]
struct Summary {
    passed: usize,
    failed: Vec<String>,
    skipped: Vec<(String, String)>,
}

impl Summary {
    fn run(&mut self, sh: &Shell, job: Job, host: Host, dry_run: bool) -> Result<()> {
        let plan = job.plan(sh, host)?;
        println!();
        println!("==> {}", plan.label);
        for note in &plan.notes {
            println!("    {note}");
        }

        match plan.action {
            Action::Skip(reason) => {
                println!("SKIP  {} — {reason}", plan.label);
                self.skipped.push((plan.label, reason));
            }
            Action::Run(steps) => {
                for step in &steps {
                    println!("    {step}");
                }
                if dry_run {
                    println!("PASS  {} (dry-run)", plan.label);
                    self.passed += 1;
                    return Ok(());
                }
                // Every step runs even after one fails: `shell` reports
                // shellcheck's findings and shfmt's diff in the same pass.
                let mut passed = true;
                for step in &steps {
                    passed &= step.run(sh).is_ok();
                }
                if passed {
                    println!("PASS  {}", plan.label);
                    self.passed += 1;
                } else {
                    println!("FAIL  {}", plan.label);
                    self.failed.push(plan.label);
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        println!();
        println!(
            "---- {} passed, {} failed, {} skipped ----",
            self.passed,
            self.failed.len(),
            self.skipped.len()
        );
        if !self.skipped.is_empty() {
            let names: Vec<&str> = self.skipped.iter().map(|(name, _)| name.as_str()).collect();
            println!("Skipped: {}", names.join(", "));
            println!("A skipped job is not a pass. Name it as not run in the PR Testing section.");
        }
        if !self.failed.is_empty() {
            bail!("failed: {}", self.failed.join(", "));
        }
        Ok(())
    }
}
