//! Binding-map construction: overlay the stored per-device (and per-app)
//! bindings on top of the built-in defaults.
//!
//! Keyed by `config_key` (`Option<&str>`) rather than any UI device record so
//! both the agent and the GUI can build the effective map from a [`Config`].

use std::collections::BTreeMap;

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_gesture_binding,
};
use openlogi_core::config::Config;

/// Effective per-button single-action map for the device `config_key`, with
/// `app_bundle`'s per-app overlay applied. Unset buttons fall back to
/// [`default_binding`].
///
/// This is the map the OS hook and the HID++ button-press path consume, so a
/// `Binding::Gesture` is projected to its `click_action()` — the gesture
/// button's per-direction swipes are dispatched via the separate
/// [`gesture_bindings_for`] map, not here.
#[must_use]
pub fn bindings_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, Action> {
    let stored = config_key
        .map(|key| config.effective_bindings(key, app_bundle))
        .unwrap_or_default();
    let mut bindings: BTreeMap<ButtonId, Action> = ButtonId::ALL
        .iter()
        .copied()
        .map(|b| (b, default_binding(b)))
        .collect();
    for (k, binding) in stored {
        // A gesture binding with no explicit `Click` has no opinion on the
        // plain-press action, so leave the button's default seed in place rather
        // than clobbering it with the `Action::None` that `click_action()` would
        // project. (An explicit `Single(Action::None)` — a user-disabled button —
        // still overrides, as it should.)
        if binding.is_gesture() && binding.direction_action(GestureDirection::Click).is_none() {
            continue;
        }
        bindings.insert(k, binding.click_action());
    }
    bindings
}

/// Effective gesture bindings for the device `config_key`. Unset directions
/// fall back to [`default_gesture_binding`].
#[must_use]
pub fn gesture_bindings_for(
    config: &Config,
    config_key: Option<&str>,
) -> BTreeMap<GestureDirection, Action> {
    // The thumb pad (HID++ 0x00c3) only gestures while it is the device's gesture
    // owner. When the user moves the role to an OS-hook button (Middle/Back/
    // Forward) or turns gestures off, return an empty map so the gesture watcher
    // dispatches nothing — otherwise the always-seeded defaults would keep the
    // thumb pad firing regardless of the selection.
    let owner = config_key.and_then(|key| config.gesture_owner(key));
    if owner != Some(ButtonId::GestureButton) {
        return BTreeMap::new();
    }
    let stored = config_key
        .map(|key| config.gesture_bindings_for(key))
        .unwrap_or_default();
    let mut bindings: BTreeMap<GestureDirection, Action> = GestureDirection::ALL
        .iter()
        .copied()
        .map(|d| (d, default_gesture_binding(d)))
        .collect();
    for (k, v) in stored {
        bindings.insert(k, v);
    }
    bindings
}

/// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
/// gesture mode) on `config_key`, with `app_bundle`'s per-app overlay applied,
/// for the OS hook to resolve a hold+swipe.
///
/// Unlike [`gesture_bindings_for`] (the dedicated HID++ gesture button, which
/// seeds every direction from [`default_gesture_binding`]), these are the raw
/// stored maps — a swipe direction the user left unbound simply does nothing.
/// The dedicated gesture button is intentionally excluded: it never reaches the
/// OS hook (it's captured over HID++), so it has no entry here.
///
/// A per-app override of the owner button turns it into a [`Binding::Single`]
/// for that app, so it stops being a gesture button there and falls through to
/// the single-action path (which applies the override) — mirroring how a single
/// binding is overridden per app.
#[must_use]
pub fn oshook_gestures_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
    let Some(key) = config_key else {
        return BTreeMap::new();
    };
    // Only an OS-hook button (Middle/Back/Forward) as the device's gesture owner
    // feeds the OS hook: the thumb pad is captured over HID++, and a non-owner
    // button is dispatched as its single click action. Returning *only* the owner
    // keeps the runtime in lockstep with `gesture_owner` and the GUI, so a stray
    // second gesture map (e.g. a hand-edited config) can't make two buttons fire.
    let Some(owner) = config
        .gesture_owner(key)
        .filter(|id| id.is_os_hook_button())
    else {
        return BTreeMap::new();
    };
    // Read the per-app *effective* map: a per-app override replaces the owner with
    // a `Single`, dropping it from the gesture set for that app.
    match config.effective_bindings(key, app_bundle).remove(&owner) {
        Some(Binding::Gesture(map)) => BTreeMap::from([(owner, map)]),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_less_gesture_keeps_default_click_in_projection() {
        // A gesture binding with no explicit `Click` (a migrated sparse v1 map or
        // a hand-edited config) must not project to `Action::None` and silently
        // disable the button — the button's default click survives.
        let mut cfg = Config::default();
        let mut map = BTreeMap::new();
        map.insert(GestureDirection::Up, Action::Copy);
        cfg.set_binding("2b042", ButtonId::GestureButton, Binding::Gesture(map));

        let projected = bindings_for(&cfg, Some("2b042"), None);
        assert_eq!(
            projected.get(&ButtonId::GestureButton),
            Some(&default_binding(ButtonId::GestureButton)),
            "a Click-less gesture must keep the default click, not None"
        );
    }

    #[test]
    fn explicit_gesture_click_overrides_default_in_projection() {
        // A gesture binding that DOES define `Click` projects that action.
        let mut cfg = Config::default();
        let mut map = BTreeMap::new();
        map.insert(GestureDirection::Click, Action::Paste);
        cfg.set_binding("2b042", ButtonId::GestureButton, Binding::Gesture(map));

        let projected = bindings_for(&cfg, Some("2b042"), None);
        assert_eq!(
            projected.get(&ButtonId::GestureButton),
            Some(&Action::Paste)
        );
    }

    #[test]
    fn oshook_gestures_collects_only_os_hook_gesture_buttons() {
        let mut cfg = Config::default();
        // A gesture-mode Back (an OS-hook button) — included, raw map preserved.
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([(GestureDirection::Up, Action::Copy)])),
        );
        // A single-mode Middle — excluded (not a gesture button).
        cfg.set_binding("2b042", ButtonId::MiddleClick, Action::MiddleClick.into());
        // The dedicated HID++ gesture button — excluded (it never reaches the
        // OS hook, so it must not appear in the hook's gesture map).
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Up,
                Action::MissionControl,
            )])),
        );

        let oshook = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert_eq!(oshook.len(), 1, "only the gesture-mode Back belongs here");
        assert_eq!(
            oshook.get(&ButtonId::Back),
            Some(&BTreeMap::from([(GestureDirection::Up, Action::Copy)]))
        );
        assert!(!oshook.contains_key(&ButtonId::MiddleClick));
        assert!(!oshook.contains_key(&ButtonId::GestureButton));
    }

    #[test]
    fn per_app_override_drops_the_owner_from_the_oshook_gesture_set() {
        // Back is the gesture owner globally...
        let mut cfg = Config::default();
        cfg.set_gesture_owner("2b042", ButtonId::Back);
        assert!(
            oshook_gestures_for(&cfg, Some("2b042"), None).contains_key(&ButtonId::Back),
            "Back gestures globally"
        );

        // ...but a per-app override makes it a single action in that app, so it
        // must drop out of the gesture set there (and fall through to the
        // single-action path, which applies the override).
        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::Back,
            Some(Action::NextTab),
        );
        assert!(
            oshook_gestures_for(&cfg, Some("2b042"), Some("com.apple.Safari")).is_empty(),
            "a per-app override of the owner removes it from the gesture set"
        );
        // Other apps are unaffected — Back still gestures.
        assert!(
            oshook_gestures_for(&cfg, Some("2b042"), Some("com.other.App"))
                .contains_key(&ButtonId::Back)
        );
    }

    #[test]
    fn gesture_bindings_silent_when_thumb_pad_is_not_the_owner() {
        let mut cfg = Config::default();
        // Default device: the thumb pad owns gestures, so its defaults are seeded.
        let defaults = gesture_bindings_for(&cfg, Some("2b042"));
        assert_eq!(
            defaults.get(&GestureDirection::Up),
            Some(&default_gesture_binding(GestureDirection::Up)),
            "the default gesture owner is the thumb pad"
        );

        // Move the gesture role to an OS-hook button: the thumb pad goes silent,
        // so the watcher dispatches nothing for 0x00c3.
        cfg.set_gesture_owner("2b042", ButtonId::Back);
        assert!(
            gesture_bindings_for(&cfg, Some("2b042")).is_empty(),
            "thumb pad must dispatch nothing once another button owns gestures"
        );
    }
}
