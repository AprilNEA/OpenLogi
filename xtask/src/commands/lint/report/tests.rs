use super::{LaneReport, LintReport, Outcome, SiteReport, Verdict, merge, render};

fn lane(name: &str, rows: &[(&str, usize, &str, Verdict)]) -> LaneReport {
    LaneReport {
        lane: name.to_owned(),
        rustc: "rustc 1.99.1".to_owned(),
        sites: rows
            .iter()
            .map(|&(file, line, lint, verdict)| SiteReport {
                file: file.to_owned(),
                line,
                column: 1,
                lints: vec![LintReport {
                    lint: lint.to_owned(),
                    verdict,
                }],
            })
            .collect(),
    }
}

#[test]
fn one_firing_host_keeps_the_allow() {
    let rows = merge(&[
        lane("linux", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
        lane("windows", &[("a.rs", 3, "dead_code", Verdict::Fires)]),
    ]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Outcome::Needed);
}

#[test]
fn silent_on_every_host_is_stale() {
    let rows = merge(&[
        lane("linux", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
        lane("macos", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
    ]);
    assert_eq!(rows[0].outcome, Outcome::Stale);
}

#[test]
fn a_missing_or_unsure_host_is_inconclusive() {
    let missing = merge(&[
        lane("linux", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
        lane("macos", &[]),
    ]);
    assert_eq!(missing[0].verdicts, [Some(Verdict::Silent), None]);
    assert_eq!(missing[0].outcome, Outcome::Inconclusive);

    let unsure = merge(&[
        lane("linux", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
        lane("macos", &[("a.rs", 3, "dead_code", Verdict::Inconclusive)]),
    ]);
    assert_eq!(unsure[0].outcome, Outcome::Inconclusive);
}

#[test]
fn each_lint_of_a_site_is_judged_on_its_own() {
    let mut report = lane("linux", &[("a.rs", 3, "dead_code", Verdict::Fires)]);
    report.sites[0].lints.push(LintReport {
        lint: "clippy::float_cmp".to_owned(),
        verdict: Verdict::Silent,
    });
    let rows = merge(&[report]);
    let outcomes: Vec<(&str, Outcome)> = rows
        .iter()
        .map(|row| (row.lint.as_str(), row.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("clippy::float_cmp", Outcome::Stale),
            ("dead_code", Outcome::Needed)
        ]
    );
}

#[test]
fn two_attributes_on_one_line_stay_two_rows() {
    // `#[allow(dead_code)] #[allow(dead_code)] fn f() {}`: the first fires,
    // the second is redundant. Neither result may overwrite the other.
    let mut report = lane("linux", &[("a.rs", 3, "dead_code", Verdict::Fires)]);
    report.sites.push(SiteReport {
        file: "a.rs".to_owned(),
        line: 3,
        column: 21,
        lints: vec![LintReport {
            lint: "dead_code".to_owned(),
            verdict: Verdict::Silent,
        }],
    });
    let rows = merge(&[report]);
    let outcomes: Vec<(usize, Outcome)> =
        rows.iter().map(|row| (row.column, row.outcome)).collect();
    assert_eq!(outcomes, [(1, Outcome::Needed), (21, Outcome::Stale)]);
}

#[test]
fn the_table_has_a_column_per_host() {
    let reports = [
        lane("linux", &[("a.rs", 3, "dead_code", Verdict::Silent)]),
        lane("windows", &[]),
    ];
    let table = render(&["linux", "windows"], &merge(&reports));
    assert!(
        table.starts_with(
            "| Site | Lint | linux | windows | Result |\n| --- | --- | --- | --- | --- |\n"
        ),
        "{table}"
    );
    assert!(
        table.contains("| `a.rs:3:1` | `dead_code` | silent | missing | **inconclusive** |"),
        "{table}"
    );
}

#[test]
fn reports_round_trip_through_json() {
    let report = lane("linux", &[("a.rs", 3, "dead_code", Verdict::Fires)]);
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains(r#""verdict":"fires""#), "{json}");
    let back: LaneReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.sites[0].lints[0].verdict, Verdict::Fires);
}
