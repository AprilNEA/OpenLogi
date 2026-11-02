use super::{AllowSite, find};

fn only(source: &str) -> AllowSite {
    let mut sites = find(source);
    assert_eq!(
        sites.len(),
        1,
        "expected one site in {source:?}, found {sites:?}"
    );
    sites.remove(0)
}

#[test]
fn a_plain_allow_is_removed_whole() {
    let source = "#[expect(clippy::allow_attributes, reason = \"x\")]\n\
                  #[allow(dead_code, reason = \"y\")]\n\
                  fn f() {}\n";
    let site = only(source);
    assert_eq!(site.line, 2);
    assert_eq!(site.lints, ["dead_code"]);
    assert_eq!(
        site.apply(source),
        "#[expect(clippy::allow_attributes, reason = \"x\")]\n\nfn f() {}\n"
    );
}

#[test]
fn a_multi_line_allow_names_every_lint() {
    // The `]` inside the reason string must not close the attribute.
    let source = "#[allow(\n    clippy::double_must_use,\n    dead_code,\n    \
                  reason = \"async_trait ] expansion\"\n)]\nfn f() {}\n";
    let site = only(source);
    assert_eq!(site.line, 1);
    assert_eq!(site.lints, ["clippy::double_must_use", "dead_code"]);
    assert_eq!(site.apply(source), "\nfn f() {}\n");
}

#[test]
fn a_cfg_attr_keeps_everything_but_the_allow() {
    let source = "#[cfg_attr(\n    target_os = \"windows\",\n    \
                  expect(clippy::allow_attributes, reason = \"see above\"),\n    \
                  allow(dead_code, reason = \"z\")\n)]\nfn f() {}\n";
    let site = only(source);
    assert_eq!(site.lints, ["dead_code"]);
    let edited = site.apply(source);
    assert!(find(&edited).is_empty(), "an allow survived: {edited}");
    assert!(edited.contains("target_os = \"windows\""), "{edited}");
    assert!(
        edited.contains("allow_attributes"),
        "the expect went too: {edited}"
    );
    assert!(edited.ends_with(")]\nfn f() {}\n"), "{edited}");
}

#[test]
fn a_cfg_attr_of_only_allows_is_removed_whole() {
    let source = "#[cfg_attr(test, allow(clippy::float_cmp, reason = \"x\"))]\nfn f() {}\n";
    let site = only(source);
    assert_eq!(site.lints, ["clippy::float_cmp"]);
    assert_eq!(site.apply(source), "\nfn f() {}\n");
}

#[test]
fn a_nested_cfg_attr_allow_is_a_site() {
    let source = "#[cfg_attr(unix, cfg_attr(test, allow(dead_code, reason = \"x\")))]\nfn f() {}\n";
    let site = only(source);
    assert_eq!(site.lints, ["dead_code"]);
    assert_eq!(site.apply(source), "\nfn f() {}\n");
}

#[test]
fn a_nested_cfg_attr_keeps_its_other_metas() {
    let source = "#[cfg_attr(unix, inline, cfg_attr(test, allow(dead_code), cold))]\nfn f() {}\n";
    let site = only(source);
    assert_eq!(site.lints, ["dead_code"]);
    let edited = site.apply(source);
    assert!(find(&edited).is_empty(), "an allow survived: {edited}");
    assert_eq!(
        edited,
        "#[cfg_attr(unix, inline, cfg_attr(test, cold))]\nfn f() {}\n"
    );
}

#[test]
fn two_attributes_on_one_line_are_two_sites() {
    let source = "fn g() {}\n    #[allow(dead_code)] #[allow(dead_code)] fn f() {}\n";
    let at: Vec<(usize, usize)> = find(source)
        .iter()
        .map(|site| (site.line, site.column))
        .collect();
    assert_eq!(at, [(2, 5), (2, 25)]);
}

#[test]
fn other_attributes_are_not_sites() {
    let source = r#"
#[expect(dead_code, reason = "allow(x) in a reason")]
#[cfg_attr(test, expect(clippy::float_cmp, reason = "x"))]
#[cfg_attr(feature = "allow-x", derive(Debug))]
#[cfg_attr(unix, cfg_attr(test, expect(dead_code, reason = "allow(x)")))]
#[doc = "allow(x)"]
#![allow(dead_code)]
fn f() {}
"#;
    assert!(find(source).is_empty(), "{:?}", find(source));
}

#[test]
fn allow_text_outside_code_is_not_a_site() {
    let source = r##"
// #[allow(dead_code)]
/* outer /* nested #[allow(dead_code)] */ still a comment #[allow(x)] */
/// #[allow(dead_code)]
const A: &str = "#[allow(dead_code)] \" [";
const B: &str = r#"#[allow(dead_code)] "quoted" ["#;
const C: char = '[';
const D: char = '\'';
fn f<'a>(x: &'a str) -> &'a str { x }
#[allow(unused, reason = "the one real site")]
fn g() {}
"##;
    let site = only(source);
    assert_eq!(site.line, 10);
    assert_eq!(site.lints, ["unused"]);
}
