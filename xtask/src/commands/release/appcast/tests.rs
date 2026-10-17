use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_compact::{PublicKey, Signature};

use super::{
    Args, Enclosure, Release, appcast, ensure_sparkle_orders, release_dmgs, sign, signing_key,
    write_appcasts,
};
use crate::support::update_feed::MacArch;

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
    assert_eq!(BASE64.encode(key.pk.as_ref()), PUBLIC_KEY);
    assert_eq!(
        sign(&key, &golden_payload()).unwrap(),
        SIGN_UPDATE_SIGNATURE
    );
}

#[test]
fn only_a_32_byte_seed_is_a_signing_key() {
    signing_key(&format!("  {PRIVATE_KEY}\n")).unwrap();
    let legacy = signing_key(&BASE64.encode([7_u8; 96])).unwrap_err();
    assert!(legacy.to_string().contains("legacy"), "{legacy}");
    for key in ["", "AAAA", "not base64!", &BASE64.encode([7_u8; 64])] {
        assert!(signing_key(key).is_err(), "{key:?}");
    }
}

#[test]
fn the_stable_feed_takes_final_releases_only() {
    for version in ["0.8.14", "1.0.0", "10.20.30"] {
        ensure_sparkle_orders(version).unwrap();
    }
    for version in ["0.9.0-rc.1", "1.2", "1.2.3.4", "v1.2.3", "1..3", ""] {
        assert!(ensure_sparkle_orders(version).is_err(), "{version:?}");
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

#[test]
fn appcast_item_carries_what_sparkle_reads() {
    let release = Release {
        version: "0.8.14",
        pub_date: "Mon, 12 Oct 2026 01:02:03 +0000".into(),
        page_url: "https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.14".into(),
        // Markup that must survive: XML specials and a CDATA terminator.
        notes: Some("## Fixes\n- a < b && c > d\n- literal ]]> stays\n".into()),
    };
    let enclosure = Enclosure {
        url: "https://u.example/releases/v0.8.14/OpenLogi-v0.8.14-macos-arm64.dmg?a=1&b=2".into(),
        length: 1234,
        signature: SIGN_UPDATE_SIGNATURE.into(),
    };
    let xml = appcast(
        "https://u.example/channels/stable/appcast-arm64.xml",
        &release,
        &enclosure,
    );
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let rss = doc.root_element();
    assert_eq!(rss.tag_name().name(), "rss");
    let channel = child(rss, None, "channel");
    let items: Vec<_> = channel
        .children()
        .filter(|n| n.tag_name().name() == "item")
        .collect();
    assert_eq!(items.len(), 1, "one release per feed");
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
    let notes: String = description.children().filter_map(|n| n.text()).collect();
    assert_eq!(notes, release.notes.as_deref().unwrap());

    let enclosure_node = child(item, None, "enclosure");
    assert_eq!(
        enclosure_node.attribute("url"),
        Some(enclosure.url.as_str())
    );
    assert_eq!(enclosure_node.attribute("length"), Some("1234"));
    assert_eq!(
        enclosure_node.attribute("type"),
        Some("application/x-apple-diskimage")
    );
    assert_eq!(
        enclosure_node.attribute((SPARKLE_NS, "edSignature")),
        Some(SIGN_UPDATE_SIGNATURE)
    );
}

#[test]
fn appcast_without_notes_has_no_description_item() {
    let release = Release {
        version: "0.8.14",
        pub_date: "Mon, 12 Oct 2026 01:02:03 +0000".into(),
        page_url: "https://github.com/AprilNEA/OpenLogi/releases/tag/v0.8.14".into(),
        notes: None,
    };
    let enclosure = Enclosure {
        url: "https://u.example/a.dmg".into(),
        length: 1,
        signature: SIGN_UPDATE_SIGNATURE.into(),
    };
    let xml = appcast("https://u.example/feed.xml", &release, &enclosure);
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let item = child(child(doc.root_element(), None, "channel"), None, "item");
    assert!(
        !item
            .children()
            .any(|n| n.tag_name().name() == "description")
    );
}

fn args(dist: &Path, output_dir: &Path, tag: &str, public_key: &str) -> Args {
    Args {
        dist: dist.to_path_buf(),
        output_dir: output_dir.to_path_buf(),
        tag: tag.into(),
        base_url: "https://updates.example/".into(),
        public_key: public_key.into(),
        release_notes: None,
    }
}

#[test]
fn each_feed_offers_its_architectures_dmg_signed_with_the_embedded_key() {
    let dist = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    touch(
        dist.path(),
        "OpenLogi-v0.8.14-macos-arm64.dmg",
        &golden_payload(),
    );
    touch(
        dist.path(),
        "OpenLogi-v0.8.14-macos-x86_64.dmg",
        b"intel dmg",
    );
    write_appcasts(
        &args(dist.path(), out.path(), "v0.8.14", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap();

    let public_key = PublicKey::from_slice(&BASE64.decode(PUBLIC_KEY).unwrap()).unwrap();
    for (arch, contents) in [
        (MacArch::Arm64, golden_payload()),
        (MacArch::X86_64, b"intel dmg".to_vec()),
    ] {
        let xml = fs_err::read_to_string(out.path().join(arch.appcast_file_name())).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let channel = child(doc.root_element(), None, "channel");
        assert_eq!(
            text(channel, None, "link"),
            format!(
                "https://updates.example/channels/stable/appcast-{}.xml",
                arch.as_str()
            )
        );
        let enclosure = child(child(channel, None, "item"), None, "enclosure");
        assert_eq!(
            enclosure.attribute("url").unwrap(),
            format!(
                "https://updates.example/releases/v0.8.14/OpenLogi-v0.8.14-macos-{}.dmg",
                arch.as_str()
            )
        );
        assert_eq!(
            enclosure.attribute("length").unwrap(),
            contents.len().to_string()
        );
        let signature = enclosure.attribute((SPARKLE_NS, "edSignature")).unwrap();
        let signature = Signature::from_slice(&BASE64.decode(signature).unwrap()).unwrap();
        public_key.verify(&contents, &signature).unwrap();
        if arch == MacArch::Arm64 {
            assert_eq!(
                enclosure.attribute((SPARKLE_NS, "edSignature")),
                Some(SIGN_UPDATE_SIGNATURE)
            );
        }
    }
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

    let error = write_appcasts(
        &args(dist.path(), out.path(), "v0.9.0-rc.1", PUBLIC_KEY),
        PRIVATE_KEY,
    )
    .unwrap_err();
    assert!(error.to_string().contains("MAJOR.MINOR.PATCH"), "{error}");
}
