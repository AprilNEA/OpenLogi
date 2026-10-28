use super::{Counts, is_error_code, judge, verdict};
use crate::commands::lint::report::Verdict;

/// A trimmed `cargo clippy --message-format=json` run: one lint, one
/// unfulfilled expectation, a summary line, and lines that are not
/// diagnostics at all.
const RUN: &str = r#"{"reason":"compiler-artifact","package_id":"x"}
{"reason":"compiler-message","message":{"level":"warning","message":"function `f` is never used","code":{"code":"dead_code","explanation":null}}}
{"reason":"compiler-message","message":{"level":"warning","message":"this lint expectation is unfulfilled","code":{"code":"unfulfilled_lint_expectations","explanation":null}}}
{"reason":"compiler-message","message":{"level":"warning","message":"2 warnings emitted","code":null}}
not json at all
{"reason":"build-finished","success":true}
"#;

/// A run that built and raised nothing.
const CLEAN: &str = r#"{"reason":"build-finished","success":true}"#;

#[test]
fn lints_are_counted_by_code() {
    let counts = Counts::from_cargo_json(RUN);
    assert_eq!(counts.of("dead_code"), 1);
    assert_eq!(counts.of("unfulfilled_lint_expectations"), 1);
    assert_eq!(counts.of("clippy::double_must_use"), 0);
    assert_eq!(counts.hard_errors, 0);
    assert!(!counts.broken());
}

#[test]
fn compile_errors_are_hard_but_lints_raised_as_errors_are_not() {
    let run = r#"{"reason":"compiler-message","message":{"level":"error","message":"cannot find value `x` in this scope","code":{"code":"E0425","explanation":"..."}}}
{"reason":"compiler-message","message":{"level":"error","message":"function `f` is never used","code":{"code":"dead_code","explanation":null}}}
{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 2 previous errors","code":null}}
"#;
    let counts = Counts::from_cargo_json(run);
    assert_eq!(counts.hard_errors, 1);
    assert_eq!(counts.of("dead_code"), 1);
}

#[test]
fn a_parse_error_without_a_code_is_hard() {
    let run = r#"{"reason":"compiler-message","message":{"level":"error","message":"expected one of `!` or `::`, found `fn`","code":null}}"#;
    assert_eq!(Counts::from_cargo_json(run).hard_errors, 1);
}

#[test]
fn verdicts_compare_against_the_baseline() {
    let clean = Counts::from_cargo_json(CLEAN);
    let run = Counts::from_cargo_json(RUN);
    assert_eq!(verdict("dead_code", &clean, &run), Verdict::Fires);
    assert_eq!(
        verdict("clippy::double_must_use", &clean, &run),
        Verdict::Silent
    );
    // Already raised before the edit: the edit added nothing.
    assert_eq!(verdict("dead_code", &run, &run), Verdict::Silent);
    // A group's members report under their own names.
    assert_eq!(verdict("unused", &clean, &run), Verdict::Inconclusive);
    let broken = Counts::from_cargo_json(
        r#"{"reason":"compiler-message","message":{"level":"error","message":"x","code":{"code":"E0425","explanation":null}}}"#,
    );
    assert_eq!(verdict("dead_code", &clean, &broken), Verdict::Inconclusive);
}

#[test]
fn a_failed_build_with_no_lint_error_is_broken() {
    // A build script failed: Cargo says so on stderr, and the JSON stream
    // only ends unsuccessfully.
    let run = Counts::from_cargo_json(
        r#"{"reason":"compiler-message","message":{"level":"warning","message":"x","code":{"code":"dead_code","explanation":null}}}
{"reason":"build-finished","success":false}"#,
    );
    assert!(run.broken());
    let clean = Counts::from_cargo_json(CLEAN);
    assert_eq!(verdict("dead_code", &clean, &run), Verdict::Inconclusive);
}

#[test]
fn a_run_cargo_never_finished_is_broken() {
    assert!(Counts::from_cargo_json("").broken());
    assert!(Counts::from_cargo_json(r#"{"reason":"compiler-artifact"}"#).broken());
}

#[test]
fn lints_raised_as_errors_explain_a_failed_build() {
    // `-D warnings`: the lint the edit exposed fails the build, as intended.
    let run = Counts::from_cargo_json(
        r#"{"reason":"compiler-message","message":{"level":"error","message":"x","code":{"code":"dead_code","explanation":null}}}
{"reason":"build-finished","success":false}"#,
    );
    assert!(!run.broken());
    let clean = Counts::from_cargo_json(CLEAN);
    assert_eq!(verdict("dead_code", &clean, &run), Verdict::Fires);
}

#[test]
fn only_e_codes_are_compile_errors() {
    assert!(is_error_code("E0425"));
    assert!(!is_error_code("dead_code"));
    assert!(!is_error_code("E042"));
    assert!(!is_error_code("Exxxx"));
}

#[test]
fn a_lint_the_attribute_names_twice_is_inconclusive() {
    // `#[cfg_attr(unix, allow(dead_code), cfg_attr(test, allow(dead_code)))]`:
    // one edit removes both, so a firing `dead_code` cannot tell which one
    // is needed.
    let clean = Counts::from_cargo_json(CLEAN);
    let run = Counts::from_cargo_json(RUN);
    let lints = ["dead_code", "clippy::float_cmp", "dead_code"].map(str::to_owned);
    let reports = judge(&lints, &clean, &run);
    let judged: Vec<(&str, Verdict)> = reports
        .iter()
        .map(|report| (report.lint.as_str(), report.verdict))
        .collect();
    assert_eq!(
        judged,
        [
            ("dead_code", Verdict::Inconclusive),
            ("clippy::float_cmp", Verdict::Silent)
        ]
    );
}
