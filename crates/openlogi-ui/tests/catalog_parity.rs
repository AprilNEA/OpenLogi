#![expect(
    clippy::tests_outside_test_module,
    reason = "integration tests are test targets by construction"
)]
#![expect(
    clippy::expect_used,
    reason = "the free integration-test parser helper must reject entries outside a domain table"
)]

//! Key parity for the shared locale catalogs in `locales/`.
//!
//! The negotiation over these catalogs' codes lives in
//! [`openlogi_core::locale`] (where the GUI-less agent can reach it); the
//! `.toml` files stay here, and this test is what keeps the two in lockstep:
//! every catalog carries exactly `en.toml`'s keys, and every code in
//! [`openlogi_core::locale::SUPPORTED`] has a parity-checked catalog.

use std::collections::BTreeSet;

use openlogi_core::locale::SUPPORTED;

/// Every shipped locale must carry the same keys as `en.toml`. New UI copy
/// is added to all catalogs in the same change; Crowdin later improves the
/// non-English values (never English fill-in).
#[test]
fn locale_files_have_the_same_keys() {
    let source = locale_entries(include_str!("../locales/en.toml"));
    assert!(
        !source.is_empty(),
        "en.toml is the string source of truth and must define keys"
    );

    let catalogs = [
        ("be", include_str!("../locales/be.toml")),
        ("ja", include_str!("../locales/ja.toml")),
        ("ru", include_str!("../locales/ru.toml")),
        ("uk", include_str!("../locales/uk.toml")),
        ("zh-CN", include_str!("../locales/zh-CN.toml")),
        ("zh-HK", include_str!("../locales/zh-HK.toml")),
        ("zh-TW", include_str!("../locales/zh-TW.toml")),
        ("it", include_str!("../locales/it.toml")),
        ("da", include_str!("../locales/da.toml")),
        ("de", include_str!("../locales/de.toml")),
        ("el", include_str!("../locales/el.toml")),
        ("es", include_str!("../locales/es.toml")),
        ("fi", include_str!("../locales/fi.toml")),
        ("fr", include_str!("../locales/fr.toml")),
        ("ko", include_str!("../locales/ko.toml")),
        ("nb", include_str!("../locales/nb.toml")),
        ("nl", include_str!("../locales/nl.toml")),
        ("pl", include_str!("../locales/pl.toml")),
        ("pt-BR", include_str!("../locales/pt-BR.toml")),
        ("pt-PT", include_str!("../locales/pt-PT.toml")),
        ("sv", include_str!("../locales/sv.toml")),
        ("tr", include_str!("../locales/tr.toml")),
    ];

    // `include_str!` needs literal paths, so this list is written out by
    // hand, which means it can silently fall behind [`SUPPORTED`]. A locale
    // nobody parity-checks is exactly how a catalog drifts.
    let checked: BTreeSet<&str> = catalogs
        .iter()
        .map(|(locale, _)| *locale)
        .chain(["en"])
        .collect();
    let shipped: BTreeSet<&str> = SUPPORTED.iter().map(|(code, _)| *code).collect();
    assert_eq!(
        checked, shipped,
        "every locale in SUPPORTED must be parity-checked here"
    );

    for (locale, file) in catalogs {
        let entries = locale_entries(file);
        let source_keys: BTreeSet<&str> = source.iter().map(|(key, _)| key.as_str()).collect();
        let keys: BTreeSet<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
        let missing: Vec<&str> = source_keys.difference(&keys).copied().collect();
        let extras: Vec<&str> = keys.difference(&source_keys).copied().collect();
        assert!(
            missing.is_empty() && extras.is_empty(),
            "{locale}.toml key mismatch vs en.toml — missing: {missing:?}, extras: {extras:?}"
        );
        assert_eq!(
            entries.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            source.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            "{locale}.toml key order must match en.toml"
        );
        for ((key, placeholders), (_, source_placeholders)) in entries.iter().zip(&source) {
            assert_eq!(
                placeholders, source_placeholders,
                "{locale}.toml placeholders differ for {key}"
            );
        }
    }
}

fn locale_entries(file: &str) -> Vec<(String, BTreeSet<String>)> {
    let mut table = None;
    let mut entries = Vec::new();
    for line in file.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            table = Some(&line[1..line.len() - 1]);
            continue;
        }
        if let Some((key, value)) = line.split_once(" = ")
            && key != "_version"
        {
            let table = table.expect("translation entries must belong to a domain table");
            entries.push((format!("{table}.{key}"), placeholders(value)));
        }
    }
    entries
}

fn placeholders(value: &str) -> BTreeSet<String> {
    let mut placeholders = BTreeSet::new();
    let mut rest = value;
    while let Some(start) = rest.find("%{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else { break };
        placeholders.insert(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    placeholders
}
