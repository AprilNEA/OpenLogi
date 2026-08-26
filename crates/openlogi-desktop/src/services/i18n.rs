//! Settings-app localization.
//!
//! Translations live in `crates/openlogi-ui/locales/*.yml` and are loaded at
//! compile time by the `rust_i18n::i18n!` macro in `main.rs` (fallback `"en"`).
//! **`en.yml` is the English source of truth** (the English text IS the key).
//! New or changed copy must land in **every** `locales/*.yml` in the same
//! change — `openlogi_core::locale`'s parity test enforces key-for-key match,
//! against the catalogs it now sits beside. Crowdin improves
//! non-English values over time and the workflow downloads only real
//! translations (`skip_untranslated_strings`). Call sites use
//! [`tr!`](crate::tr) / `rust_i18n::t!` with the **English string as the key**.
//! Missing keys fall back to English at runtime, but catalogs must not lag.
//!
//! The current locale is a process-global atomic inside `rust_i18n`. Setting it
//! re-localizes both our own call sites *and* gpui-component's built-in widget
//! strings, since the framework reads the same global. Apply it once at startup
//! via [`apply`] and on a live switch via
//! [`AppState::set_language`](crate::state::AppState::set_language); each must be
//! followed by a window refresh so open views re-render with the new locale.
//!
//! Which catalog a BCP-47 code resolves to is decided in
//! [`openlogi_core::locale`], shared with the overlay helper.

use openlogi_core::config::AppSettings;
use openlogi_core::locale::activate;

/// Serializes tests that mutate `rust_i18n`'s process-global locale, so a
/// locale switch in one test cannot interleave with another's assertions.
#[cfg(test)]
pub(crate) static LOCALE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Apply the configured language to the process-global locale at startup.
/// Safe to call before any window opens — the locale is a plain atomic.
pub fn apply(settings: &AppSettings) {
    activate(settings.language.as_deref());
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Action, ButtonId, GestureDirection};

    use crate::features::mouse::thumbwheel::ThumbwheelPreset;

    /// End-to-end check that `locales/*.yml` loaded and the gettext-style
    /// English keys match — a typo'd key silently falls back to English, which
    /// this catches. All locale-dependent assertions live in this one test on
    /// purpose: `rust_i18n`'s locale is a process-global, so splitting them into
    /// separate `#[test]`s would race under the parallel harness. Tests that
    /// must switch the locale for other reasons hold [`super::LOCALE_LOCK`].
    #[test]
    fn locale_file_resolves_keys() {
        // The accessibility blurb is the longest, most typo-prone key.
        const BLURB: &str = "OpenLogi captures mouse buttons (Back / Forward / gesture button) through the system Accessibility permission and runs the actions you bind. Features that talk to the device directly — DPI, SmartShift — are unaffected.";

        // A poisoned lock still holds the guard inside the `Err`, so a panic
        // in one locale test cannot unlock the others.
        let _locale = super::LOCALE_LOCK.lock();

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
        assert_eq!(rust_i18n::t!("Hold %{chord}", chord => "X"), "按住 X"); // held action label
        assert_eq!(rust_i18n::t!("Quit OpenLogi"), "退出 OpenLogi"); // menu-bar status item
        assert_eq!(rust_i18n::t!("No devices connected"), "未连接设备"); // menu-bar device line
        assert_eq!(rust_i18n::t!("Lighting"), "灯光"); // keyboard lighting tab
        assert_eq!(rust_i18n::t!("Brightness"), "亮度"); // lighting panel label
        assert_eq!(
            rust_i18n::t!("Automatically start OpenLogi when you log in to macOS."),
            "登录 macOS 时自动启动 OpenLogi。"
        );
        assert_eq!(
            rust_i18n::t!("No supported pairing-capable receiver was found."),
            "未找到支持配对的接收器。"
        );
        assert_eq!(
            rust_i18n::t!("Device offline — DPI unavailable."),
            "设备离线 —— DPI 不可用。"
        );
        assert_eq!(
            rust_i18n::t!("This device does not report native HID++ scroll inversion support."),
            "此设备未报告原生 HID++ 滚动反转支持。"
        );
        assert_ne!(
            rust_i18n::t!(BLURB),
            BLURB,
            "blurb key missing from zh-CN.yml"
        );

        // Exhaustive: every non-parameterized device/action label has a `zh-CN`
        // entry. Parameterized `Action`s (`SetDpiPreset`, `CustomShortcut`,
        // `HoldShortcut`) are skipped here and checked explicitly above where
        // needed.
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

        // Thumb-wheel preset labels are flat full-phrase keys ("Back /
        // Forward", not a composition of the two action names) with reviewed
        // translations in every catalog. #910 replaced them with per-action
        // composition on the wrong belief that these keys were untranslated;
        // these assertions pin both the coverage and the full-phrase wording
        // so that diagnosis cannot recur.
        for preset in ThumbwheelPreset::ALL {
            assert!(covered(preset.label()), "no zh-CN for {preset:?}");
        }
        assert_eq!(
            rust_i18n::t!(ThumbwheelPreset::BackForward.label()),
            "后退 / 前进"
        );
        assert_eq!(
            rust_i18n::t!(ThumbwheelPreset::VerticalScroll.label()),
            "垂直滚动"
        );

        rust_i18n::set_locale("ja");
        assert_eq!(rust_i18n::t!("Settings"), "設定");
        assert_eq!(rust_i18n::t!("Left Click"), "左クリック");

        rust_i18n::set_locale("ru");
        assert_eq!(rust_i18n::t!("Settings"), "Настройки");
        assert_eq!(rust_i18n::t!("Left Click"), "Левый щелчок");

        rust_i18n::set_locale("uk");
        assert_eq!(rust_i18n::t!("Settings"), "Налаштування");
        assert_eq!(rust_i18n::t!("Left Click"), "Клацання лівою");

        rust_i18n::set_locale("zh-TW");
        assert_eq!(rust_i18n::t!("Settings"), "設定");
        assert_eq!(rust_i18n::t!("Left Click"), "左鍵按一下");
        assert_eq!(rust_i18n::t!("Bind %{name}", name => "X"), "設定 X");
        assert_eq!(
            rust_i18n::t!("No supported pairing-capable receiver was found."),
            "找不到支援配對的接收器。"
        );
        assert_eq!(
            rust_i18n::t!("Device offline — DPI unavailable."),
            "裝置離線 —— DPI 無法使用。"
        );
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
}
