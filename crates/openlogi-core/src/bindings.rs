//! Binding-map construction: overlay the stored per-device (and per-app)
//! bindings on top of the built-in defaults.
//!
//! Keyed by `config_key` (`Option<&str>`) rather than any UI device record so
//! both the agent and the GUI can build the effective map from a [`Config`].

use std::collections::BTreeMap;

use crate::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_binding_for,
};
use crate::config::Config;

/// Effective per-button single-action map for the device `config_key`, with
/// `app_bundle`'s per-app overlay applied. Unset buttons fall back to
/// [`default_binding`].
///
/// This is the map the OS hook and the HID++ button-press path consume, so a
/// `Binding::Gesture` is projected to its `click_action()` — a gesture-mode
/// button's per-direction swipes are dispatched via the separate
/// [`hidpp_gesture_maps_for`] / [`oshook_gestures_for`] maps, not here.
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

/// Per-direction maps for every HID++ gesture source (the dedicated gesture
/// button, the MX Master 4 haptic panel) in gesture mode on `config_key`,
/// keyed by the button its captured swipes dispatch as. Each map is seeded
/// via [`Binding::fill_gesture_defaults`] — the one canonical seeding rule —
/// so the watcher always dispatches the full five-direction set the GUI
/// shows. Empty when no HID++ source gestures (or `config_key` is `None`).
#[must_use]
pub fn hidpp_gesture_maps_for(
    config: &Config,
    config_key: Option<&str>,
) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
    let Some(key) = config_key else {
        return BTreeMap::new();
    };
    let stored = config.bindings_for(key);
    ButtonId::ALL
        .iter()
        .copied()
        .filter(|button| button.is_hidpp_gesture_source())
        .filter_map(|button| {
            // The stored shape (or the button's canonical default) IS gesture
            // mode — a Single-shaped source simply drops out.
            let mut binding = stored
                .get(&button)
                .cloned()
                .unwrap_or_else(|| default_binding_for(button));
            binding.fill_gesture_defaults();
            match binding {
                Binding::Gesture(map) => Some((button, map)),
                Binding::Single(_) => None,
            }
        })
        .collect()
}

/// Per-direction maps for every OS-hook button (Middle/Back/Forward) in
/// gesture mode on `config_key`, with `app_bundle`'s per-app overlay applied,
/// for the OS hook to resolve a hold+swipe. Gesture mode is per-button (see
/// [`Config::is_gesture_mode`]), so any number of entries may be live at once —
/// concurrency between them is the hook's first-hold-wins policy, not a config
/// concern.
///
/// Unlike [`hidpp_gesture_maps_for`] (whose maps seed every direction at
/// projection time), this returns each button's raw stored map. In practice
/// those maps are
/// already fully populated — [`Config::set_gesture_mode`] seeds all five
/// directions via [`Binding::fill_gesture_defaults`] when a button is
/// promoted — so only a hand-edited sparse map leaves a direction unbound, in
/// which case that swipe simply does nothing. The dedicated gesture button is
/// intentionally excluded: it never reaches the OS hook (it's captured over
/// HID++), so it has no entry here.
///
/// A per-app override of a gesture button turns it into a [`Binding::Single`]
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
    // Read the per-app *effective* map: a per-app override replaces a gesture
    // button with a `Single`, dropping it from the gesture set for that app.
    config
        .effective_bindings(key, app_bundle)
        .into_iter()
        .filter(|(id, _)| id.is_os_hook_button())
        .filter_map(|(id, binding)| match binding {
            Binding::Gesture(map) => Some((id, map)),
            Binding::Single(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::binding::default_gesture_binding;

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
    fn oshook_gestures_includes_every_gesture_mode_button() {
        // The owner lock is gone: every OS-hook button in gesture mode
        // dispatches, each through its own direction map.
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::Back, true);
        cfg.set_gesture_mode("2b042", ButtonId::MiddleClick, true);

        let oshook = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert!(oshook.contains_key(&ButtonId::Back), "got: {oshook:?}");
        assert!(
            oshook.contains_key(&ButtonId::MiddleClick),
            "got: {oshook:?}"
        );
    }

    #[test]
    fn hidpp_gesture_maps_includes_every_gesture_mode_source() {
        // Both HID++ sources in gesture mode dispatch simultaneously, each
        // through its own seeded direction map.
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::HapticPanel, true);

        let maps = hidpp_gesture_maps_for(&cfg, Some("2b042"));
        // The dedicated button gestures by default...
        let dedicated = maps
            .get(&ButtonId::GestureButton)
            .expect("the dedicated button's default gesture mode must survive");
        assert_eq!(
            dedicated.get(&GestureDirection::Up),
            Some(&default_gesture_binding(GestureDirection::Up))
        );
        // ...and the panel's promotion adds a second, fully-seeded map.
        let panel = maps
            .get(&ButtonId::HapticPanel)
            .expect("a gesture-mode panel must dispatch");
        for dir in GestureDirection::ALL {
            assert!(panel.contains_key(&dir), "unseeded panel arm {dir:?}");
        }
    }

    #[test]
    fn per_app_override_drops_the_owner_from_the_oshook_gesture_set() {
        // Back is the gesture owner globally...
        let mut cfg = Config::default();
        cfg.set_gesture_mode("2b042", ButtonId::Back, true);
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
    fn hidpp_maps_silent_for_a_demoted_dedicated_button() {
        // Default device: the dedicated HID++ gesture button gestures, with its
        // defaults seeded.
        let mut cfg = Config::default();
        let maps = hidpp_gesture_maps_for(&cfg, Some("2b042"));
        assert_eq!(
            maps.get(&ButtonId::GestureButton)
                .and_then(|m| m.get(&GestureDirection::Up)),
            Some(&default_gesture_binding(GestureDirection::Up)),
            "the dedicated button gestures by default, seeded"
        );

        // Demoting it silences the watcher for 0x00c3 — and promoting an
        // OS-hook button never resurrects it.
        cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);
        cfg.set_gesture_mode("2b042", ButtonId::Back, true);
        assert!(
            hidpp_gesture_maps_for(&cfg, Some("2b042")).is_empty(),
            "a demoted dedicated button must dispatch nothing over HID++"
        );
    }
}
