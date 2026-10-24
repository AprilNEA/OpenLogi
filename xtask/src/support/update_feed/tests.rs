use super::{MacArch, appcast_url, record_dir, release_dir, release_page_url, release_url};
use crate::support::release_tag::ReleaseTag;

#[test]
fn a_release_keeps_its_artifacts_and_its_feed_record_in_its_own_directory() {
    let tag = ReleaseTag::parse("v0.8.18-beta.3").unwrap();
    assert_eq!(release_dir(&tag), "releases/v0.8.18-beta.3");
    assert_eq!(record_dir(&tag), "releases/v0.8.18-beta.3/feed");
    assert_eq!(
        release_url("https://updates.openlogi.org//", &tag),
        "https://updates.openlogi.org/releases/v0.8.18-beta.3"
    );
    assert_eq!(
        release_page_url(&tag),
        "https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.18-beta.3"
    );
}

#[test]
fn each_architecture_reads_its_own_feed_on_the_channel() {
    assert_eq!(
        appcast_url("https://updates.openlogi.org/", MacArch::Arm64),
        "https://updates.openlogi.org/channels/stable/appcast-arm64.xml"
    );
    assert_eq!(
        appcast_url("https://mirror.example.org", MacArch::X86_64),
        "https://mirror.example.org/channels/stable/appcast-x86_64.xml"
    );
}

#[test]
fn release_dmg_names_carry_their_architecture() {
    for (name, arch) in [
        ("OpenLogi-v0.8.14-macos-arm64.dmg", Some(MacArch::Arm64)),
        ("OpenLogi-v0.8.14-macos-x86_64.dmg", Some(MacArch::X86_64)),
        ("OpenLogi-v0.8.14-macos-universal.dmg", None),
        ("OpenLogi-v0.8.14-macos-arm64.dmg.minisig", None),
        ("OpenLogi-v0.8.14-windows-x86_64.msi", None),
        ("OpenLogi.dmg", None),
    ] {
        assert_eq!(MacArch::of_dmg(name), arch, "{name}");
    }
}
