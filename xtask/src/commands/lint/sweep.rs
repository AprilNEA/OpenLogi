//! Runs clippy once per `allow` site, with that site's `allow` removed.
//!
//! The tree is clippy-clean, so the baseline run raises none of the lints the
//! `allow`s name. A lint whose count rises once its `allow` is gone fires on
//! this host. Counting is by diagnostic code from `--message-format=json`, so
//! `-D warnings` in `RUSTFLAGS` (as CI sets it) changes nothing: a lint raised
//! as an error still carries its own code. `--keep-going` matters under that
//! flag — without it Cargo starts no new unit after the first failing one, and
//! a lint that fires only in a crate's test build could go unseen.
//!
//! A run whose build failed for any reason other than lints raised as errors
//! is broken, and none of its counts are used. A compile error shows up as a
//! diagnostic. A failed build script or download shows up only on stderr, so
//! the sweep reads Cargo's closing `build-finished` message instead of the
//! exit status; that keeps stderr streaming to the terminal.
//!
//! Each edit is written in place and put back by a guard, so the sweep refuses
//! to start on a file with uncommitted changes. If the process is killed
//! mid-run, `git checkout -- <file>` restores it.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use xshell::{Shell, cmd};

use super::allow_sites;
use super::report::{self, LaneReport, LintReport, SiteReport, Verdict};
use crate::support::fs::repo_root;

/// Lint groups. Clippy reports each member lint under its own name, so a
/// group's count never moves, and the sweep cannot judge an `allow` of one.
const LINT_GROUPS: &[&str] = &[
    "warnings",
    "unused",
    "nonstandard_style",
    "future_incompatible",
    "let_underscore",
    "rust_2018_idioms",
    "rust_2021_compatibility",
    "rust_2024_compatibility",
    "clippy::all",
    "clippy::cargo",
    "clippy::complexity",
    "clippy::correctness",
    "clippy::nursery",
    "clippy::pedantic",
    "clippy::perf",
    "clippy::restriction",
    "clippy::style",
    "clippy::suspicious",
];

pub(crate) fn run(out: &Path) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    sh.change_dir(&root);

    let mut targets = Vec::new();
    for file in cmd!(sh, "git ls-files -- crates/*.rs xtask/*.rs")
        .read()?
        .lines()
    {
        let source = fs_err::read_to_string(root.join(file))?;
        let sites = allow_sites::find(&source);
        if !sites.is_empty() {
            targets.push((file.to_owned(), source, sites));
        }
    }
    if targets.is_empty() {
        bail!("no `allow` attribute under crates/ or xtask/ — nothing to sweep");
    }
    let files: Vec<&str> = targets.iter().map(|(file, ..)| file.as_str()).collect();
    let dirty = cmd!(sh, "git status --porcelain -- {files...}").read()?;
    if !dirty.trim().is_empty() {
        bail!("the sweep edits these files in place; commit or stash them first:\n{dirty}");
    }

    let lane = rustc_host(&sh)?;
    let rustc = cmd!(sh, "rustc -V").read()?;
    eprintln!("==> baseline clippy on {lane}");
    let baseline = clippy(&sh)?;
    if baseline.broken() {
        bail!("the unedited tree does not build under clippy; fix that before sweeping");
    }

    let mut sites = Vec::new();
    for (file, source, found) in &targets {
        let path = root.join(file);
        for site in found {
            eprintln!(
                "==> {file}:{} without allow({})",
                site.line,
                site.lints.join(", ")
            );
            let run = {
                let _restore = Restore::write(&path, source, &site.apply(source))?;
                clippy(&sh)?
            };
            sites.push(SiteReport {
                file: file.clone(),
                line: site.line,
                column: site.column,
                lints: judge(&site.lints, &baseline, &run),
            });
        }
    }

    let report = LaneReport { lane, rustc, sites };
    if let Some(parent) = out.parent() {
        fs_err::create_dir_all(parent)?;
    }
    fs_err::write(out, serde_json::to_string_pretty(&report)?)?;
    println!(
        "{}",
        report::render(
            &[&report.lane],
            &report::merge(std::slice::from_ref(&report))
        )
    );
    println!(
        "`stale` here means silent on this host only. Run `cargo xtask lint merge-allows` \
         over every host's report before removing anything."
    );
    eprintln!("wrote {}", out.display());
    Ok(())
}

fn rustc_host(sh: &Shell) -> Result<String> {
    let verbose = cmd!(sh, "rustc -vV").read()?;
    verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("`rustc -vV` printed no host line")
}

fn clippy(sh: &Shell) -> Result<Counts> {
    let output = cmd!(
        sh,
        "cargo clippy --workspace --all-targets --keep-going --message-format=json"
    )
    .ignore_status()
    .read()?;
    Ok(Counts::from_cargo_json(&output))
}

fn verdict(lint: &str, baseline: &Counts, run: &Counts) -> Verdict {
    if run.broken() || LINT_GROUPS.contains(&lint) {
        Verdict::Inconclusive
    } else if run.of(lint) > baseline.of(lint) {
        Verdict::Fires
    } else {
        Verdict::Silent
    }
}

/// One verdict per distinct lint of a site. A lint the attribute names more
/// than once is inconclusive: the edit removes every `allow` in the
/// attribute, so a needed one would hide a redundant one.
fn judge(lints: &[String], baseline: &Counts, run: &Counts) -> Vec<LintReport> {
    let mut reports: Vec<LintReport> = Vec::new();
    for lint in lints {
        if let Some(seen) = reports.iter_mut().find(|report| report.lint == *lint) {
            seen.verdict = Verdict::Inconclusive;
        } else {
            reports.push(LintReport {
                lint: lint.clone(),
                verdict: verdict(lint, baseline, run),
            });
        }
    }
    reports
}

/// Diagnostic counts from one clippy run.
#[derive(Debug, Default)]
struct Counts {
    by_code: HashMap<String, usize>,
    /// Errors that are not lints: the edit broke the build, so no lint count
    /// from this run can be trusted.
    hard_errors: usize,
    /// Lints raised as errors, by `-D warnings` or a `deny` level. They are
    /// what a failed build is allowed to fail on.
    lint_errors: usize,
    /// `success` from Cargo's `build-finished` message; `None` when Cargo
    /// stopped before printing one.
    finished: Option<bool>,
}

impl Counts {
    fn from_cargo_json(output: &str) -> Self {
        let mut counts = Self::default();
        for line in output.lines() {
            let Ok(message) = serde_json::from_str::<CargoMessage>(line) else {
                continue;
            };
            if message.reason == "build-finished" {
                counts.finished = message.success;
                continue;
            }
            let (true, Some(diagnostic)) = (message.reason == "compiler-message", message.message)
            else {
                continue;
            };
            let is_error = diagnostic.level.starts_with("error");
            match diagnostic.code {
                Some(code) => {
                    if is_error {
                        if is_error_code(&code.code) {
                            counts.hard_errors += 1;
                        } else {
                            counts.lint_errors += 1;
                        }
                    }
                    *counts.by_code.entry(code.code).or_default() += 1;
                }
                // A parse error has no code; "aborting due to N previous
                // errors" only summarises errors already counted.
                None if is_error && !diagnostic.message.starts_with("aborting due to") => {
                    counts.hard_errors += 1;
                }
                None => {}
            }
        }
        counts
    }

    /// The run cannot be trusted: a compile error, or a failed build that no
    /// lint raised as an error explains (a build script, a download, a lock).
    fn broken(&self) -> bool {
        self.hard_errors > 0 || (self.finished != Some(true) && self.lint_errors == 0)
    }

    fn of(&self, lint: &str) -> usize {
        self.by_code.get(lint).copied().unwrap_or(0)
    }
}

/// `E0425` and friends: compile errors, as opposed to lints raised as errors.
fn is_error_code(code: &str) -> bool {
    code.len() == 5 && code.starts_with('E') && code[1..].bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Deserialize)]
struct CargoMessage {
    reason: String,
    message: Option<Diagnostic>,
    /// Set on `build-finished` only.
    success: Option<bool>,
}

#[derive(Deserialize)]
struct Diagnostic {
    level: String,
    message: String,
    code: Option<DiagnosticCode>,
}

#[derive(Deserialize)]
struct DiagnosticCode {
    code: String,
}

/// Writes an edited copy of a file and puts the original back on drop, so a
/// failed clippy run or an early `?` cannot leave the tree edited.
struct Restore<'a> {
    path: &'a Path,
    original: &'a str,
}

impl<'a> Restore<'a> {
    fn write(path: &'a Path, original: &'a str, edited: &str) -> Result<Self> {
        fs_err::write(path, edited)?;
        Ok(Self { path, original })
    }
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Err(error) = fs_err::write(self.path, self.original) {
            eprintln!(
                "could not restore {}: {error}. `git checkout -- {}` puts it back.",
                self.path.display(),
                self.path.display()
            );
        }
    }
}
