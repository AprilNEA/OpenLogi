//! Key parity for the shared locale catalogs in `locales/`.
//!
//! The negotiation over these catalogs' codes lives in
//! [`openlogi_core::locale`] (where the GUI-less agent can reach it); the
//! `.yml` files stay here, and this test is what keeps the two in lockstep:
//! every catalog carries exactly `en.yml`'s keys, and every code in
//! [`openlogi_core::locale::SUPPORTED`] has a parity-checked catalog.

use std::collections::BTreeSet;

use openlogi_core::locale::SUPPORTED;

/// Every shipped locale must carry the same keys as `en.yml`. New UI copy
/// is added to all catalogs in the same change; Crowdin later improves the
/// non-English values (never English fill-in).
#[test]
fn locale_files_have_the_same_keys() {
    let source: BTreeSet<&str> = locale_keys(include_str!("../locales/en.yml"))
        .into_iter()
        .collect();
    assert!(
        !source.is_empty(),
        "en.yml is the string source of truth and must define keys"
    );

    let catalogs = [
        ("ja", include_str!("../locales/ja.yml")),
        ("ru", include_str!("../locales/ru.yml")),
        ("uk", include_str!("../locales/uk.yml")),
        ("zh-CN", include_str!("../locales/zh-CN.yml")),
        ("zh-HK", include_str!("../locales/zh-HK.yml")),
        ("zh-TW", include_str!("../locales/zh-TW.yml")),
        ("it", include_str!("../locales/it.yml")),
        ("da", include_str!("../locales/da.yml")),
        ("de", include_str!("../locales/de.yml")),
        ("el", include_str!("../locales/el.yml")),
        ("es", include_str!("../locales/es.yml")),
        ("fi", include_str!("../locales/fi.yml")),
        ("fr", include_str!("../locales/fr.yml")),
        ("ko", include_str!("../locales/ko.yml")),
        ("nb", include_str!("../locales/nb.yml")),
        ("nl", include_str!("../locales/nl.yml")),
        ("pl", include_str!("../locales/pl.yml")),
        ("pt-BR", include_str!("../locales/pt-BR.yml")),
        ("pt-PT", include_str!("../locales/pt-PT.yml")),
        ("sv", include_str!("../locales/sv.yml")),
        ("tr", include_str!("../locales/tr.yml")),
    ];

    // `include_str!` needs literal paths, so this list is written out by
    // hand — which means it can silently fall behind [`SUPPORTED`], and a
    // locale nobody parity-checks is exactly how a catalog drifts.
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
        let keys: BTreeSet<&str> = locale_keys(file).into_iter().collect();
        let missing: Vec<&str> = source.difference(&keys).copied().collect();
        let extras: Vec<&str> = keys.difference(&source).copied().collect();
        assert!(
            missing.is_empty() && extras.is_empty(),
            "{locale}.yml key mismatch vs en.yml — missing: {missing:?}, extras: {extras:?}"
        );
    }
}

fn locale_keys(file: &str) -> Vec<&str> {
    file.lines()
        .filter_map(|line| line.strip_prefix('"'))
        .filter_map(|line| line.split_once("\": ").map(|(key, _)| key))
        .collect()
}
