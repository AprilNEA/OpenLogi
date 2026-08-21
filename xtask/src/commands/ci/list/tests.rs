use super::super::jobs::Job;
use super::{JOB_NAMES_HELP, WIDTH, render};

/// A job the table drops is a job a reader concludes CI does not have.
#[test]
fn every_job_has_a_row() {
    let listing = render();
    for job in Job::default_run().chain(Job::focused()) {
        assert!(listing.contains(job.name()), "{job:?} has no row in --list");
    }
}

/// The fixed width is the point of setting one: `--list` output that is
/// the same in a terminal, an issue and a PR body.
#[test]
fn the_table_stays_within_its_width() {
    for line in render().lines() {
        // The footer holds commands that are quoted verbatim; only the
        // generated tables are bound by the width.
        if line.starts_with("  ") || line.contains("nix fmt") {
            continue;
        }
        assert!(
            line.chars().count() <= usize::from(WIDTH),
            "line is {} columns: {line}",
            line.chars().count()
        );
    }
}

/// The help is prose, so it can only be trusted if every name it hands the
/// reader is a name the runner accepts.
#[test]
fn every_name_the_help_advertises_resolves() {
    let names: Vec<&str> = JOB_NAMES_HELP
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Jobs:")
                .or_else(|| line.strip_prefix("Also:"))
        })
        .flat_map(str::split_whitespace)
        .collect();
    assert!(!names.is_empty(), "the help lists no job names");
    for name in names {
        assert!(Job::resolve(name).is_some(), "the help advertises {name}");
    }
}
