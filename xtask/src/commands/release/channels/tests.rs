use std::path::Path;

use gpui_updater_release::{Appcast, AppcastItem, Enclosure};
use openlogi_core::update_feed::{MANIFEST_FILE_NAME, UpdateChannel};
use strum::VariantArray as _;

use super::{Args, Outcome, feed_items, write_channels};
use crate::commands::release::latest_json::Manifest;
use crate::support::update_feed::{MacArch, appcast_url};

const BASE: &str = "https://updates.example";

/// The manifest `release latest-json` writes for `tag`, labelled `channel`.
fn manifest(tag: &str, channel: &str) -> String {
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "app_id": "com.openlogi.app",
        "version": tag.trim_start_matches('v'),
        "tag": tag,
        "channel": channel,
        "published_at": "2026-10-23T08:00:00Z",
        "release_url": format!("https://github.com/AprilNEA/OpenLogi/releases/tag/{tag}"),
        "assets": [{
            "name": format!("OpenLogi-{tag}-macos-arm64.dmg"),
            "url": format!("{BASE}/releases/{tag}/OpenLogi-{tag}-macos-arm64.dmg"),
            "signature_url": format!("{BASE}/releases/{tag}/OpenLogi-{tag}-macos-arm64.dmg.minisig"),
            "os": "macos",
            "arch": "arm64",
            "format": "dmg",
            "content_type": "application/x-apple-diskimage",
            "size": 1234,
            "sha256": "00",
            "minimum_os_version": "13.0",
        }],
    }))
    .unwrap();
    manifest.render().unwrap()
}

fn is_beta(tag: &str) -> bool {
    tag.contains("-beta.")
}

/// `tag`'s item for `arch`, as `release appcast` writes it.
fn item(tag: &str, build: &str, arch: MacArch) -> AppcastItem {
    let version = tag.trim_start_matches('v');
    let dmg = format!(
        "{BASE}/releases/{tag}/OpenLogi-{tag}-macos-{}.dmg",
        arch.as_str()
    );
    let mut enclosure = Enclosure::new(dmg, 1234, "application/x-apple-diskimage");
    enclosure.ed_signature = Some(format!("signature of {tag} for {}", arch.as_str()));
    let item = AppcastItem::new(build, enclosure)
        .title(format!("OpenLogi {version}"))
        .short_version(version);
    if is_beta(tag) {
        item.channel("beta")
    } else {
        item
    }
}

/// A feed at `link` offering `items`, newest first.
fn appcast(link: &str, description: &str, items: Vec<AppcastItem>) -> String {
    items
        .into_iter()
        .fold(
            Appcast::new("OpenLogi", link).description(description),
            Appcast::item,
        )
        .render()
}

fn write(path: &Path, contents: &str) {
    fs_err::create_dir_all(path.parent().unwrap()).unwrap();
    fs_err::write(path, contents).unwrap();
}

/// Upload `tag`'s record, built as `build`, to the host `mirror` copies.
fn record(mirror: &Path, tag: &str, build: &str) {
    let dir = mirror.join("releases").join(tag).join("feed");
    let channel = if is_beta(tag) { "beta" } else { "stable" };
    write(&dir.join(MANIFEST_FILE_NAME), &manifest(tag, channel));
    for arch in MacArch::VARIANTS {
        write(
            &dir.join(arch.appcast_file_name()),
            &appcast(
                &format!("{BASE}/releases/{tag}/feed/{}", arch.appcast_file_name()),
                "a record",
                vec![item(tag, build, *arch)],
            ),
        );
    }
}

/// The feed the stable channel serves when it offers `items` (tag, build),
/// newest first, as the publish job wrote it before records.
fn stable_feed(arch: MacArch, items: &[(&str, &str)]) -> String {
    appcast(
        &appcast_url(BASE, arch),
        "OpenLogi updates (stable channel)",
        items
            .iter()
            .map(|(tag, build)| item(tag, build, arch))
            .collect(),
    )
}

/// The stable channel's files on the host before records began: `tag`'s,
/// with `beta` (tag, build) in the feed before it.
fn legacy_stable_channel(mirror: &Path, tag: &str, beta: Option<(&str, &str)>) {
    let dir = mirror.join("channels/stable");
    write(&dir.join(MANIFEST_FILE_NAME), &manifest(tag, "stable"));
    let version = tag.trim_start_matches('v');
    for arch in MacArch::VARIANTS {
        let items: Vec<(&str, &str)> = beta.into_iter().chain([(tag, version)]).collect();
        write(
            &dir.join(arch.appcast_file_name()),
            &stable_feed(*arch, &items),
        );
    }
}

fn args(mirror: &Path, out: &Path, tag: &str) -> Args {
    Args {
        tag: tag.into(),
        mirror: mirror.to_path_buf(),
        output_dir: out.join("stage"),
        base_url: format!("{BASE}/"),
    }
}

fn read(out: &Path, file: &str) -> String {
    fs_err::read_to_string(out.join("stage").join(file)).unwrap()
}

/// The (short version, channel) of each item `out`'s `arch` feed offers.
fn offered(out: &Path, arch: MacArch) -> Vec<(String, Option<String>)> {
    feed_items(&read(
        out,
        &format!("channels/stable/{}", arch.appcast_file_name()),
    ))
    .unwrap()
    .into_iter()
    .map(|item| (item.short_version.unwrap(), item.channel))
    .collect()
}

fn outcome(stable: &str, beta: &str, beta_changed: bool) -> Outcome {
    Outcome {
        stable_tag: stable.into(),
        beta_tag: beta.into(),
        beta_version: beta.trim_start_matches('v').into(),
        beta_changed,
    }
}

#[test]
fn a_stable_release_alone_serves_the_feed_it_always_had() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.18")).unwrap();

    assert_eq!(done, outcome("v0.8.18", "v0.8.18", true));
    for arch in MacArch::VARIANTS {
        assert_eq!(
            read(
                out.path(),
                &format!("channels/stable/{}", arch.appcast_file_name())
            ),
            stable_feed(*arch, &[("v0.8.18", "0.8.18")]),
            "byte for byte the single-item feed installed apps have read since 0.8.14"
        );
    }
    assert_eq!(
        read(out.path(), "channels/stable/latest.json"),
        manifest("v0.8.18", "stable")
    );
    assert_eq!(
        read(out.path(), "channels/beta/latest.json"),
        manifest("v0.8.18", "beta"),
        "the beta channel offers the stable release, labelled beta"
    );
    let mut written: Vec<String> = Vec::new();
    for channel in UpdateChannel::ALL {
        for entry in fs_err::read_dir(out.path().join("stage").join(channel.dir())).unwrap() {
            written.push(format!(
                "{}/{}",
                channel.dir(),
                entry.unwrap().file_name().to_str().unwrap()
            ));
        }
    }
    written.sort();
    assert_eq!(
        written,
        [
            "channels/beta/latest.json",
            "channels/stable/appcast-arm64.xml",
            "channels/stable/appcast-x86_64.xml",
            "channels/stable/latest.json",
        ]
    );
}

#[test]
fn a_beta_newer_than_stable_joins_the_feed_on_its_channel_only() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");
    record(mirror.path(), "v0.8.19-beta.4", "0.8.18.4");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.19-beta.4")).unwrap();

    assert_eq!(done, outcome("v0.8.18", "v0.8.19-beta.4", true));
    for arch in MacArch::VARIANTS {
        assert_eq!(
            read(
                out.path(),
                &format!("channels/stable/{}", arch.appcast_file_name())
            ),
            stable_feed(
                *arch,
                &[("v0.8.19-beta.4", "0.8.18.4"), ("v0.8.18", "0.8.18")]
            )
        );
    }
    assert_eq!(
        read(out.path(), "channels/stable/latest.json"),
        manifest("v0.8.18", "stable"),
        "a beta leaves the stable channel's manifest as it was"
    );
    assert_eq!(
        read(out.path(), "channels/beta/latest.json"),
        manifest("v0.8.19-beta.4", "beta")
    );
}

#[test]
fn a_stable_release_published_after_a_newer_beta_keeps_the_beta() {
    // The beta was tagged after v0.8.18 shipped but its publish job finished
    // first; v0.8.18's own publish runs now.
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.17", "0.8.17");
    record(mirror.path(), "v0.8.18", "0.8.18");
    record(mirror.path(), "v0.8.19-beta.2", "0.8.18.2");
    write(
        &mirror.path().join("channels/beta/latest.json"),
        &manifest("v0.8.19-beta.2", "beta"),
    );

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.18")).unwrap();

    assert_eq!(done, outcome("v0.8.18", "v0.8.19-beta.2", false));
    assert_eq!(
        offered(out.path(), MacArch::Arm64),
        [
            ("0.8.19-beta.2".to_owned(), Some("beta".to_owned())),
            ("0.8.18".to_owned(), None)
        ]
    );
}

#[test]
fn a_beta_older_than_the_newest_stable_release_leaves_the_feeds() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18-beta.3", "0.8.17.3");
    record(mirror.path(), "v0.8.18", "0.8.18");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.18")).unwrap();

    assert_eq!(done, outcome("v0.8.18", "v0.8.18", true));
    assert_eq!(
        offered(out.path(), MacArch::X86_64),
        [("0.8.18".to_owned(), None)]
    );
    assert_eq!(
        read(out.path(), "channels/beta/latest.json"),
        manifest("v0.8.18", "beta")
    );
}

#[test]
fn a_beta_cut_before_the_stable_release_is_older_whatever_its_number() {
    // Tagged 0.9.0-beta.5 while the release PR said 0.9.0; the release PR then
    // went back to 0.8.19, which shipped from a later commit.
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");
    record(mirror.path(), "v0.9.0-beta.5", "0.8.18.5");
    record(mirror.path(), "v0.8.19", "0.8.19");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.19")).unwrap();

    assert_eq!(done, outcome("v0.8.19", "v0.8.19", true));
    assert_eq!(
        offered(out.path(), MacArch::Arm64),
        [("0.8.19".to_owned(), None)]
    );
}

#[test]
fn the_newest_beta_is_the_one_cut_last() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");
    record(mirror.path(), "v0.9.0-beta.3", "0.8.18.3");
    record(mirror.path(), "v0.8.19-beta.10", "0.8.18.10");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.19-beta.10")).unwrap();

    assert_eq!(done.beta_tag, "v0.8.19-beta.10");
    assert_eq!(
        offered(out.path(), MacArch::Arm64),
        [
            ("0.8.19-beta.10".to_owned(), Some("beta".to_owned())),
            ("0.8.18".to_owned(), None)
        ]
    );
}

#[test]
fn the_stable_channel_stands_in_for_a_stable_release_without_a_record() {
    // v0.8.17 shipped before records; a first beta has since been composed in
    // beside it.
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    legacy_stable_channel(
        mirror.path(),
        "v0.8.17",
        Some(("v0.8.18-beta.1", "0.8.17.1")),
    );
    record(mirror.path(), "v0.8.18-beta.1", "0.8.17.1");
    record(mirror.path(), "v0.8.18-beta.6", "0.8.17.6");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.18-beta.6")).unwrap();

    assert_eq!(done, outcome("v0.8.17", "v0.8.18-beta.6", true));
    for arch in MacArch::VARIANTS {
        assert_eq!(
            read(
                out.path(),
                &format!("channels/stable/{}", arch.appcast_file_name())
            ),
            stable_feed(
                *arch,
                &[("v0.8.18-beta.6", "0.8.17.6"), ("v0.8.17", "0.8.17")]
            ),
            "the stand-in's own item, without the beta composed in beside it"
        );
    }
    assert_eq!(
        read(out.path(), "channels/stable/latest.json"),
        manifest("v0.8.17", "stable")
    );
}

#[test]
fn a_stable_record_replaces_the_stand_in() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    legacy_stable_channel(mirror.path(), "v0.8.17", None);
    record(mirror.path(), "v0.8.18", "0.8.18");

    let done = write_channels(&args(mirror.path(), out.path(), "v0.8.18")).unwrap();

    assert_eq!(done.stable_tag, "v0.8.18");
    assert_eq!(
        offered(out.path(), MacArch::Arm64),
        [("0.8.18".to_owned(), None)]
    );
}

fn error(mirror: &Path, tag: &str) -> String {
    let out = tempfile::tempdir().unwrap();
    let error = write_channels(&args(mirror, out.path(), tag)).unwrap_err();
    assert!(
        !out.path().join("stage").exists(),
        "a failed composition writes nothing"
    );
    format!("{error:#}")
}

#[test]
fn the_published_release_needs_its_record() {
    let mirror = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");
    let error = error(mirror.path(), "v0.8.19-beta.1");
    assert!(error.contains("no record of v0.8.19-beta.1"), "{error}");
}

#[test]
fn without_any_stable_release_there_is_nothing_to_offer() {
    let mirror = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.19-beta.1", "0.8.18.1");
    let error = error(mirror.path(), "v0.8.19-beta.1");
    assert!(error.contains("no stable release"), "{error}");
}

/// A way to break the record in a directory.
type Break = fn(&Path);

#[test]
fn a_broken_record_stops_the_composition() {
    let broken: [(&str, Break); 5] = [
        ("No such file", |dir| {
            fs_err::remove_file(dir.join("appcast-x86_64.xml")).unwrap();
        }),
        ("not v0.8.19-beta.1's record", |dir| {
            // A beta item without its channel would reach every stable user.
            let path = dir.join("appcast-arm64.xml");
            let xml = fs_err::read_to_string(&path).unwrap();
            write(
                &path,
                &xml.replace("      <sparkle:channel>beta</sparkle:channel>\n", ""),
            );
        }),
        ("ends in .1", |dir| {
            let path = dir.join("appcast-arm64.xml");
            let xml = fs_err::read_to_string(&path).unwrap();
            write(&path, &xml.replace("0.8.18.1<", "0.8.18.2<"));
        }),
        ("is the manifest of v0.8.18", |dir| {
            write(&dir.join(MANIFEST_FILE_NAME), &manifest("v0.8.18", "beta"));
        }),
        ("holds 0 items", |dir| {
            write(
                &dir.join("appcast-arm64.xml"),
                &appcast("https://x", "empty", Vec::new()),
            );
        }),
    ];
    for (says, breaks) in broken {
        let mirror = tempfile::tempdir().unwrap();
        record(mirror.path(), "v0.8.18", "0.8.18");
        record(mirror.path(), "v0.8.19-beta.1", "0.8.18.1");
        breaks(&mirror.path().join("releases/v0.8.19-beta.1/feed"));
        let error = error(mirror.path(), "v0.8.18");
        assert!(error.contains(says), "{says}: {error}");
    }
}

#[test]
fn nothing_stale_is_uploaded_with_the_channel_files() {
    let mirror = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    record(mirror.path(), "v0.8.18", "0.8.18");
    write(&out.path().join("stage/stale.json"), "{}");
    let error = write_channels(&args(mirror.path(), out.path(), "v0.8.18")).unwrap_err();
    assert!(error.to_string().contains("is not empty"), "{error}");
}
