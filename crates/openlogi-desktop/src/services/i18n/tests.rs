use openlogi_core::binding::{
    Action, ActionRingIcon, AdvancedClick, ButtonId, ClickButton, ClickCount, GestureDirection,
    KNOWN_CONTROLS, KeyCombo,
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

/// Advanced clicks carry a payload too, and name their click through a key of
/// their own for every button and count.
#[test]
fn advanced_click_labels_resolve() {
    let _locale = super::LOCALE_LOCK.lock();
    let label = |button, count, modifiers: &str| {
        localized_action_label(&Action::AdvancedClick(AdvancedClick {
            button,
            count,
            modifiers: modifiers.parse().expect("test modifiers must be valid"),
        }))
        .to_string()
    };

    rust_i18n::set_locale("en");
    for button in [ClickButton::Left, ClickButton::Right, ClickButton::Middle] {
        for count in [ClickCount::Single, ClickCount::Double, ClickCount::Triple] {
            let click = AdvancedClick {
                button,
                count,
                ..AdvancedClick::default()
            };
            assert_eq!(
                label(button, count, ""),
                click.click_label(),
                "{button:?} {count:?}"
            );
        }
    }
    rust_i18n::set_locale("zh-CN");
    let shift_right_double = label(ClickButton::Right, ClickCount::Double, "Shift");
    let cmd_alt_triple = label(ClickButton::Left, ClickCount::Triple, "Alt+Cmd");
    rust_i18n::set_locale("en");

    assert_eq!(shift_right_double, "Shift+右键双击");
    assert_eq!(cmd_alt_triple, "Cmd+Alt+三击");
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
