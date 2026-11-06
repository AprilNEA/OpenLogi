use openlogi_core::binding::{
    Action, ActionRingIcon, ButtonId, GestureDirection, GridKey, KNOWN_CONTROLS, KeyCombo,
};

use crate::features::mouse::thumbwheel::ThumbwheelPreset;
use crate::ui::action::localized_action_label;

/// Typed dynamic keys are the part catalog parity cannot inspect: a typo in
/// one of these methods compiles and renders the key at runtime. Resolve every
/// typed key against the English catalog, which also proves this binary loaded
/// the shared catalog at the expected relative path.
#[test]
fn typed_translation_keys_resolve() {
    let _locale = super::LOCALE_LOCK.lock();
    rust_i18n::set_locale("en");
    let covered = |key: &str| rust_i18n::t!(key) != key;
    assert!(covered("app.settings"), "desktop catalog is not wired up");

    for b in ButtonId::ALL
        .into_iter()
        .chain(KNOWN_CONTROLS.iter().map(|control| ButtonId::Control(control.id)))
        // A control with no catalog row resolves to the one generic key.
        .chain([ButtonId::control(0xffff)])
    {
        assert!(
            covered(b.translation_key()),
            "no catalog key for ButtonId::{b:?}"
        );
    }
    // Grid keys bind from config.toml before any GUI list exists; their
    // labels must still resolve the day a picker offers them.
    for key in GridKey::ALL {
        assert!(
            covered(key.translation_key()),
            "no catalog key for GridKey::{key:?}"
        );
    }
    for d in GestureDirection::ALL {
        assert!(
            covered(d.translation_key()),
            "no catalog key for GestureDirection::{d:?}"
        );
    }
    for a in Action::catalog() {
        let key = a
            .translation_key()
            .expect("every catalog action is payload-free");
        assert!(covered(key), "no catalog key for Action::{a:?}");
        assert!(
            covered(a.category().translation_key()),
            "no catalog key for {:?}",
            a.category()
        );
    }
    for icon in ActionRingIcon::ALL {
        assert!(
            covered(icon.translation_key()),
            "no catalog key for {icon:?}"
        );
    }
    for preset in ThumbwheelPreset::ALL {
        assert!(
            covered(preset.translation_key()),
            "no catalog key for {preset:?}"
        );
    }
}

/// Recorded chord actions carry a payload, so the typed-key sweep above cannot
/// reach their keys. Render the real labels in a translated locale instead: a
/// mistyped key or placeholder name would leave the key or a raw `%{…}` behind.
#[test]
fn chord_action_labels_resolve() {
    let _locale = super::LOCALE_LOCK.lock();
    let chord = |text: &str| -> KeyCombo { text.parse().expect("test shortcut must be valid") };
    let label = |action: Action| localized_action_label(&action).to_string();

    rust_i18n::set_locale("zh-CN");
    let hold = label(Action::HoldShortcut(chord("Cmd+Tab")));
    let switcher = label(Action::TapKeyHoldingModifiers(chord("Cmd+Tab")));
    let bare_key = label(Action::TapKeyHoldingModifiers(chord("F5")));
    rust_i18n::set_locale("en");

    assert_eq!(hold, "按住 Cmd+Tab");
    assert_eq!(switcher, "按住 Cmd，点按 Tab");
    assert_eq!(bare_key, "点按 F5");
}

/// Czech must resolve in the actual desktop backend, including the pairing
/// entry point and the authentication instructions.
#[test]
fn czech_pairing_translations_resolve() {
    assert_eq!(
        rust_i18n::t!("pairing.add_device", locale = "cs"),
        "Přidat zařízení"
    );
    assert_eq!(
        rust_i18n::t!(
            "pairing.keyboard_pairing_passkey_instructions",
            locale = "cs"
        ),
        "Na nové klávesnici napište tento kód a stiskněte Enter."
    );
    assert_eq!(
        rust_i18n::t!("pairing.paired_receiver_slot", locale = "cs", slot = 2),
        "Spárováno na pozici 2."
    );
}

/// Switching the configured locale must translate both material controls and
/// their descriptions through this binary's live catalog, without fallback.
#[test]
fn glass_controls_follow_live_locale_changes() {
    struct RestoreLocale(String);

    impl Drop for RestoreLocale {
        fn drop(&mut self) {
            rust_i18n::set_locale(&self.0);
        }
    }

    let _locale = super::LOCALE_LOCK.lock().expect("locale lock");
    let _restore = RestoreLocale(rust_i18n::locale().to_string());
    let keys = [
        "appearance.acrylic_glass",
        "appearance.acrylic_glass_description",
        "appearance.glass_opacity",
        "appearance.glass_opacity_description",
        "appearance.window",
        "appearance.reset_opacity",
        "appearance.reset_opacity_description",
    ];
    let english = keys.map(|key| rust_i18n::t!(key, locale = "en").to_string());

    for (language, switch_label, slider_label) in [
        ("zh-CN", "亚克力玻璃", "玻璃不透明度"),
        ("fr", "Verre acrylique", "Opacité du verre"),
        ("de", "Acrylglas", "Glasdeckkraft"),
        ("ja", "アクリルガラス", "ガラスの不透明度"),
        ("zh-HK", "亞加力玻璃", "玻璃不透明度"),
        ("zh-TW", "壓克力玻璃", "玻璃不透明度"),
        ("zh-CN", "亚克力玻璃", "玻璃不透明度"),
    ] {
        super::apply(&openlogi_core::config::AppSettings {
            language: Some(language.to_owned()),
            ..Default::default()
        });
        assert_eq!(rust_i18n::t!("appearance.acrylic_glass"), switch_label);
        assert_eq!(rust_i18n::t!("appearance.glass_opacity"), slider_label);
        let opacity = openlogi_core::config::AppSettings::default().acrylic_opacity();
        let reset_description =
            rust_i18n::t!("appearance.reset_opacity_description", opacity = opacity);
        assert!(reset_description.contains(&opacity.to_string()));
        assert!(!reset_description.contains("%{"));

        for (key, fallback) in keys.into_iter().zip(&english) {
            let translated = rust_i18n::t!(key).to_string();
            assert!(!translated.is_empty(), "{language}: {key} is empty");
            assert_ne!(translated, key, "{language}: unresolved key {key}");
            assert_ne!(
                &translated, fallback,
                "{language}: English fallback for {key}"
            );
        }
    }
}
