use std::path::Path;

use serde_json::json;

use super::{Args, build, render, run};
use crate::support::release_tag::ReleaseTag;

const BASE: &str = "https://updates.example";

fn args(dist: &Path, stage: &Path, tag: &str, include_windows: bool) -> Args {
    Args {
        dist: dist.to_path_buf(),
        stage: stage.to_path_buf(),
        tag: tag.to_owned(),
        base_url: format!("{BASE}/"),
        include_windows,
    }
}

/// Write `name` and, when `signed`, its `.minisig` into `dist`.
fn artifact(dist: &Path, name: &str, signed: bool) {
    fs_err::write(dist.join(name), b"artifact").unwrap();
    if signed {
        fs_err::write(dist.join(format!("{name}.minisig")), b"signature").unwrap();
    }
}

fn names(dist: &Path, tag: &str, include_windows: bool) -> Vec<String> {
    let stage = tempfile::tempdir().unwrap();
    let tag_parsed = ReleaseTag::parse(tag).unwrap();
    build(&args(dist, stage.path(), tag, include_windows), &tag_parsed)
        .unwrap()
        .assets
        .into_iter()
        .map(|asset| asset.name)
        .collect()
}

#[test]
fn every_updater_artifact_needs_its_minisign_signature() {
    let dist = tempfile::tempdir().unwrap();
    artifact(dist.path(), "OpenLogi-v1.2.3-macos-arm64.dmg", false);
    let tag = ReleaseTag::parse("v1.2.3").unwrap();
    let error = build(&args(dist.path(), dist.path(), "v1.2.3", false), &tag).unwrap_err();
    assert!(format!("{error:#}").contains("minisig"), "{error:#}");
}

#[test]
fn the_manifest_lists_each_dmg_with_its_signature_url() {
    let dist = tempfile::tempdir().unwrap();
    artifact(dist.path(), "OpenLogi-v1.2.3-macos-arm64.dmg", true);
    let tag = ReleaseTag::parse("v1.2.3").unwrap();
    let manifest = build(&args(dist.path(), dist.path(), "v1.2.3", false), &tag).unwrap();

    let mut value = serde_json::to_value(&manifest).unwrap();
    let published_at = value
        .as_object_mut()
        .unwrap()
        .remove("published_at")
        .unwrap();
    assert!(
        published_at.as_str().unwrap().ends_with('Z'),
        "{published_at}"
    );
    assert_eq!(
        value,
        json!({
            "schema_version": 1,
            "app_id": openlogi_core::brand::APP_ID,
            "version": "1.2.3",
            "tag": "v1.2.3",
            "channel": "stable",
            "release_url": "https://github.com/AprilNEA/OpenLogi/releases/tag/v1.2.3",
            "assets": [{
                "name": "OpenLogi-v1.2.3-macos-arm64.dmg",
                "url": format!("{BASE}/releases/v1.2.3/OpenLogi-v1.2.3-macos-arm64.dmg"),
                "os": "macos",
                "arch": "arm64",
                "format": "dmg",
                "content_type": "application/x-apple-diskimage",
                "size": 8,
                "sha256": "c7c5c1d70c5dec4416ab6158afd0b223ef40c29b1dc1f97ed9428b94d4cadb1c",
                "signature_url": format!("{BASE}/releases/v1.2.3/OpenLogi-v1.2.3-macos-arm64.dmg.minisig"),
                "minimum_os_version": "13.0",
            }],
        })
    );
}

#[test]
fn windows_artifacts_are_listed_only_when_opted_in() {
    // Off by default: the manifest must never reference Windows objects
    // the release workflow's R2 upload step doesn't ship.
    let dist = tempfile::tempdir().unwrap();
    for name in [
        "OpenLogi-v1.2.3-macos-arm64.dmg",
        "OpenLogi-v1.2.3-windows-x86_64.msi",
        "OpenLogi-v1.2.3-windows-arm64.msi",
        "OpenLogi-v1.2.3-windows-x86_64.zip",
    ] {
        artifact(dist.path(), name, true);
    }
    assert_eq!(
        names(dist.path(), "v1.2.3", false),
        ["OpenLogi-v1.2.3-macos-arm64.dmg"]
    );

    let stage = tempfile::tempdir().unwrap();
    let tag = ReleaseTag::parse("v1.2.3").unwrap();
    let manifest = build(&args(dist.path(), stage.path(), "v1.2.3", true), &tag).unwrap();
    // (name, arch, format, minimum OS) of each Windows entry.
    let windows: Vec<String> = manifest
        .assets
        .iter()
        .filter(|asset| asset.os.as_deref() == Some("windows"))
        .map(|asset| {
            format!(
                "{} {} {} {}",
                asset.name,
                asset.arch.as_deref().unwrap_or("-"),
                asset.format.as_deref().unwrap_or("-"),
                asset.minimum_os_version.as_deref().unwrap_or("-"),
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            "OpenLogi-v1.2.3-windows-arm64.msi arm64 msi 10.0",
            "OpenLogi-v1.2.3-windows-x86_64.msi x86_64 msi 10.0",
            "OpenLogi-v1.2.3-windows-x86_64.zip x86_64 zip 10.0",
        ]
    );
}

#[test]
fn everything_the_in_app_updater_does_not_install_is_left_out() {
    let dist = tempfile::tempdir().unwrap();
    artifact(dist.path(), "OpenLogi-v1.2.3-macos-arm64.dmg", true);
    for name in [
        "openlogi-v1.2.3-linux-amd64.deb",
        "openlogi-v1.2.3-linux-amd64.rpm",
        "OpenLogi-v1.2.3-windows-x86_64.exe",
        "OpenLogi-v1.2.3-macos-universal.dmg",
        "OpenLogi-v1.2.3-macos-arm64.from-0.8.16.12.sparkle.delta",
        "not-an-artifact.zip",
        "SHA256SUMS",
    ] {
        artifact(dist.path(), name, true);
    }
    assert_eq!(
        names(dist.path(), "v1.2.3", true),
        ["OpenLogi-v1.2.3-macos-arm64.dmg"]
    );
}

#[test]
fn a_release_without_a_macos_dmg_has_no_manifest() {
    let dist = tempfile::tempdir().unwrap();
    artifact(dist.path(), "OpenLogi-v1.2.3-windows-x86_64.msi", true);
    let tag = ReleaseTag::parse("v1.2.3").unwrap();
    let error = build(&args(dist.path(), dist.path(), "v1.2.3", true), &tag).unwrap_err();
    assert!(format!("{error:#}").contains("macos"), "{error:#}");
}

#[test]
fn a_beta_manifest_goes_into_its_record_labelled_beta() {
    let dist = tempfile::tempdir().unwrap();
    let stage = tempfile::tempdir().unwrap();
    artifact(
        dist.path(),
        "OpenLogi-v0.8.18-beta.3-macos-x86_64.dmg",
        true,
    );
    run(&args(dist.path(), stage.path(), "v0.8.18-beta.3", false)).unwrap();

    let path = stage
        .path()
        .join("releases/v0.8.18-beta.3/feed/latest.json");
    let text = fs_err::read_to_string(&path).unwrap();
    let manifest = gpui_updater_release::read_manifest(&path).unwrap();
    assert_eq!(manifest.version, "0.8.18-beta.3");
    assert_eq!(manifest.tag.as_deref(), Some("v0.8.18-beta.3"));
    assert_eq!(manifest.channel.as_deref(), Some("beta"));
    assert_eq!(
        render(&manifest).unwrap(),
        text,
        "`release channels` re-serves a record byte for byte"
    );
}
