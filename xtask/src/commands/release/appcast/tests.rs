use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use gpui_updater_release::Enclosure;

use super::{
    Args, Release, ensure_sparkle_orders, item, record, release_dmgs, signing_key, write_appcasts,
};
use crate::support::update_feed::MacArch;
use openlogi_core::update_feed::UpdateChannel;

/// Golden values made with Sparkle 2.10's own tools (also pinned in
/// gpui-updater-core's verifier tests): the seed `[7; 32]` exported the way
/// `generate_keys -x` writes it, the public key `generate_keys` prints for it,
/// and `sign_update`'s signature over [`golden_payload`].
const PRIVATE_KEY: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";
const PUBLIC_KEY: &str = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";
const SIGN_UPDATE_SIGNATURE: &str =
    "fkZSsqZWXCJYAnlGh01EaGCGfLf0DLUpVDF6d7qpDPyexRhMbj85AAF61DR7ksXSCa/Yw1lZk6kDSYJOvioFAQ==";

fn golden_payload() -> Vec<u8> {
    (0..70 * 1024_u32)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap())
        .collect()
}

#[test]
fn signatures_match_sparkles_sign_update() {
    assert_eq!(BASE64.decode(PRIVATE_KEY).unwrap(), [7_u8; 32]);
    let key = signing_key(PRIVATE_KEY).unwrap();
    assert_eq!(key.public_key_base64(), PUBLIC_KEY);
    assert_eq!(key.sign_bytes(&golden_payload()), SIGN_UPDATE_SIGNATURE);
}

#[test]
fn only_a_32_byte_seed_is_a_signing_key() {
    signing_key(&format!("  {PRIVATE_KEY}\n")).unwrap();
    let legacy = signing_key(&BASE64.encode([7_u8; 96])).unwrap_err();
    assert!(format!("{legacy:#}").contains("legacy"), "{legacy:#}");
    for key in ["", "AAAA", "not base64!", &BASE64.encode([7_u8; 64])] {
        assert!(signing_key(key).is_err(), "{key:?}");
    }
}

#[test]
fn a_bundle_version_has_three_fields_for_stable_and_four_for_beta() {
    for version in ["0.8.14", "1.0.0", "10.20.30"] {
        ensure_sparkle_orders(version, UpdateChannel::Stable).unwrap();
    }
    for version in ["0.9.0-rc.1", "1.2", "1.2.3.4", "v1.2.3", "1..3", ""] {
        assert!(
            ensure_sparkle_orders(version, UpdateChannel::Stable).is_err(),
            "{version:?}"
        );
    }
    ensure_sparkle_orders("0.8.14.3", UpdateChannel::Beta).unwrap();
    for version in ["0.8.14", "0.9.0-beta.3", "0.8.14.3.1"] {
        assert!(
            ensure_sparkle_orders(version, UpdateChannel::Beta).is_err(),
            "{version:?}"
        );
    }
}

fn touch(dir: &Path, name: &str, contents: &[u8]) {
    fs_err::write(dir.join(name), contents).unwrap();
}

#[test]
fn every_architecture_needs_exactly_one_dmg() {
    let dist = tempfile::tempdir().unwrap();
    touch(dist.path(), "OpenLogi-v0.8.14-macos-x86_64.dmg", b"x86");
    touch(
        dist.path(),
        "OpenLogi-v0.8.14-macos-x86_64.dmg.minisig",
        b"sig",
    );
    touch(dist.path(), "OpenLogi-v0.8.14-windows-x86_64.msi", b"msi");
    touch(dist.path(), "SHA256SUMS", b"sums");
    let missing = release_dmgs(dist.path()).unwrap_err();
    assert!(missing.to_string().contains("no arm64 DMG"), "{missing}");

    touch(dist.path(), "OpenLogi-v0.8.14-macos-arm64.dmg", b"arm");
    // A delta's name ends in `.sparkle.delta`, so it is never taken for a DMG.
    touch(
        dist.path(),
        "OpenLogi-v0.8.14-macos-arm64.dmg.from-0.8.13.sparkle.delta",
        b"delta",
    );
    let dmgs = release_dmgs(dist.path()).unwrap();
    let found: Vec<_> = dmgs
        .iter()
        .map(|(arch, path)| (*arch, path.file_name().unwrap().to_str().unwrap()))
        .collect();
    assert_eq!(
        found,
        [
            (MacArch::Arm64, "OpenLogi-v0.8.14-macos-arm64.dmg"),
            (MacArch::X86_64, "OpenLogi-v0.8.14-macos-x86_64.dmg"),
        ]
    );

    touch(dist.path(), "OpenLogi-dev-macos-arm64.dmg", b"arm");
    let duplicate = release_dmgs(dist.path()).unwrap_err();
    assert!(
        duplicate.to_string().contains("two arm64 DMGs"),
        "{duplicate}"
    );
}

const SPARKLE_NS: &str = "http://www.andymatuschak.org/xml-namespaces/sparkle";

fn child<'a>(
    node: roxmltree::Node<'a, 'a>,
    ns: Option<&str>,
    name: &str,
) -> roxmltree::Node<'a, 'a> {
    node.children()
        .find(|n| n.tag_name().name() == name && n.tag_name().namespace() == ns)
        .unwrap_or_else(|| panic!("no <{name}> in <{}>", node.tag_name().name()))
}

fn text<'a>(node: roxmltree::Node<'a, 'a>, ns: Option<&str>, name: &str) -> &'a str {
    child(node, ns, name).text().unwrap_or_default()
}

fn release(notes: Option<&str>) -> Release<'static> {
    Release {
        bundle_version: "0.8.14",
        version: "0.8.14",
        channel: UpdateChannel::Stable,
        pub_date: "Mon, 12 Oct 2026 01:02:03 +0000".into(),
        page_url: "https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.14".into(),
        notes: notes.map(Into::into),
    }
}

fn enclosure(url: &str, length: u64) -> Enclosure {
    let mut enclosure = Enclosure::new(url, length, "application/x-apple-diskimage");
    enclosure.ed_signature = Some(SIGN_UPDATE_SIGNATURE.into());
    enclosure
}

/// The item OpenLogi wrote with its own template before rendering moved to
/// gpui-updater-release. Installed apps have parsed exactly this shape since
/// 0.8.14, and `release channels` copies the record's item into the feed they
/// read, so a release without deltas must keep producing it byte for byte.
#[test]
fn a_record_item_without_deltas_is_unchanged() {
    let xml = record(
        "https://u.example/releases/v0.8.14/feed/appcast-arm64.xml",
        &release(None),
        MacArch::Arm64,
        item(
            &release(Some("## Fixes\n- a ]]> b\n")),
            enclosure("https://u.example/a.dmg?x=1&y=2", 1234),
        ),
    );
    let expected = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>OpenLogi</title>
    <link>https://u.example/releases/v0.8.14/feed/appcast-arm64.xml</link>
    <description>OpenLogi 0.8.14 for arm64 (feed record)</description>
    <item>
      <title>OpenLogi 0.8.14</title>
      <pubDate>Mon, 12 Oct 2026 01:02:03 +0000</pubDate>
      <sparkle:version>0.8.14</sparkle:version>
      <sparkle:shortVersionString>0.8.14</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
      <link>https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.14</link>
      <sparkle:fullReleaseNotesLink>https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.14</sparkle:fullReleaseNotesLink>
      <description sparkle:format="markdown"><![CDATA[## Fixes
- a ]]]]><![CDATA[> b
]]></description>
      <enclosure url="https://u.example/a.dmg?x=1&amp;y=2" length="1234" type="application/x-apple-diskimage" sparkle:edSignature="{SIGN_UPDATE_SIGNATURE}"/>
    </item>
  </channel>
</rss>
"#
    );
    assert_eq!(xml, expected);
}

#[test]
fn appcast_item_carries_what_sparkle_reads() {
    // Markup that must survive: XML specials and a CDATA terminator.
    let notes = "## Fixes\n- a < b && c > d\n- literal ]]> stays\n";
    let url = "https://u.example/releases/v0.8.14/OpenLogi-v0.8.14-macos-arm64.dmg?a=1&b=2";
    let xml = record(
        "https://u.example/releases/v0.8.14/feed/appcast-arm64.xml",
        &release(None),
        MacArch::Arm64,
        item(&release(Some(notes)), enclosure(url, 1234)),
    );

    let doc = roxmltree::Document::parse(&xml).unwrap();
    let rss = doc.root_element();
    assert_eq!(rss.tag_name().name(), "rss");
    let channel = child(rss, None, "channel");
    let items: Vec<_> = channel
        .children()
        .filter(|n| n.tag_name().name() == "item")
        .collect();
    assert_eq!(items.len(), 1, "one release per record");
    let item = items[0];

    assert_eq!(text(item, Some(SPARKLE_NS), "version"), "0.8.14");
    assert_eq!(text(item, Some(SPARKLE_NS), "shortVersionString"), "0.8.14");
    assert_eq!(text(item, Some(SPARKLE_NS), "minimumSystemVersion"), "13.0");
    assert_eq!(
        text(item, None, "pubDate"),
        "Mon, 12 Oct 2026 01:02:03 +0000"
    );
    let description = child(item, None, "description");
    assert_eq!(
        description.attribute((SPARKLE_NS, "format")),
        Some("markdown")
    );
    let parsed: String = description.children().filter_map(|n| n.text()).collect();
    assert_eq!(parsed, notes);

    let enclosure_node = child(item, None, "enclosure");
    assert_eq!(enclosure_node.attribute("url"), Some(url));
    assert_eq!(enclosure_node.attribute("length"), Some("1234"));
    assert_eq!(
        enclosure_node.attribute("type"),
        Some("application/x-apple-diskimage")
    );
    assert_eq!(
        enclosure_node.attribute((SPARKLE_NS, "edSignature")),
        Some(SIGN_UPDATE_SIGNATURE)
    );
    assert!(
        !item.children().any(|n| n.tag_name().name() == "deltas"),
        "no deltas were given"
    );
    assert!(
        !item.children().any(|n| n.tag_name().name() == "channel"),
        "Sparkle offers a stable item to every channel"
    );
}

#[test]
fn appcast_without_notes_has_no_description_item() {
    let xml = record(
        "https://u.example/feed.xml",
        &release(None),
        MacArch::Arm64,
        item(&release(None), enclosure("https://u.example/a.dmg", 1)),
    );
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let item = child(child(doc.root_element(), None, "channel"), None, "item");
    assert!(
        !item
            .children()
            .any(|n| n.tag_name().name() == "description")
    );
}

/// Arguments for a stable `tag`, whose build version is its version.
fn args(dist: &Path, stage: &Path, tag: &str, public_key: &str) -> Args {
    Args {
        dist: dist.to_path_buf(),
        stage: stage.to_path_buf(),
        tag: tag.into(),
        build_version: tag.trim_start_matches('v').into(),
        base_url: "https://updates.example/".into(),
        public_key: public_key.into(),
        release_notes: None,
    }
}

fn release_dist() -> tempfile::TempDir {
    let dist = tempfile::tempdir().unwrap();
    touch(
        dist.path(),
        "OpenLogi-v0.8.15-macos-arm64.dmg",
        &golden_payload(),
    );
    touch(
        dist.path(),
        "OpenLogi-v0.8.15-macos-x86_64.dmg",
        b"intel dmg",
    );
    dist
}

/// A delta in `dist` with the metadata `release sparkle-deltas` writes next
/// to it (`SparkleDelta::write_metadata`'s format).
fn delta(dist: &Path, dmg: &str, from: &str, to: &str, contents: &[u8]) -> String {
    let name = format!("{dmg}.from-{from}.sparkle.delta");
    touch(dist, &name, contents);
    let metadata = serde_json::json!({
        "path": name,
        "length": contents.len(),
        "from": {
            "bundle_version": from,
            "short_version": from,
            "sparkle_executable_size": 976_432,
            "sparkle_locales": "ar,ca,cs",
        },
        "to_bundle_version": to,
        "format_version": null,
    });
    touch(
        dist,
        &format!("{name}.json"),
        metadata.to_string().as_bytes(),
    );
    name
}

#[test]
fn each_feed_offers_its_architectures_dmg_signed_with_the_embedded_key() {
    let dist = release_dist();
    let out = tempfile::tempdir().unwrap();
    write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap();

    let key = signing_key(PRIVATE_KEY).unwrap();
    for (arch, contents) in [
        (MacArch::Arm64, golden_payload()),
        (MacArch::X86_64, b"intel dmg".to_vec()),
    ] {
        let xml = fs_err::read_to_string(
            out.path()
                .join("releases/v0.8.15/feed")
                .join(arch.appcast_file_name()),
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let channel = child(doc.root_element(), None, "channel");
        assert_eq!(
            text(channel, None, "link"),
            format!(
                "https://updates.example/releases/v0.8.15/feed/appcast-{}.xml",
                arch.as_str()
            )
        );
        let enclosure = child(child(channel, None, "item"), None, "enclosure");
        assert_eq!(
            enclosure.attribute("url").unwrap(),
            format!(
                "https://updates.example/releases/v0.8.15/OpenLogi-v0.8.15-macos-{}.dmg",
                arch.as_str()
            )
        );
        assert_eq!(
            enclosure.attribute("length").unwrap(),
            contents.len().to_string()
        );
        // Ed25519 is deterministic: the signature over the DMG is exactly the
        // one `sign_update` makes with the embedded key's seed.
        assert_eq!(
            enclosure.attribute((SPARKLE_NS, "edSignature")),
            Some(key.sign_bytes(&contents).as_str())
        );
        if arch == MacArch::Arm64 {
            assert_eq!(
                enclosure.attribute((SPARKLE_NS, "edSignature")),
                Some(SIGN_UPDATE_SIGNATURE)
            );
        }
    }
}

#[test]
fn deltas_go_into_their_architectures_feed_signed() {
    let dist = release_dist();
    let out = tempfile::tempdir().unwrap();
    let arm = "OpenLogi-v0.8.15-macos-arm64.dmg";
    let from_14 = delta(dist.path(), arm, "0.8.14", "0.8.15", b"delta from 14");
    let from_13 = delta(dist.path(), arm, "0.8.13", "0.8.15", b"delta from 13");
    write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap();

    let key = signing_key(PRIVATE_KEY).unwrap();
    let records = out.path().join("releases/v0.8.15/feed");
    let xml = fs_err::read_to_string(records.join(MacArch::Arm64.appcast_file_name())).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let item = child(child(doc.root_element(), None, "channel"), None, "item");
    let deltas: Vec<_> = child(item, Some(SPARKLE_NS), "deltas")
        .children()
        .filter(|n| n.tag_name().name() == "enclosure")
        .collect();
    assert_eq!(deltas.len(), 2);
    for (node, (name, from, contents)) in deltas.iter().zip([
        (&from_13, "0.8.13", b"delta from 13"),
        (&from_14, "0.8.14", b"delta from 14"),
    ]) {
        assert_eq!(
            node.attribute("url").unwrap(),
            format!("https://updates.example/releases/v0.8.15/{name}")
        );
        assert_eq!(node.attribute((SPARKLE_NS, "deltaFrom")), Some(from));
        assert_eq!(
            node.attribute("length").unwrap(),
            contents.len().to_string()
        );
        assert_eq!(
            node.attribute((SPARKLE_NS, "edSignature")),
            Some(key.sign_bytes(contents).as_str())
        );
        assert_eq!(
            node.attribute((SPARKLE_NS, "deltaFromSparkleExecutableSize")),
            Some("976432")
        );
        assert_eq!(
            node.attribute((SPARKLE_NS, "deltaFromSparkleLocales")),
            Some("ar,ca,cs")
        );
    }

    let intel = fs_err::read_to_string(records.join(MacArch::X86_64.appcast_file_name())).unwrap();
    assert!(
        !intel.contains("sparkle:deltas"),
        "an arm64 delta leaked into the x86_64 feed"
    );
}

#[test]
fn a_delta_that_does_not_fit_this_release_writes_no_feed() {
    let arm = "OpenLogi-v0.8.15-macos-arm64.dmg";
    let out = tempfile::tempdir().unwrap();

    // Made for another release: Sparkle would rebuild a bundle the feed does
    // not describe.
    let dist = release_dist();
    delta(dist.path(), arm, "0.8.14", "0.8.16", b"delta");
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("not 0.8.15"), "{error:#}");
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());

    // No metadata: nothing says which build it updates from.
    let dist = release_dist();
    touch(
        dist.path(),
        &format!("{arm}.from-0.8.14.sparkle.delta"),
        b"x",
    );
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("metadata"), "{error:#}");
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());

    // Named for one base, made from another: Sparkle would offer it to the
    // wrong installed version.
    let dist = release_dist();
    let name = delta(dist.path(), arm, "0.8.14", "0.8.15", b"delta");
    let misnamed = name.replace("0.8.14", "0.8.13");
    fs_err::rename(dist.path().join(&name), dist.path().join(&misnamed)).unwrap();
    fs_err::rename(
        dist.path().join(format!("{name}.json")),
        dist.path().join(format!("{misnamed}.json")),
    )
    .unwrap();
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("should be named"),
        "{error:#}"
    );
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());

    // Changed after its metadata was written.
    let dist = release_dist();
    let name = delta(dist.path(), arm, "0.8.14", "0.8.15", b"delta");
    touch(dist.path(), &name, b"a different delta");
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.8.15", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("bytes"), "{error:#}");
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());
}

#[test]
fn a_key_the_bundles_do_not_embed_signs_nothing() {
    let dist = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    touch(dist.path(), "OpenLogi-v0.8.14-macos-arm64.dmg", b"arm");
    touch(dist.path(), "OpenLogi-v0.8.14-macos-x86_64.dmg", b"x86");
    let other_key = BASE64.encode([8_u8; 32]);
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.8.14", PUBLIC_KEY),
        &other_key,
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not belong"), "{error}");
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());

    // Not a release tag at all.
    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.9.0-rc.1", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(error.to_string().contains("pre-release"), "{error}");
    // A beta whose build version is not one `release version` gives it.
    let mut beta = args(dist.path(), out.path(), "v0.9.0-beta.1", PUBLIC_KEY);
    beta.build_version = "0.9.0".into();
    let error = write_appcasts(&beta, PRIVATE_KEY).unwrap_err();
    assert!(error.to_string().contains("ends in .1"), "{error}");
    assert!(fs_err::read_dir(out.path()).unwrap().next().is_none());
}

#[test]
fn a_beta_record_offers_its_build_version_on_the_beta_channel() {
    let dist = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    touch(
        dist.path(),
        "OpenLogi-v0.8.15-beta.4-macos-arm64.dmg",
        b"arm",
    );
    touch(
        dist.path(),
        "OpenLogi-v0.8.15-beta.4-macos-x86_64.dmg",
        b"x86",
    );
    let mut beta = args(dist.path(), out.path(), "v0.8.15-beta.4", PUBLIC_KEY);
    beta.build_version = "0.8.14.4".into();
    write_appcasts(&beta, PRIVATE_KEY).unwrap();

    let xml = fs_err::read_to_string(
        out.path()
            .join("releases/v0.8.15-beta.4/feed")
            .join(MacArch::Arm64.appcast_file_name()),
    )
    .unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let item = child(child(doc.root_element(), None, "channel"), None, "item");
    assert_eq!(text(item, Some(SPARKLE_NS), "version"), "0.8.14.4");
    assert_eq!(
        text(item, Some(SPARKLE_NS), "shortVersionString"),
        "0.8.15-beta.4"
    );
    assert_eq!(text(item, Some(SPARKLE_NS), "channel"), "beta");
    assert_eq!(text(item, None, "title"), "OpenLogi 0.8.15-beta.4");
    assert_eq!(
        child(item, None, "enclosure").attribute("url"),
        Some(
            "https://updates.example/releases/v0.8.15-beta.4/OpenLogi-v0.8.15-beta.4-macos-arm64.dmg"
        )
    );
}
