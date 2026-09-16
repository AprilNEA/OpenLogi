use super::*;

#[test]
fn collect_assets_requires_minisign_signature_for_each_dmg() {
    let dist = tempfile::tempdir().unwrap();
    fs_err::write(dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();

    assert!(
        collect_assets(
            dist.path(),
            "https://updates.example/releases/v1.2.3",
            false
        )
        .is_err()
    );
}

#[test]
fn collect_assets_publishes_signature_url() {
    let dist = tempfile::tempdir().unwrap();
    fs_err::write(dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg"), b"dmg").unwrap();
    fs_err::write(
        dist.path().join("OpenLogi-v1.2.3-macos-arm64.dmg.minisig"),
        b"signature",
    )
    .unwrap();

    let assets = collect_assets(
        dist.path(),
        "https://updates.example/releases/v1.2.3",
        false,
    )
    .unwrap();

    assert_eq!(
        assets[0].signature_url,
        "https://updates.example/releases/v1.2.3/OpenLogi-v1.2.3-macos-arm64.dmg.minisig"
    );
}

#[test]
fn collect_assets_skips_windows_artifacts_unless_opted_in() {
    // Off by default: the manifest must never reference Windows objects
    // the release workflow's R2 upload step doesn't ship.
    let dist = tempfile::tempdir().unwrap();
    for name in [
        "OpenLogi-v1.2.3-windows-x86_64.msi",
        "OpenLogi-v1.2.3-windows-x86_64.zip",
    ] {
        fs_err::write(dist.path().join(name), b"artifact").unwrap();
        fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
    }

    let assets = collect_assets(
        dist.path(),
        "https://updates.example/releases/v1.2.3",
        false,
    )
    .unwrap();

    assert!(assets.is_empty());
}

#[test]
fn collect_assets_includes_windows_msi_and_zip_per_arch() {
    let dist = tempfile::tempdir().unwrap();
    for name in [
        "OpenLogi-v1.2.3-windows-x86_64.msi",
        "OpenLogi-v1.2.3-windows-arm64.msi",
        "OpenLogi-v1.2.3-windows-x86_64.zip",
    ] {
        fs_err::write(dist.path().join(name), b"artifact").unwrap();
        fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
    }

    let assets =
        collect_assets(dist.path(), "https://updates.example/releases/v1.2.3", true).unwrap();

    assert_eq!(assets.len(), 3);
    assert!(assets.iter().all(|a| a.os == "windows"));
    let msi = assets
        .iter()
        .find(|a| a.name.ends_with("x86_64.msi"))
        .unwrap();
    assert_eq!((msi.arch.as_str(), msi.format), ("x86_64", "msi"));
    let zip = assets
        .iter()
        .find(|a| a.name.ends_with("x86_64.zip"))
        .unwrap();
    assert_eq!((zip.arch.as_str(), zip.format), ("x86_64", "zip"));
    assert!(assets.iter().any(|a| a.arch == "arm64"));
}

#[test]
fn collect_assets_skips_linux_packages_and_foreign_archives() {
    let dist = tempfile::tempdir().unwrap();
    for name in [
        "openlogi-v1.2.3-linux-amd64.deb",
        "openlogi-v1.2.3-linux-amd64.rpm",
        "openlogi-v1.2.3-linux-amd64.pkg.tar.zst",
        "not-an-artifact.zip",
        "SHA256SUMS",
    ] {
        fs_err::write(dist.path().join(name), b"artifact").unwrap();
        fs_err::write(dist.path().join(format!("{name}.minisig")), b"signature").unwrap();
    }

    let assets =
        collect_assets(dist.path(), "https://updates.example/releases/v1.2.3", true).unwrap();

    assert!(assets.is_empty());
}
