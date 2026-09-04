use openlogi_core::binding::{Action, ActionRingIcon, ButtonId, GestureDirection};

use crate::features::mouse::thumbwheel::ThumbwheelPreset;

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
        .chain(ButtonId::KEYBOARD_KEYS)
        .chain(ButtonId::GAMING_BUTTONS)
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
