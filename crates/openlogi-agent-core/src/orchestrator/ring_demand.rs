//! Whether a device can open the Actions Ring — the question that decides
//! whether the agent starts the overlay ahead of its first ring.

use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::bindings::button_bindings_for;
use openlogi_core::config::Config;
use openlogi_core::device::Capabilities;

/// Whether `key` opens its ring from any profile: its global bindings over
/// their defaults, or any application's overrides.
///
/// The haptic panel counts only on a device that has one. Its default is the
/// ring on every device, so without the check every mouse and keyboard would
/// keep the overlay warm.
pub(super) fn opens_ring(config: &Config, key: &str, capabilities: Option<Capabilities>) -> bool {
    if !config.device_enabled(key) || !config.action_ring(key).enabled {
        return false;
    }
    let has_panel = capabilities.is_some_and(|capabilities| capabilities.haptic_panel);
    let present = |button: ButtonId| has_panel || button != ButtonId::HapticPanel;
    let global = button_bindings_for(config, Some(key), None)
        .into_iter()
        .any(|(button, binding)| present(button) && binding_opens_ring(&binding));
    global
        || config.app_profiles(key).any(|app| {
            config.per_app_overrides(key, app).is_some_and(|overrides| {
                overrides
                    .iter()
                    .any(|(button, action)| present(*button) && is_ring(action))
            })
        })
}

fn binding_opens_ring(binding: &Binding) -> bool {
    match binding {
        Binding::Single(action) => is_ring(action),
        Binding::Gesture(directions) => directions.values().any(is_ring),
        Binding::LongPress(press) => is_ring(press.short()) || is_ring(press.long()),
        Binding::WheelHold(hold) => is_ring(hold.click()),
    }
}

const fn is_ring(action: &Action) -> bool {
    matches!(action, Action::ShowActionsRing)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use openlogi_core::binding::{GestureDirection, LongPressBinding, WheelHoldBinding};

    use super::*;

    const KEY: &str = "mouse";

    fn panel() -> Capabilities {
        Capabilities {
            buttons: true,
            haptic_panel: true,
            ..Capabilities::default()
        }
    }

    fn no_panel() -> Capabilities {
        Capabilities {
            buttons: true,
            ..Capabilities::default()
        }
    }

    #[test]
    fn the_haptic_panel_opens_the_ring_by_default() {
        assert!(opens_ring(&Config::default(), KEY, Some(panel())));
    }

    #[test]
    fn a_device_without_a_panel_does_not_inherit_the_panel_default() {
        let config = Config::default();
        assert!(!opens_ring(&config, KEY, Some(no_panel())));
        assert!(
            !opens_ring(&config, KEY, None),
            "an unprobed device is not presumed to have a panel"
        );
    }

    #[test]
    fn a_panel_bound_elsewhere_does_not_open_the_ring() {
        let mut config = Config::default();
        config.set_binding(KEY, ButtonId::HapticPanel, Binding::Single(Action::Copy));
        assert!(!opens_ring(&config, KEY, Some(panel())));
    }

    #[test]
    fn a_ring_bound_to_any_button_in_any_shape_opens_it() {
        let ring = Action::ShowActionsRing;
        let shapes = [
            Binding::Single(ring.clone()),
            Binding::LongPress(LongPressBinding::new(Action::Copy, ring.clone())),
            Binding::WheelHold(WheelHoldBinding::horizontal_scroll(ring.clone())),
            Binding::Gesture(BTreeMap::from([
                (GestureDirection::Click, Action::Copy),
                (GestureDirection::Up, ring),
            ])),
        ];
        for binding in shapes {
            let mut config = Config::default();
            config.set_binding(KEY, ButtonId::Back, binding.clone());
            assert!(opens_ring(&config, KEY, Some(no_panel())), "{binding:?}");
        }
    }

    #[test]
    fn a_per_app_override_opens_the_ring() {
        let mut config = Config::default();
        config.set_per_app_binding(
            KEY,
            "exe:code.exe",
            ButtonId::Forward,
            Some(Action::ShowActionsRing),
        );
        assert!(opens_ring(&config, KEY, Some(no_panel())));
    }

    #[test]
    fn a_disabled_ring_or_device_is_never_warmed_for() {
        let mut ring_off = Config::default();
        ring_off.set_action_ring_enabled(KEY, false);
        assert!(!opens_ring(&ring_off, KEY, Some(panel())));

        let mut device_off = Config::default();
        device_off.set_device_enabled(KEY, false);
        assert!(!opens_ring(&device_off, KEY, Some(panel())));
    }
}
