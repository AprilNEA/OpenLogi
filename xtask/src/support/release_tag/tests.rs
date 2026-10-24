use openlogi_core::update_feed::UpdateChannel;
use semver::Version;

use super::{BuildVersion, ReleaseTag};

fn tags(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// `tag`'s build version, with `merged` the tags reachable from it and, for a
/// beta, `commits` the commits from its stable release to it.
fn build_version(tag: &str, merged: &[&str], commits: u64) -> anyhow::Result<String> {
    let tag = ReleaseTag::parse(tag)?;
    let merged = tags(merged);
    let base = tag.beta_base(&merged)?;
    BuildVersion::of(&tag, base.as_ref().map(|base| (base, commits))).map(|build| build.to_string())
}

#[test]
fn a_stable_tag_is_its_version_on_the_stable_channel() {
    let tag = ReleaseTag::parse("v0.8.16").unwrap();
    assert_eq!(tag.channel(), UpdateChannel::Stable);
    assert_eq!(tag.version(), &Version::new(0, 8, 16));
    assert_eq!(tag.to_string(), "v0.8.16");
}

#[test]
fn a_beta_tag_is_its_version_on_the_beta_channel() {
    let tag = ReleaseTag::parse("v0.8.17-beta.12").unwrap();
    assert_eq!(tag.channel(), UpdateChannel::Beta);
    assert_eq!(tag.version().to_string(), "0.8.17-beta.12");
    assert_eq!(tag.to_string(), "v0.8.17-beta.12");
}

#[test]
fn every_other_shape_is_rejected() {
    for tag in [
        "0.8.16",
        "v0.8",
        "v0.8.16.1",
        "v01.8.16",
        "v+0.8.16",
        "v0.8.16+build.1",
        "v0.8.17-beta",
        "v0.8.17-beta.",
        "v0.8.17-beta.0",
        "v0.8.17-beta.01",
        "v0.8.17-beta.1.2",
        "v0.8.17-beta.x",
        "v0.8.17-rc.1",
        "v0.8.17-alpha.1",
        "vnext",
        "",
    ] {
        assert!(ReleaseTag::parse(tag).is_err(), "{tag:?} parsed");
        assert_eq!(ReleaseTag::stable_version(tag), None, "{tag:?}");
    }
}

#[test]
fn stable_version_skips_betas() {
    assert_eq!(
        ReleaseTag::stable_version("v10.0.123"),
        Some(Version::new(10, 0, 123))
    );
    assert_eq!(ReleaseTag::stable_version("v0.8.17-beta.12"), None);
}

#[test]
fn a_stable_release_builds_as_its_version() {
    assert_eq!(build_version("v0.8.16", &[], 0).unwrap(), "0.8.16");
}

#[test]
fn a_beta_builds_as_the_stable_release_before_it_and_its_number() {
    let merged = [
        "v0.8.9",
        "v0.8.15",
        "v0.8.16",
        "v0.8.17-beta.3",
        "v0.8.17-beta.12",
        "vnext",
    ];
    assert_eq!(
        build_version("v0.8.17-beta.12", &merged, 12).unwrap(),
        "0.8.16.12"
    );
    // A beta toward a minor release follows the same stable release.
    assert_eq!(
        build_version("v0.9.0-beta.40", &merged, 40).unwrap(),
        "0.8.16.40"
    );
    let tag = ReleaseTag::parse("v0.8.17-beta.12").unwrap();
    let base = tag.beta_base(&tags(&merged)).unwrap().unwrap();
    assert_eq!(base.tag, "v0.8.16");
}

#[test]
fn a_beta_number_is_the_commit_count_since_its_stable_release() {
    for commits in [11, 13] {
        let error = build_version("v0.8.17-beta.12", &["v0.8.16"], commits).unwrap_err();
        assert!(
            error.to_string().contains("commits lead from v0.8.16"),
            "{error}"
        );
    }
}

#[test]
fn only_a_beta_takes_a_stable_release_before_it() {
    let base_of = |tag: &str| {
        ReleaseTag::parse(tag)
            .unwrap()
            .beta_base(&tags(&["v0.8.16"]))
            .unwrap()
    };
    assert_eq!(base_of("v0.8.17"), None);
    let base = base_of("v0.8.17-beta.1").unwrap();
    let stable = ReleaseTag::parse("v0.8.17").unwrap();
    let error = BuildVersion::of(&stable, Some((&base, 1))).unwrap_err();
    assert!(error.to_string().contains("stable release"), "{error}");
    let beta = ReleaseTag::parse("v0.8.17-beta.1").unwrap();
    let error = BuildVersion::of(&beta, None).unwrap_err();
    assert!(error.to_string().contains("is a beta"), "{error}");
}

#[test]
fn a_beta_tags_the_workspace_version_of_the_stable_release_it_follows() {
    let merged = tags(&["v0.8.16"]);
    let stable = ReleaseTag::parse("v0.8.17").unwrap();
    assert_eq!(
        stable.tagged_workspace_version(stable.beta_base(&merged).unwrap().as_ref()),
        &Version::new(0, 8, 17)
    );
    let beta = ReleaseTag::parse("v0.8.17-beta.12").unwrap();
    let base = beta.beta_base(&merged).unwrap();
    assert_eq!(
        beta.tagged_workspace_version(base.as_ref()),
        &Version::new(0, 8, 16)
    );
}

#[test]
fn only_a_beta_build_stamps_its_own_version() {
    let merged = tags(&["v0.8.16"]);
    let stable = ReleaseTag::parse("v0.8.17").unwrap();
    assert_eq!(
        stable.stamp_version(stable.beta_base(&merged).unwrap().as_ref()),
        None
    );
    let beta = ReleaseTag::parse("v0.8.17-beta.12").unwrap();
    let base = beta.beta_base(&merged).unwrap();
    assert_eq!(
        beta.stamp_version(base.as_ref())
            .map(Version::to_string)
            .as_deref(),
        Some("0.8.17-beta.12")
    );
}

#[test]
fn a_beta_needs_a_stable_release_before_it() {
    let error = build_version("v0.1.0-beta.1", &["v0.1.0-beta.1"], 1).unwrap_err();
    assert!(error.to_string().contains("no stable release"), "{error}");
}

#[test]
fn a_beta_must_lead_past_the_stable_release_before_it() {
    // A beta for 0.8.16 cut after 0.8.16 shipped would build below it.
    let error = build_version("v0.8.16-beta.2", &["v0.8.16"], 2).unwrap_err();
    assert!(error.to_string().contains("does not lead past"), "{error}");
}

#[test]
fn build_versions_order_betas_between_the_stable_releases() {
    let parse = |text: &str| -> Vec<u64> {
        text.split('.')
            .map(|field| field.parse().unwrap())
            .collect()
    };
    let stable = parse(&build_version("v0.8.16", &[], 0).unwrap());
    let early = parse(&build_version("v0.8.17-beta.3", &["v0.8.16"], 3).unwrap());
    let late = parse(&build_version("v0.8.17-beta.12", &["v0.8.16"], 12).unwrap());
    let next = parse(&build_version("v0.8.17", &[], 0).unwrap());
    // Sparkle compares the fields left to right and pads a missing one with
    // 0. A beta's fourth field is at least 1, so plain field order holds.
    assert!(stable < early && early < late && late < next);
}

#[test]
fn a_build_version_must_fit_an_msi_product_version() {
    assert_eq!(
        build_version("v255.255.65535", &[], 0).unwrap(),
        "255.255.65535"
    );
    for (tag, merged) in [
        ("v256.0.0", &[][..]),
        ("v0.256.0", &[][..]),
        ("v0.0.65536", &[][..]),
        ("v0.8.17-beta.65536", &["v0.8.16"][..]),
    ] {
        let error = build_version(tag, merged, 65_536).unwrap_err();
        assert!(error.to_string().contains("MSI"), "{tag}: {error}");
    }
}

#[test]
fn a_build_version_parses_back_from_its_text() {
    for text in ["0.8.16", "0.8.16.12", "0.0.0", "255.255.65535.65535"] {
        let build: BuildVersion = text.parse().unwrap();
        assert_eq!(build.to_string(), text);
    }
    let tag = ReleaseTag::parse("v0.8.17-beta.12").unwrap();
    let base = tag.beta_base(&tags(&["v0.8.16"])).unwrap().unwrap();
    let built = BuildVersion::of(&tag, Some((&base, 12))).unwrap();
    assert_eq!(built.to_string().parse::<BuildVersion>().unwrap(), built);
}

#[test]
fn a_build_version_is_only_numeric_fields() {
    for text in [
        "",
        "0.8",
        "0.8.16.12.1",
        "0.8.16-beta.12",
        "0.8.016",
        "0..16",
        "0.8.+16",
        "0.8.16.",
        "0.8.16.0",
        "256.0.0",
        "0.8.16.65536",
        "0.8.99999999999999999999",
    ] {
        assert!(text.parse::<BuildVersion>().is_err(), "{text:?} parsed");
    }
}

#[test]
fn a_stable_release_notes_compare_with_the_stable_release_before_it() {
    let merged = tags(&[
        "v0.8.15",
        "v0.8.16",
        "v0.8.17-beta.3",
        "v0.8.17-beta.12",
        "v0.8.17",
    ]);
    let base = |tag: &str| ReleaseTag::parse(tag).unwrap().notes_base(&merged);
    assert_eq!(
        base("v0.8.17"),
        Some("v0.8.16"),
        "a stable release skips its betas"
    );
    assert_eq!(base("v0.8.16"), Some("v0.8.15"));
    assert_eq!(
        base("v0.8.15"),
        None,
        "the first release has nothing to compare with"
    );
}

#[test]
fn a_beta_notes_compare_with_the_release_just_before_it() {
    let merged = tags(&["v0.8.16", "v0.8.17-beta.3", "v0.8.17-beta.12", "v0.8.17"]);
    let base = |tag: &str| ReleaseTag::parse(tag).unwrap().notes_base(&merged);
    assert_eq!(
        base("v0.8.17-beta.12"),
        Some("v0.8.17-beta.3"),
        "beta numbers compare as numbers, not text"
    );
    assert_eq!(base("v0.8.17-beta.3"), Some("v0.8.16"));
    assert_eq!(base("v0.8.18-beta.2"), Some("v0.8.17"));
}

#[test]
fn release_notes_skip_tags_that_are_not_release_tags() {
    let merged = tags(&[
        "v0.8.16",
        "v0.8.17-rc.1",
        "v0.8.17+build.1",
        "vnext",
        "0.8.17",
    ]);
    let base = ReleaseTag::parse("v0.8.18").unwrap().notes_base(&merged);
    assert_eq!(base, Some("v0.8.16"));
}

#[test]
fn build_versions_order_as_sparkle_compares_them() {
    let ordered: Vec<BuildVersion> = ["0.8.16", "0.8.16.1", "0.8.16.12", "0.8.17", "0.9.0.3"]
        .iter()
        .map(|text| text.parse().unwrap())
        .collect();
    for pair in ordered.windows(2) {
        assert!(pair[0] < pair[1], "{} < {}", pair[0], pair[1]);
    }
}

#[test]
fn a_build_version_fits_only_the_tag_that_built_it() {
    let fits = |build: &str, tag: &str| {
        build
            .parse::<BuildVersion>()
            .unwrap()
            .ensure_fits(&ReleaseTag::parse(tag).unwrap())
    };
    fits("0.8.17", "v0.8.17").unwrap();
    fits("0.8.17.12", "v0.8.18-beta.12").unwrap();
    fits("0.8.16.12", "v0.9.0-beta.12").unwrap();
    for (build, tag, says) in [
        ("0.8.16", "v0.8.17", "its build version is 0.8.17"),
        ("0.8.17.3", "v0.8.17", "its build version is 0.8.17"),
        ("0.8.17", "v0.8.18-beta.12", "ends in .12"),
        ("0.8.17.11", "v0.8.18-beta.12", "ends in .12"),
        (
            "0.8.18.12",
            "v0.8.18-beta.12",
            "starts below 0.8.18-beta.12",
        ),
    ] {
        let error = fits(build, tag).unwrap_err().to_string();
        assert!(error.contains(says), "{build} for {tag}: {error}");
    }
}

#[test]
fn a_beta_tag_is_spelled_from_its_release_and_number() {
    let release = Version::parse("0.8.18").unwrap();
    let tag = ReleaseTag::beta(&release, 12).unwrap();
    assert_eq!(tag.to_string(), "v0.8.18-beta.12");
    assert_eq!(tag, ReleaseTag::parse("v0.8.18-beta.12").unwrap());
    assert_eq!(tag.channel(), UpdateChannel::Beta);
    assert!(
        ReleaseTag::beta(&release, 0).is_err(),
        "beta.0 would equal the release before it"
    );
    for release in ["0.8.18-beta.1", "0.8.18+meta"] {
        let release = Version::parse(release).unwrap();
        let error = ReleaseTag::beta(&release, 1).unwrap_err();
        assert!(
            error.to_string().contains("is not `X.Y.Z`"),
            "{release}: {error}"
        );
    }
}
