use super::{FIRST_SPARKLE_RELEASE, Version, delta_bases};

#[test]
fn only_final_release_tags_have_a_version() {
    assert_eq!(Version::of_tag("v0.8.14"), Some(Version([0, 8, 14])));
    assert_eq!(Version::of_tag("v10.0.123"), Some(Version([10, 0, 123])));
    for tag in [
        "0.8.14",
        "v0.9.0-rc.1",
        "v0.8",
        "v0.8.14.1",
        "v0..14",
        "v0.8.x",
        "nightly",
        "",
    ] {
        assert_eq!(Version::of_tag(tag), None, "{tag:?}");
    }
}

#[test]
fn versions_order_numerically_not_as_text() {
    assert!(Version([0, 8, 10]) > Version([0, 8, 9]));
    assert!(Version([0, 10, 0]) > Version([0, 9, 99]));
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|tag| (*tag).to_owned()).collect()
}

#[test]
fn bases_are_the_newest_earlier_sparkle_releases() {
    // `gh release list` order is not relied on, and a pre-release, a tag
    // without Sparkle, the release itself and a later one are all skipped.
    let listed = tags(&[
        "v0.8.15",
        "v0.8.13",
        "v0.9.0-rc.1",
        "v0.8.17",
        "v0.8.10",
        "v0.8.14",
        "v0.8.16",
        "v0.8.18",
    ]);
    let current = Version([0, 8, 17]);
    assert_eq!(
        delta_bases(&listed, current, 3),
        ["v0.8.16", "v0.8.15", "v0.8.14"]
    );
    assert_eq!(delta_bases(&listed, current, 1), ["v0.8.16"]);
    assert_eq!(
        delta_bases(&listed, current, 10),
        ["v0.8.16", "v0.8.15", "v0.8.14"],
        "nothing older than the first Sparkle release"
    );
    assert!(delta_bases(&listed, current, 0).is_empty());
}

#[test]
fn the_first_sparkle_release_has_nothing_to_update_from() {
    let listed = tags(&["v0.8.12", "v0.8.13", "v0.8.14"]);
    assert!(delta_bases(&listed, FIRST_SPARKLE_RELEASE, 3).is_empty());
}
