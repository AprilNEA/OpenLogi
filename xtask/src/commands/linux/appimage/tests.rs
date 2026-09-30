use std::ffi::OsStr;
use std::path::Path;

use super::{APPLICATIONS_DIR, DIR_ICON_DIR, Entry, nfpm_contents, single_entry_under};
use crate::commands::linux::package::PACKAGED_BINS;

const NFPM_YAML: &str = include_str!("../../../../../packaging/linux/nfpm.yaml");

/// The AppDir is staged from `nfpm.yaml`, so the parse has to see every
/// binary the package installs — the same set the build produces.
#[test]
fn the_real_manifest_yields_every_packaged_binary() {
    let entries = nfpm_contents(NFPM_YAML).expect("nfpm.yaml parses");
    for bin in PACKAGED_BINS {
        let src = format!("target/release/{bin}");
        let entry = entries
            .iter()
            .find(|entry| entry.src == src)
            .unwrap_or_else(|| panic!("{src} is not installed by nfpm.yaml"));
        assert_eq!(entry.dst, format!("/usr/bin/{bin}"));
    }
}

/// appimagetool needs one desktop entry and one root icon; both are found by
/// the directory nfpm installs them into, so the manifest has to keep exactly
/// one file in each.
#[test]
fn the_real_manifest_names_the_root_desktop_entry_and_icon() {
    let entries = nfpm_contents(NFPM_YAML).expect("nfpm.yaml parses");
    let desktop = single_entry_under(&entries, APPLICATIONS_DIR).expect("one desktop entry");
    assert_eq!(Path::new(desktop).extension(), Some(OsStr::new("desktop")));
    let icon = single_entry_under(&entries, DIR_ICON_DIR).expect("one 256px icon");
    assert_eq!(Path::new(icon).extension(), Some(OsStr::new("png")));
}

#[test]
fn entries_pair_each_src_with_the_following_dst() {
    let config = "contents:\n  - src: a\n    dst: /x/a\n    file_info:\n      mode: 0644\n  - src: b\n    dst: /y/b\n";
    let entries = nfpm_contents(config).expect("parses");
    assert_eq!(
        entries,
        vec![
            Entry {
                src: "a",
                dst: "/x/a"
            },
            Entry {
                src: "b",
                dst: "/y/b"
            },
        ]
    );
}

#[test]
fn an_entry_without_dst_is_rejected() {
    let missing_middle = "  - src: a\n  - src: b\n    dst: /y/b\n";
    nfpm_contents(missing_middle).expect_err("a src without a following dst");
    let missing_last = "  - src: a\n    dst: /x/a\n  - src: b\n";
    nfpm_contents(missing_last).expect_err("a trailing src without a dst");
}

#[test]
fn a_relative_dst_is_rejected() {
    nfpm_contents("  - src: a\n    dst: usr/bin/a\n").expect_err("relative dst");
}

#[test]
fn a_manifest_without_contents_is_rejected() {
    nfpm_contents("name: openlogi\n").expect_err("no contents");
}
