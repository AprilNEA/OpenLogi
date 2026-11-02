//! Per-host sweep reports, and the verdict across hosts.
//!
//! A lint is stale only when every host ran and none of them raised it. One
//! host's `silent` proves nothing on its own: `ServiceStatus` and
//! `hid_usage_to_windows` are dead code on some hosts and live on others.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// What one host's sweep found.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LaneReport {
    /// The host triple clippy compiled for, from `rustc -vV`.
    pub(crate) lane: String,
    /// `rustc -V` for that run.
    pub(crate) rustc: String,
    pub(crate) sites: Vec<SiteReport>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SiteReport {
    /// Repository-relative path.
    pub(crate) file: String,
    pub(crate) line: usize,
    /// 1-based column of the attribute's `#`: two attributes can share a line.
    pub(crate) column: usize,
    pub(crate) lints: Vec<LintReport>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LintReport {
    pub(crate) lint: String,
    pub(crate) verdict: Verdict,
}

/// One lint on one host, with its `allow` removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// Clippy raised it, so this host needs the `allow`.
    Fires,
    /// Clippy did not raise it on this host.
    Silent,
    /// The edited tree did not build, the name is a lint group, or the
    /// attribute names the lint more than once.
    Inconclusive,
}

/// One lint across every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// At least one host raises it.
    Needed,
    /// Every host ran and none raises it: remove it from the `allow`.
    Stale,
    /// A host has no verdict for it, or could not tell.
    Inconclusive,
}

#[derive(Debug)]
pub(crate) struct Row {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) lint: String,
    /// One entry per report, in argument order; `None` when that report has
    /// no such site.
    pub(crate) verdicts: Vec<Option<Verdict>>,
    pub(crate) outcome: Outcome,
}

/// Join `reports` into one row per (site, lint).
pub(crate) fn merge(reports: &[LaneReport]) -> Vec<Row> {
    type Key = (String, usize, usize, String);
    let mut table: BTreeMap<Key, Vec<Option<Verdict>>> = BTreeMap::new();
    for (index, report) in reports.iter().enumerate() {
        for site in &report.sites {
            for lint in &site.lints {
                let verdicts = table
                    .entry((site.file.clone(), site.line, site.column, lint.lint.clone()))
                    .or_insert_with(|| vec![None; reports.len()]);
                verdicts[index] = Some(lint.verdict);
            }
        }
    }
    table
        .into_iter()
        .map(|((file, line, column, lint), verdicts)| Row {
            outcome: outcome(&verdicts),
            file,
            line,
            column,
            lint,
            verdicts,
        })
        .collect()
}

fn outcome(verdicts: &[Option<Verdict>]) -> Outcome {
    if verdicts.contains(&Some(Verdict::Fires)) {
        Outcome::Needed
    } else if verdicts
        .iter()
        .all(|verdict| *verdict == Some(Verdict::Silent))
    {
        Outcome::Stale
    } else {
        Outcome::Inconclusive
    }
}

/// A Markdown table: one column per host, then the cross-host outcome.
pub(crate) fn render(lanes: &[&str], rows: &[Row]) -> String {
    let mut table = String::from("| Site | Lint |");
    for lane in lanes {
        let _ = write!(table, " {lane} |");
    }
    table.push_str(" Result |\n| --- | --- |");
    table.push_str(&" --- |".repeat(lanes.len() + 1));
    table.push('\n');
    for row in rows {
        let _ = write!(
            table,
            "| `{}:{}:{}` | `{}` |",
            row.file, row.line, row.column, row.lint
        );
        for verdict in &row.verdicts {
            let _ = write!(table, " {} |", verdict.map_or("missing", Verdict::label));
        }
        let _ = writeln!(table, " {} |", row.outcome.label());
    }
    table
}

impl Verdict {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Fires => "fires",
            Self::Silent => "silent",
            Self::Inconclusive => "inconclusive",
        }
    }
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Needed => "needed",
            Self::Stale => "**stale**",
            Self::Inconclusive => "**inconclusive**",
        }
    }
}

pub(crate) fn run_merge(paths: &[PathBuf]) -> Result<()> {
    let reports = paths
        .iter()
        .map(|path| {
            let text = fs_err::read_to_string(path)?;
            serde_json::from_str::<LaneReport>(&text)
                .with_context(|| format!("{} is not a sweep report", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let lanes: Vec<&str> = reports.iter().map(|report| report.lane.as_str()).collect();
    let rows = merge(&reports);
    println!("{}", render(&lanes, &rows));
    let stale = rows
        .iter()
        .filter(|row| row.outcome == Outcome::Stale)
        .count();
    let unsure = rows
        .iter()
        .filter(|row| row.outcome == Outcome::Inconclusive)
        .count();
    if stale + unsure > 0 {
        bail!(
            "{stale} stale and {unsure} inconclusive `allow` lint(s) across {} host(s)",
            reports.len()
        );
    }
    println!(
        "Every `allow` lint fires on at least one of {} host(s).",
        reports.len()
    );
    Ok(())
}
