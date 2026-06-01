//! UI localization plumbing.
//!
//! Translations live in `crates/openlogi-gui/locales/app.yml` and are loaded
//! at compile time by the `rust_i18n::i18n!` macro in `main.rs`. Call sites use
//! the [`tr!`](crate::tr) helper (or `rust_i18n::t!`) with the **English string
//! as the key** — a missing entry falls back to that English text, so the file
//! carries only the translated `ja` / `ru` / `zh-CN` / `zh-HK` columns;
//! English is the key itself.
//!
//! The current locale is a process-global atomic inside `rust_i18n`. Setting it
//! re-localizes both our own call sites *and* gpui-component's built-in widget
//! strings, since the framework reads the same global. Apply it once at startup
//! via [`apply`] and on a live switch via
//! [`AppState::set_language`](crate::state::AppState::set_language); each must be
//! followed by a window refresh so open views re-render with the new locale.

use openlogi_core::config::AppSettings;

/// Locales the GUI ships, as `(code, native name)`. The codes match the
/// sub-keys in `locales/app.yml`; `en` / `zh-CN` / `zh-HK` also match
/// gpui-component's bundled `ui.yml`, so choosing one localizes the framework's
/// own widgets too. `ja` and `ru` are *not* in `ui.yml`, so under Japanese or
/// Russian our app strings localize but the framework's built-in widget strings
/// fall back to English.
/// Order here is the order shown in the Settings picker (after "Follow system").
pub const SUPPORTED: &[(&str, &str)] = &[
    ("en", "English"),
    ("ja", "日本語"),
    ("ru", "Русский"),
    ("zh-CN", "简体中文"),
    ("zh-HK", "繁體中文"),
    ("it", "Italiano"),
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
/// by matching its primary subtag. A `zh` tag is decided by its *first*
/// script-or-region subtag (script precedes region in BCP-47): `Hant` or a
/// Traditional-using region (`tw` / `hk` / `mo`) → `zh-HK`; an explicit `Hans`
/// or no such subtag → `zh-CN`. So `zh-Hans-HK` stays Simplified — the script
/// tag wins over the region.
fn match_supported(code: &str) -> Option<&'static str> {
    let lower = code.to_ascii_lowercase();
    let mut subtags = lower.split(['-', '_']);
    match subtags.next() {
        Some("en") => Some("en"),
        Some("ja") => Some("ja"),
        Some("ru") => Some("ru"),
        Some("it") => Some("it"),
        Some("zh") => {
            let traditional = matches!(
                subtags.find(|t| matches!(*t, "hans" | "hant" | "tw" | "hk" | "mo")),
                Some("hant" | "tw" | "hk" | "mo")
            );
            Some(if traditional { "zh-HK" } else { "zh-CN" })
        }
        _ => None,
    }
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
        assert_eq!(match_supported("zh-Hant-TW"), Some("zh-HK"));
        assert_eq!(match_supported("zh-HK"), Some("zh-HK"));
        assert_eq!(match_supported("ja"), Some("ja"));
        assert_eq!(match_supported("ja-JP"), Some("ja"));
        assert_eq!(match_supported("ru"), Some("ru"));
        assert_eq!(match_supported("ru-RU"), Some("ru"));
        assert_eq!(match_supported("en-US"), Some("en"));
        assert_eq!(match_supported("fr-FR"), None);
        assert_eq!(match_supported("it"), Some("it"));
        assert_eq!(match_supported("it-IT"), Some("it"));
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

    /// End-to-end check that `locales/app.yml` loaded and the gettext-style
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
        assert_eq!(rust_i18n::t!("Bind %{name}", name => "X"), "绑定 X"); // interpolation
        assert_eq!(rust_i18n::t!("Quit OpenLogi"), "退出 OpenLogi"); // menu-bar status item
        assert_eq!(rust_i18n::t!("No device connected"), "未连接设备"); // menu-bar device line
        assert_ne!(
            rust_i18n::t!(BLURB),
            BLURB,
            "blurb key missing from app.yml"
        );

        // Exhaustive: every non-parameterized device/action label has a `zh-CN`
        // entry. `DPI` is a universal initialism, identical across locales — it
        // has no entry, so it's the one exception to "differs from English".
        // Parameterized `Action`s (`SetDpiPreset`, `CustomShortcut`) are
        // English-only by design and absent from the catalog, so they're skipped.
        let covered = |label: &str| label == "DPI" || rust_i18n::t!(label) != label;
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

        rust_i18n::set_locale("it");
        assert_eq!(rust_i18n::t!("Settings"), "Impostazioni");
        assert_eq!(rust_i18n::t!("Left Click"), "Click sinistro");
        assert_eq!(rust_i18n::t!("Cancel"), "Annulla");

        // English has no column: every key falls back to the English source.
        rust_i18n::set_locale("en");
        assert_eq!(rust_i18n::t!("Settings"), "Settings");
        assert_eq!(rust_i18n::t!(BLURB), BLURB);
    }
}
