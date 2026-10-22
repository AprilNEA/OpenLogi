use semver::Version;

use super::{FIRST_SPARKLE_RELEASE, delta_bases};

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|tag| (*tag).to_owned()).collect()
}

#[test]
fn bases_are_the_newest_earlier_sparkle_releases() {
    // `gh release list` order is not relied on, and a pre-release, a tag
    // without Sparkle, the release itself and a later one are all skipped.
    let listed = tags(&[
        "v0.8.15",
        "v0.8.17-beta.4",
        "v0.8.13",
        "v0.9.0-rc.1",
        "v0.8.17",
        "v0.8.10",
        "v0.8.14",
        "v0.8.16",
        "v0.8.18",
    ]);
    let current = Version::new(0, 8, 17);
    assert_eq!(
        delta_bases(&listed, &current, 3),
        ["v0.8.16", "v0.8.15", "v0.8.14"]
    );
    assert_eq!(delta_bases(&listed, &current, 1), ["v0.8.16"]);
    assert_eq!(
        delta_bases(&listed, &current, 10),
        ["v0.8.16", "v0.8.15", "v0.8.14"],
        "nothing older than the first Sparkle release"
    );
    assert!(delta_bases(&listed, &current, 0).is_empty());
}

#[test]
fn the_first_sparkle_release_has_nothing_to_update_from() {
    let listed = tags(&["v0.8.12", "v0.8.13", "v0.8.14"]);
    assert!(delta_bases(&listed, &FIRST_SPARKLE_RELEASE, 3).is_empty());
}
