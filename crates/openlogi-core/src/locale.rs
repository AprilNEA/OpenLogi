//! Locale negotiation: which shipped catalog a BCP-47 code resolves to.
//!
//! Shared by every process that renders localized text — the settings app
//! picks the locale from its stored [`AppSettings`](crate::config::AppSettings),
//! the agent from the same setting for its menu-bar tray, and the overlay
//! helper from the code the agent hands it per invocation. Keeping one
//! implementation is what stops the processes from disagreeing about, say,
//! whether `zh-Hant-HK` is Hong Kong or Taiwan.
//!
//! The catalogs themselves stay in `crates/openlogi-ui/locales/` (each binary
//! expands its own `rust_i18n::i18n!` over that directory by relative path);
//! this module is only the negotiation over their codes, so it can live in the
//! platform-free core where the GUI-less agent can reach it.
//!
//! Setting the locale is global: `rust_i18n` stores it in a process-wide atomic
//! that gpui-component reads too, so in the settings app
//! [`crate::locale::activate`] re-localizes the framework's own widget strings
//! alongside ours.

use fluent_langneg::{LanguageIdentifier, NegotiationStrategy, negotiate_languages};

/// Locales the GUI ships, as `(code, native name)`. The codes match the
/// `locales/*.toml` filenames; a subset (`en`, `zh-CN`, `zh-HK`, `it`) also
/// matches gpui-component's bundled `ui.yml`, so choosing one localizes the
/// framework's own widgets too. Under a locale the framework doesn't bundle, our
/// app strings localize but gpui-component's built-in widget strings fall back
/// to English.
/// Order here is the order shown in the Settings picker (after "Follow system"):
/// native-name alphabetical within each script.
pub const SUPPORTED: &[(&str, &str)] = &[
    ("be", "Беларуская"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("en", "English"),
    ("es", "Español"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("nb", "Norsk"),
    ("pl", "Polski"),
    ("pt-PT", "Português"),
    ("pt-BR", "Português - Brasil"),
    ("fi", "Suomi"),
    ("sv", "Svenska"),
    ("tr", "Türkçe"),
    ("el", "Ελληνικά"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("ja", "日本語"),
    ("zh-CN", "简体中文"),
    ("zh-HK", "繁體中文（香港）"),
    ("zh-TW", "正體中文（臺灣）"),
    ("ko", "한국어"),
];

/// Resolve the locale to apply, preferring an explicit stored `setting`, then
/// the system locale, and finally `"en"`. An unrecognized stored code is
/// treated as "follow system" rather than failing.
fn resolve(setting: Option<&str>) -> &'static str {
    setting
        .and_then(match_supported)
        .or_else(|| {
            sys_locale::get_locale()
                .as_deref()
                .and_then(match_supported)
        })
        .unwrap_or("en")
}

/// Collapse an arbitrary BCP-47 locale onto one of [`SUPPORTED`], or `None`,
/// by matching its primary subtag. Three families need more than a primary-tag
/// match:
/// - `zh` is decided by examining all subtags for script and region: explicit
///   `Hans` → `zh-CN` (always wins); `hk` / `mo` region → `zh-HK`; `tw` region
///   or bare `Hant` script → `zh-TW`; no recognized indicator → `zh-CN`. So
///   `zh-Hans-HK` stays Simplified (script wins), `zh-Hant-HK` resolves to Hong
///   Kong (region wins over generic script), and bare `zh-Hant` → Taiwan.
/// - `pt` splits on region: a `br` subtag → `pt-BR`, otherwise `pt-PT`.
/// - Norwegian's `nb` / `nn` / the macrolanguage `no` all fold onto `nb`
///   (the catalog ships Bokmål, shown as "Norsk").
fn match_supported(code: &str) -> Option<&'static str> {
    let requested = code.replace('_', "-").parse::<LanguageIdentifier>().ok()?;
    special_locale(&requested).or_else(|| lookup_supported(&requested))
}

fn special_locale(requested: &LanguageIdentifier) -> Option<&'static str> {
    match requested.language.as_str() {
        "nb" | "nn" | "no" => Some("nb"),
        "pt" => {
            if requested
                .region
                .as_ref()
                .is_some_and(|region| region.as_str() == "BR")
            {
                Some("pt-BR")
            } else {
                Some("pt-PT")
            }
        }
        "zh" => {
            let script = requested.script.as_ref().map(ToString::to_string);
            let region = requested.region.as_ref().map(ToString::to_string);
            match (script.as_deref(), region.as_deref()) {
                (Some("Hans"), _) => Some("zh-CN"),
                (_, Some("HK" | "MO")) => Some("zh-HK"),
                (_, Some("TW")) | (Some("Hant"), _) => Some("zh-TW"),
                _ => Some("zh-CN"),
            }
        }
        _ => None,
    }
}

fn lookup_supported(requested: &LanguageIdentifier) -> Option<&'static str> {
    let available = supported_langids();
    let matched = negotiate_languages(
        std::slice::from_ref(requested),
        &available,
        None,
        NegotiationStrategy::Lookup,
    )
    .into_iter()
    .next()?;
    let matched = matched.to_string();
    SUPPORTED
        .iter()
        .find_map(|(code, _)| (*code == matched).then_some(*code))
}

fn supported_langids() -> Vec<LanguageIdentifier> {
    SUPPORTED
        .iter()
        .filter_map(|(code, _)| code.parse().ok())
        .collect()
}

/// Switch the process-global locale to the resolution of `language`
/// (`None` = follow system). The single resolve→`set_locale` surface for every
/// caller — app startup, the live Settings switch, and each overlay
/// invocation — so the resolution policy can't drift between them.
///
/// The caller is responsible for refreshing any open window afterwards; views
/// already rendered do not re-read the locale on their own.
pub fn activate(language: Option<&str>) {
    rust_i18n::set_locale(resolve(language));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_locale_variants() {
        assert_eq!(match_supported("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(match_supported("zh-CN"), Some("zh-CN"));
        assert_eq!(match_supported("zh-Hans-HK"), Some("zh-CN"));
        assert_eq!(match_supported("zh-Hant-TW"), Some("zh-TW"));
        assert_eq!(match_supported("zh-TW"), Some("zh-TW"));
        assert_eq!(match_supported("zh-Hant"), Some("zh-TW"));
        assert_eq!(match_supported("zh-HK"), Some("zh-HK"));
        assert_eq!(match_supported("zh-Hant-HK"), Some("zh-HK"));
        assert_eq!(match_supported("ja"), Some("ja"));
        assert_eq!(match_supported("ja-JP"), Some("ja"));
        assert_eq!(match_supported("ru"), Some("ru"));
        assert_eq!(match_supported("ru-RU"), Some("ru"));
        assert_eq!(match_supported("uk"), Some("uk"));
        assert_eq!(match_supported("uk-UA"), Some("uk"));
        assert_eq!(match_supported("en-US"), Some("en"));
        assert_eq!(match_supported("it"), Some("it"));
        assert_eq!(match_supported("it-IT"), Some("it"));
        assert_eq!(match_supported("fr-FR"), Some("fr"));
        assert_eq!(match_supported("de"), Some("de"));
        assert_eq!(match_supported("ko-KR"), Some("ko"));
        assert_eq!(match_supported("pt"), Some("pt-PT"));
        assert_eq!(match_supported("pt-PT"), Some("pt-PT"));
        assert_eq!(match_supported("pt-BR"), Some("pt-BR"));
        assert_eq!(match_supported("tr"), Some("tr"));
        assert_eq!(match_supported("tr-TR"), Some("tr"));
        assert_eq!(match_supported("nb-NO"), Some("nb"));
        assert_eq!(match_supported("no"), Some("nb"));
        assert_eq!(match_supported("nn"), Some("nb"));
        assert_eq!(match_supported("be"), Some("be"));
        assert_eq!(match_supported("be-BY"), Some("be"));
        assert_eq!(match_supported("klingon"), None);
    }

    #[test]
    fn explicit_setting_wins_over_system() {
        assert_eq!(resolve(Some("zh-CN")), "zh-CN");
        // An unknown stored code falls through to system/`en`, never panics.
        assert!(
            SUPPORTED
                .iter()
                .any(|(c, _)| *c == resolve(Some("klingon")))
        );
    }
}
