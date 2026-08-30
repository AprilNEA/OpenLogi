//! Hold-mode DPI resolution and arming policy for a capture plan.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, ButtonId, GestureDirection};
use openlogi_core::hid::Dpi;
use openlogi_hid::session::gesture::{DIVERTABLE_STANDARD_BUTTONS, GESTURE_SOURCE_BUTTONS};
use tracing::warn;

/// MX-class factory default (1000 CPI). Used only when HID++ `getSensorDpi`,
/// the configured cycle preset, and `config.dpi` are all missing. Hold-mode
/// still arms; the 2.5 mm deadzone is then approximate.
pub(crate) const FALLBACK_HOLD_SENSOR_DPI: Dpi = Dpi::new(1000);

/// Sensor DPI used to size the hold-mode deadzone, plus whether it is the
/// last-resort factory default rather than a live or configured reading.
pub(super) struct ResolvedHoldDpi {
    /// Counts-per-inch applied to the 2.5 mm deadzone.
    pub dpi: Dpi,
    /// True when both the live sensor and `config.dpi` were missing.
    pub used_fallback: bool,
}

/// Live sensor, then committed config DPI, then [`FALLBACK_HOLD_SENSOR_DPI`].
/// A missing reading must not disable hold-mode — it only degrades deadzone
/// accuracy.
pub(super) fn resolve_hold_sensor_dpi(
    live: Option<Dpi>,
    configured: Option<Dpi>,
) -> ResolvedHoldDpi {
    if let Some(dpi) = live.or(configured) {
        return ResolvedHoldDpi {
            dpi,
            used_fallback: false,
        };
    }
    ResolvedHoldDpi {
        dpi: FALLBACK_HOLD_SENSOR_DPI,
        used_fallback: true,
    }
}

/// Raw-XY hold CIDs when injection can deliver. Empty otherwise — the
/// caller then plain-diverts those buttons so firmware cannot keep scrolling.
pub(super) fn raw_xy_hold_diverts(
    hold_bindings: &BTreeMap<ButtonId, Action>,
    gesture_bindings: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    oshook: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    injection_available: bool,
) -> Vec<(u16, ButtonId)> {
    if !injection_available {
        return Vec::new();
    }
    DIVERTABLE_STANDARD_BUTTONS
        .into_iter()
        .chain(GESTURE_SOURCE_BUTTONS)
        .filter(|(_, button)| hold_bindings.contains_key(button))
        .filter(|(_, button)| !gesture_bindings.contains_key(button))
        .filter(|(_, button)| !oshook.contains_key(button))
        .collect()
}

/// Log when hold-mode is arming with [`FALLBACK_HOLD_SENSOR_DPI`].
pub(super) fn warn_if_hold_dpi_is_approximate(
    config_key: &str,
    hold_bound: bool,
    injection_available: bool,
    resolved: &ResolvedHoldDpi,
) {
    if resolved.used_fallback && hold_bound && injection_available {
        warn!(
            config_key,
            dpi = %resolved.dpi,
            "hold-mode sizing is approximate; sensor DPI unread"
        );
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Action, Binding, ButtonId};
    use openlogi_core::config::Config;
    use openlogi_core::device_order::PhysicalDeviceKey;
    use openlogi_core::hid::Dpi;
    use openlogi_hid::DeviceRoute;

    use super::*;
    use crate::capture_plan::{CaptureHostAbility, plan_for_device_with};

    fn route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "cafe".into(),
            slot: 2,
        }
    }

    fn plan_with(
        config: &Config,
        config_key: &str,
        ability: CaptureHostAbility,
    ) -> crate::capture_plan::DeviceCapturePlan {
        plan_for_device_with(
            config,
            PhysicalDeviceKey::parse("receiver:cafe:slot:2")
                .expect("fixture should be a physical key"),
            config_key,
            route(),
            None,
            0,
            ability,
        )
    }

    #[test]
    fn live_reading_wins_over_configured_and_fallback() {
        let resolved = resolve_hold_sensor_dpi(Some(Dpi::new(400)), Some(Dpi::new(1600)));
        assert_eq!(resolved.dpi, Dpi::new(400));
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn configured_dpi_wins_over_fallback() {
        let resolved = resolve_hold_sensor_dpi(None, Some(Dpi::new(800)));
        assert_eq!(resolved.dpi, Dpi::new(800));
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn missing_every_read_uses_the_named_fallback() {
        let resolved = resolve_hold_sensor_dpi(None, None);
        assert_eq!(resolved.dpi, FALLBACK_HOLD_SENSOR_DPI);
        assert!(resolved.used_fallback);
    }

    #[test]
    fn hold_mode_arms_with_fallback_dpi_when_every_read_is_missing() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Pan));

        let plan = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: true,
                sensor_dpi: None,
            },
        );
        assert_eq!(
            plan.target.spec.sensor_dpi,
            Some(FALLBACK_HOLD_SENSOR_DPI),
            "a missing DPI must degrade the deadzone, not refuse to arm"
        );
        assert!(
            plan.target
                .spec
                .divert_hold_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Back),
            "hold-mode must arm when injection can deliver, even with no sensor reading"
        );
    }

    #[test]
    fn unarmed_hold_is_plain_diverted_so_firmware_cannot_scroll() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Forward, Binding::Single(Action::Pan));
        cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Zoom));

        let plan = plan_with(
            &cfg,
            "2b042",
            CaptureHostAbility {
                os_mouse_hook_available: true,
                injection_available: false,
                sensor_dpi: Some(Dpi::new(1000)),
            },
        );
        assert!(
            plan.target.spec.divert_hold_buttons.is_empty(),
            "raw-XY must stay off when injection cannot deliver"
        );
        assert!(
            plan.target
                .spec
                .divert_buttons
                .iter()
                .any(|&(_, button)| button == ButtonId::Forward)
                && plan
                    .target
                    .spec
                    .divert_buttons
                    .iter()
                    .any(|&(_, button)| button == ButtonId::Back),
            "an undeliverable hold must be swallowed over HID++, not left native for firmware scroll: {:?}",
            plan.target.spec.divert_buttons
        );
    }
}
