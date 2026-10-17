use super::{latest_release_tag, release_version, strip_version_section};

#[test]
fn latest_release_tag_ignores_non_release_shapes() {
    let tags = "v0.7.10\nv0.7.4\nv0.7.4-rc.1\nv1\nvnext\n";
    assert_eq!(latest_release_tag(tags).as_deref(), Some("v0.7.10"));
}

#[test]
fn latest_release_tag_orders_numerically_not_lexically() {
    // `sort` without -V puts v0.7.9 last; the changelog range would then
    // start after the newest release and come out empty.
    assert_eq!(
        latest_release_tag("v0.7.9\nv0.7.10\n").as_deref(),
        Some("v0.7.10")
    );
}

#[test]
fn latest_release_tag_is_none_without_a_release_tag() {
    assert_eq!(latest_release_tag("vnext\n"), None);
    assert_eq!(latest_release_tag(""), None);
}

#[test]
fn release_version_rejects_extra_fields() {
    assert_eq!(release_version("v1.2.3"), Some([1, 2, 3]));
    assert_eq!(release_version("v1.2.3.4"), None);
    assert_eq!(release_version("1.2.3"), None);
}

#[test]
fn strip_version_section_removes_only_that_section() {
    let changelog = "\
# Changelog

## [0.8.0](https://example.invalid) - 2026-08-21

### Added

- a thing

## [0.7.4](https://example.invalid) - 2026-08-01

- an older thing
";
    let stripped = strip_version_section(changelog, "0.8.0").expect("0.8.0 section is present");
    assert_eq!(
        stripped,
        "\
# Changelog

## [0.7.4](https://example.invalid) - 2026-08-01

- an older thing
"
    );
}

#[test]
fn strip_version_section_removes_a_trailing_section() {
    let changelog = "# Changelog\n\n## [0.8.0] - 2026-08-21\n\n- only entry\n";
    let stripped = strip_version_section(changelog, "0.8.0").expect("0.8.0 section is present");
    assert_eq!(stripped, "# Changelog\n\n");
}

#[test]
fn strip_version_section_is_none_when_absent() {
    let changelog = "# Changelog\n\n## [0.7.4] - 2026-08-01\n\n- a thing\n";
    assert_eq!(strip_version_section(changelog, "0.8.0"), None);
}

#[test]
fn strip_version_section_does_not_match_a_version_prefix() {
    // `## [0.8.0]` must not be found by a search for `0.8`.
    let changelog = "# Changelog\n\n## [0.8.0] - 2026-08-21\n\n- a thing\n";
    assert_eq!(strip_version_section(changelog, "0.8"), None);
}
