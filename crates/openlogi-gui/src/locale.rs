//! Pure locale negotiation shared by the settings app and overlay helper.

use fluent_langneg::{LanguageIdentifier, NegotiationStrategy, negotiate_languages};

/// Locales the GUI ships, as `(code, native name)` in picker order.
pub const SUPPORTED: &[(&str, &str)] = &[
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
    ("el", "Ελληνικά"),
    ("ru", "Русский"),
    ("ja", "日本語"),
    ("zh-CN", "简体中文"),
    ("zh-HK", "繁體中文（香港）"),
    ("zh-TW", "正體中文（臺灣）"),
    ("ko", "한국어"),
];

/// Resolve an explicit setting or system locale to a shipped locale.
#[must_use]
pub fn resolve(setting: Option<&str>) -> &'static str {
    setting
        .and_then(match_supported)
        .or_else(|| {
            sys_locale::get_locale()
                .as_deref()
                .and_then(match_supported)
        })
        .unwrap_or("en")
}

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
    .next()?
    .to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_locale_variants() {
        assert_eq!(match_supported("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(match_supported("zh-Hans-HK"), Some("zh-CN"));
        assert_eq!(match_supported("zh-Hant-TW"), Some("zh-TW"));
        assert_eq!(match_supported("zh-Hant"), Some("zh-TW"));
        assert_eq!(match_supported("zh-HK"), Some("zh-HK"));
        assert_eq!(match_supported("zh-Hant-HK"), Some("zh-HK"));
        assert_eq!(match_supported("ja-JP"), Some("ja"));
        assert_eq!(match_supported("ru-RU"), Some("ru"));
        assert_eq!(match_supported("en-US"), Some("en"));
        assert_eq!(match_supported("it-IT"), Some("it"));
        assert_eq!(match_supported("fr-FR"), Some("fr"));
        assert_eq!(match_supported("ko-KR"), Some("ko"));
        assert_eq!(match_supported("pt"), Some("pt-PT"));
        assert_eq!(match_supported("pt-BR"), Some("pt-BR"));
        assert_eq!(match_supported("nb-NO"), Some("nb"));
        assert_eq!(match_supported("no"), Some("nb"));
        assert_eq!(match_supported("nn"), Some("nb"));
        assert_eq!(match_supported("klingon"), None);
    }
}
