//! The Symbol section's rules: which Lucide icons a query finds and in what
//! order, which suggestions show before anything is typed, and what typed
//! emoji text saves as.

use super::super::key_symbol::{
    EMOJI_GROUPS, MAX_RESULTS, SymbolMode, emoji_presets, emoji_symbol, search_icons,
    suggested_icons,
};
use openlogi_core::config::KeySymbol;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn suggestions_keep_only_icons_the_catalog_has_in_their_order() {
    let catalog = names(&["brush", "camera", "mic", "zzz"]);

    assert_eq!(
        suggested_icons(&catalog),
        names(&["camera", "mic", "brush"])
    );
    assert!(suggested_icons(&[]).is_empty());
}

#[test]
fn a_blank_query_lists_the_suggestions() {
    let catalog = names(&["camera", "mic", "play", "pause", "zzz"]);

    assert_eq!(
        search_icons(&catalog, "  ", 60),
        names(&["camera", "mic", "play", "pause"])
    );
    assert_eq!(
        search_icons(&catalog, "", 2),
        names(&["camera", "mic"]),
        "capped too"
    );
}

#[test]
fn a_query_finds_substrings_exact_name_then_prefixes_first() {
    let catalog = names(&[
        "arrow-up",
        "camera",
        "camera-off",
        "image",
        "switch-camera",
        "video",
    ]);

    assert_eq!(
        search_icons(&catalog, " CAMERA ", 60),
        names(&["camera", "camera-off", "switch-camera"]),
        "case and surrounding space ignored; exact, then prefix, then the rest"
    );
    assert!(search_icons(&catalog, "nothing-like-it", 60).is_empty());
}

#[test]
fn the_search_is_capped_over_the_real_catalog() {
    let catalog = openlogi_ui::lucide::names();

    let arrows = search_icons(&catalog, "arrow", MAX_RESULTS);
    assert_eq!(arrows.len(), MAX_RESULTS, "the catalog has far more arrows");
    assert!(arrows.iter().all(|name| name.contains("arrow")));
    let suggested = search_icons(&catalog, "", MAX_RESULTS);
    assert!(
        suggested.len() >= 20,
        "the suggestions exist in Lucide 1.43"
    );
    assert_eq!(suggested[0], "camera");
}

#[test]
fn typed_emoji_is_trimmed_and_blank_saves_nothing() {
    assert_eq!(
        emoji_symbol(" 🎨 "),
        Some(KeySymbol::Emoji("🎨".to_owned()))
    );
    assert_eq!(emoji_symbol("   "), None);
    let long = "🎨".repeat(KeySymbol::MAX_EMOJI_CHARS + 4);
    let Some(KeySymbol::Emoji(kept)) = emoji_symbol(&long) else {
        panic!("a long emoji run still saves");
    };
    assert_eq!(kept.chars().count(), KeySymbol::MAX_EMOJI_CHARS);
    for preset in emoji_presets() {
        assert_eq!(
            emoji_symbol(preset),
            Some(KeySymbol::Emoji(preset.to_owned()))
        );
    }
}

#[test]
fn the_emoji_catalog_is_large_grouped_and_free_of_duplicates() {
    let all: Vec<&str> = emoji_presets().collect();
    assert!(all.len() >= 300, "only {} emojis", all.len());
    let unique: std::collections::BTreeSet<&str> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len(), "an emoji is offered twice");
    for group in EMOJI_GROUPS {
        assert!(!group.emojis.is_empty(), "{} is empty", group.key);
        assert!(
            group.key.starts_with("keyboard.emoji_group_"),
            "{}",
            group.key
        );
    }
}

#[test]
fn the_section_opens_on_the_kind_of_symbol_the_key_has() {
    assert_eq!(SymbolMode::of(None), SymbolMode::ActionIcon);
    assert_eq!(
        SymbolMode::of(Some(&KeySymbol::Lucide("camera".into()))),
        SymbolMode::Lucide
    );
    assert_eq!(
        SymbolMode::of(Some(&KeySymbol::Emoji("🎨".into()))),
        SymbolMode::Emoji
    );
}
