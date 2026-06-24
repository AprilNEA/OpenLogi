//! UI localization plumbing.
//!
//! Translations live in `crates/openlogi-gui/locales/*.yml` and are loaded at
//! compile time by the `rust_i18n::i18n!` macro in `main.rs`. Crowdin manages one
//! file per locale: `en.yml` is the source file, and every other locale in
//! [`SUPPORTED`] is downloaded as a translated YAML file. Call sites use the
//! [`tr!`](crate::tr) helper (or `rust_i18n::t!`) with the **English string as
//! the key**.
//!
//! The current locale is a process-global atomic inside `rust_i18n`. Setting it
//! re-localizes both our own call sites *and* gpui-component's built-in widget
//! strings, since the framework reads the same global. Apply it once at startup
//! via [`apply`] and on a live switch via
//! [`AppState::set_language`](crate::state::AppState::set_language); each must be
//! followed by a window refresh so open views re-render with the new locale.

use fluent_langneg::{LanguageIdentifier, NegotiationStrategy, negotiate_languages};
use openlogi_core::config::AppSettings;

/// Locales the GUI ships, as `(code, native name)`. The codes match the
/// `locales/*.yml` filenames; a subset (`en`, `zh-CN`, `zh-HK`, `it`) also
/// matches gpui-component's bundled `ui.yml`, so choosing one localizes the
/// framework's own widgets too. Under a locale the framework doesn't bundle, our
/// app strings localize but gpui-component's built-in widget strings fall back
/// to English.
/// Order here is the order shown in the Settings picker (after "Follow system"):
/// native-name alphabetical within each script.
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

/// Resolve the locale to apply, preferring an explicit stored `setting`, then
/// the system locale, and finally `"en"`. An unrecognized stored code is
/// treated as "follow system" rather than failing.
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
/// (`None` = follow system). The single resolve→`set_locale` surface shared by
/// startup ([`apply`]) and the live Settings switch
/// ([`AppState::set_language`](crate::state::AppState::set_language)), so the
/// resolution policy can't drift between them.
pub fn activate(language: Option<&str>) {
    rust_i18n::set_locale(resolve(language));
}

/// Apply the configured language to the process-global locale at startup.
/// Safe to call before any window opens — the locale is a plain atomic.
pub fn apply(settings: &AppSettings) {
    activate(settings.language.as_deref());
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
        assert_eq!(match_supported("en-US"), Some("en"));
        assert_eq!(match_supported("it"), Some("it"));
        assert_eq!(match_supported("it-IT"), Some("it"));
        assert_eq!(match_supported("fr-FR"), Some("fr"));
        assert_eq!(match_supported("de"), Some("de"));
        assert_eq!(match_supported("ko-KR"), Some("ko"));
        assert_eq!(match_supported("pt"), Some("pt-PT"));
        assert_eq!(match_supported("pt-PT"), Some("pt-PT"));
        assert_eq!(match_supported("pt-BR"), Some("pt-BR"));
        assert_eq!(match_supported("nb-NO"), Some("nb"));
        assert_eq!(match_supported("no"), Some("nb"));
        assert_eq!(match_supported("nn"), Some("nb"));
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

    /// End-to-end check that `locales/*.yml` loaded and the gettext-style
    /// English keys match — a typo'd key silently falls back to English, which
    /// this catches. All locale-dependent assertions live in this one test on
    /// purpose: `rust_i18n`'s locale is a process-global, so splitting them into
    /// separate `#[test]`s would race under the parallel harness.
    #[test]
    fn locale_file_resolves_keys() {
        use openlogi_core::binding::{Action, ButtonId, GestureDirection};

        // The accessibility blurb is the longest, most typo-prone key.
        const BLURB: &str = "OpenLogi captures mouse buttons (Back / Forward / gesture button) through the system Accessibility permission and runs the actions you bind. Features that talk to the device directly — DPI, SmartShift — are unaffected.";

        rust_i18n::set_locale("zh-CN");
        assert_eq!(rust_i18n::t!("Settings"), "设置"); // GUI chrome
        assert_eq!(rust_i18n::t!("Left Click"), "左键单击"); // core enum label
        assert_eq!(rust_i18n::t!("DPI"), "灵敏度"); // DPI panel/category label
        assert_eq!(rust_i18n::t!("Bind %{name}", name => "X"), "绑定 X"); // interpolation
        assert_eq!(rust_i18n::t!("Unbound"), "未绑定"); // mouse model card state
        assert_eq!(rust_i18n::t!("Default"), "默认"); // default-binding card state
        assert_eq!(rust_i18n::t!("5 directions"), "5 个方向"); // gesture card summary
        assert_eq!(
            rust_i18n::t!("DPI Preset %{index}", index => "2"),
            "灵敏度预设 2"
        ); // parameterized action label
        assert_eq!(rust_i18n::t!("Quit OpenLogi"), "退出 OpenLogi"); // menu-bar status item
        assert_eq!(rust_i18n::t!("No devices connected"), "未连接设备"); // menu-bar device line
        assert_eq!(rust_i18n::t!("Lighting"), "灯光"); // keyboard lighting tab
        assert_eq!(rust_i18n::t!("BRIGHTNESS"), "亮度"); // lighting panel label
        assert_ne!(
            rust_i18n::t!(BLURB),
            BLURB,
            "blurb key missing from zh-CN.yml"
        );

        // Exhaustive: every non-parameterized device/action label has a `zh-CN`
        // entry. Parameterized `Action`s (`SetDpiPreset`, `CustomShortcut`) are
        // skipped here and checked explicitly above where needed.
        let covered = |label: &str| rust_i18n::t!(label) != label;
        for b in ButtonId::ALL {
            assert!(covered(b.label()), "no zh-CN for ButtonId::{b:?}");
        }
        for d in GestureDirection::ALL {
            assert!(covered(d.label()), "no zh-CN for GestureDirection::{d:?}");
        }
        for a in Action::catalog() {
            assert!(covered(&a.label()), "no zh-CN for Action::{a:?}");
            assert!(
                covered(a.category().label()),
                "no zh-CN for {:?}",
                a.category()
            );
        }

        rust_i18n::set_locale("ja");
        assert_eq!(rust_i18n::t!("Settings"), "設定");
        assert_eq!(rust_i18n::t!("Left Click"), "左クリック");

        rust_i18n::set_locale("ru");
        assert_eq!(rust_i18n::t!("Settings"), "Настройки");
        assert_eq!(rust_i18n::t!("Left Click"), "Левый щелчок");

        rust_i18n::set_locale("zh-TW");
        assert_eq!(rust_i18n::t!("Settings"), "設定");
        assert_eq!(rust_i18n::t!("Left Click"), "左鍵按一下");
        assert_eq!(rust_i18n::t!("Bind %{name}", name => "X"), "設定 X");
        assert_ne!(
            rust_i18n::t!(BLURB),
            BLURB,
            "blurb key missing from zh-TW.yml"
        );

        rust_i18n::set_locale("it");
        assert_eq!(rust_i18n::t!("Settings"), "Impostazioni");
        assert_eq!(rust_i18n::t!("Left Click"), "Click sinistro");
        assert_eq!(rust_i18n::t!("Cancel"), "Annulla");

        // English is the Crowdin source locale.
        rust_i18n::set_locale("en");
        assert_eq!(rust_i18n::t!("Settings"), "Settings");
        assert_eq!(rust_i18n::t!(BLURB), BLURB);
    }

    #[test]
    fn locale_files_have_the_same_keys() {
        let source = locale_keys(include_str!("../locales/en.yml"));
        for (locale, file) in [
            ("ja", include_str!("../locales/ja.yml")),
            ("ru", include_str!("../locales/ru.yml")),
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
        ] {
            let keys = locale_keys(file);
            assert_eq!(keys, source, "{locale}.yml keys drifted from en.yml");
        }
    }

    fn locale_keys(file: &str) -> Vec<&str> {
        file.lines()
            .filter_map(|line| line.strip_prefix('"'))
            .filter_map(|line| line.split_once("\": ").map(|(key, _)| key))
            .collect()
    }
}
