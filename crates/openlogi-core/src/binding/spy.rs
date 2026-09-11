//! Which HID++ models have a locked `0x8110` spy remap, and how to add one.
//!
//! Extra buttons such as G502 G6–G9 must **not** join [`ButtonId::ALL`]: that
//! list seeds every MX popover. Each supported model is one [`SpyModel`] row
//! keyed by [`crate::device::DeviceModelInfo::config_key`] (for the G502 X
//! Plus, [`G502_X_PLUS_CONFIG_KEY`]). Schema-5 *settings* keys (`unit:…`)
//! resolve through [`crate::bindings::spy_model_for_device`].
//!
//! DPI Up / DPI Down / the other extras appear in the GUI only for a row in
//! [`SPY_MODELS`]. An MX Master (`2b042`) or an undocumented cousin (`0409f`)
//! gets nothing until someone lands a live watch dump — do not copy bits.
//!
//! # Adding a model
//!
//! Do not copy another row's bits or silhouette. A cousin (G502 X LS) still
//! needs its own dump even if the feature table looks identical.
//!
//! 1. Quit Options+ / G HUB / the brew app so this agent owns the receiver.
//! 2. `openlogi diag mouse-buttons --watch` — press each extra button; record
//!    bit → official name. G4/G5 may also appear in that mask: list them on
//!    [`SpyModel::spy_owned_os_buttons`] only when the mouse has no `0x1b04`
//!    ReprogControls (the OS hook cannot divert those clicks).
//! 3. Add the HID++ model id to [`SPY_MODELS`].
//! 4. Add the same key's bit table in `openlogi-device`
//!    `session/gesture/spy.rs` (`SPY_BIT_TABLES`). Bits stay per-model — two
//!    cousins can share [`ButtonId`]s with different masks.
//! 5. Add a silhouette overlay in `openlogi-desktop`
//!    `hotspots::spy_overlay_for` (gated on that key only).
//!
//! The CLI dump/watch path stays read-only and never switches Host mode.

use std::collections::BTreeMap;

use super::action::Action;
use super::button::{ButtonId, G502_X_PLUS_CONFIG_KEY};
use super::defaults::default_binding;
use super::value::Binding;

/// One firmware model that may enter Host mode and remap `0x8110` buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpyModel {
    /// HID++ registry id (`extended_model_id` + `model_ids[0]`).
    pub config_key: &'static str,
    /// Extra buttons seeded and shown for this model. Kept out of [`ButtonId::ALL`].
    pub extra_buttons: &'static [ButtonId],
    /// OS-hook buttons this model remaps through spy when they leave their
    /// native default. Empty when `0x1b04` already owns those controls.
    pub spy_owned_os_buttons: &'static [ButtonId],
}

/// Locked spy models. Only rows backed by a live watch dump belong here.
pub const SPY_MODELS: &[SpyModel] = &[SpyModel {
    config_key: G502_X_PLUS_CONFIG_KEY,
    extra_buttons: &ButtonId::SPY_BUTTONS,
    // No `0x1b04` on this mouse: remapped G4/G5 must be suppressed in the
    // `0x8110` mapping or the OS keeps seeing hardware Back/Forward.
    spy_owned_os_buttons: &[ButtonId::Back, ButtonId::Forward],
}];

impl SpyModel {
    /// The locked row for this HID++ model id, if any.
    #[must_use]
    pub fn for_hidpp_key(key: &str) -> Option<&'static Self> {
        SPY_MODELS.iter().find(|model| model.config_key == key)
    }

    /// Buttons to arm for a Host-mode spy session.
    ///
    /// One customized extra takes over the whole extra cluster (Host mode
    /// pauses onboard profiles for all of them). Remapped
    /// [`Self::spy_owned_os_buttons`] join that set one-by-one so unbound G4/G5
    /// stay firmware-native.
    #[must_use]
    pub fn armed_buttons(&self, bindings: &BTreeMap<ButtonId, Binding>) -> Vec<ButtonId> {
        let extras_customized = self.extra_buttons.iter().any(|button| {
            bindings
                .get(button)
                .is_some_and(|binding| binding.click_action() != Action::None)
        });
        let mut armed = Vec::new();
        if extras_customized {
            armed.extend_from_slice(self.extra_buttons);
        }
        for button in self.spy_owned_os_buttons {
            if bindings
                .get(button)
                .is_some_and(|binding| os_button_needs_spy(*button, binding))
            {
                armed.push(*button);
            }
        }
        armed
    }
}

fn os_button_needs_spy(button: ButtonId, binding: &Binding) -> bool {
    match binding {
        Binding::LongPress(_) | Binding::Gesture(_) => true,
        Binding::Single(action) => *action != default_binding(button),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_g502_x_plus_is_a_spy_model() {
        let model = SpyModel::for_hidpp_key(G502_X_PLUS_CONFIG_KEY).expect("G502 row");
        assert_eq!(model.extra_buttons, ButtonId::SPY_BUTTONS.as_slice());
        assert_eq!(
            model.spy_owned_os_buttons,
            [ButtonId::Back, ButtonId::Forward].as_slice()
        );
        assert_eq!(SpyModel::for_hidpp_key("2b042"), None);
        assert_eq!(SpyModel::for_hidpp_key("0409f"), None);
        assert_eq!(SpyModel::for_hidpp_key("unit:75c69495"), None);
    }

    #[test]
    fn extras_arm_the_cluster_without_native_side_buttons() {
        let model = SpyModel::for_hidpp_key(G502_X_PLUS_CONFIG_KEY).expect("G502 row");
        let bindings = BTreeMap::from([(ButtonId::DpiUp, Binding::Single(Action::Copy))]);
        assert_eq!(
            model.armed_buttons(&bindings),
            ButtonId::SPY_BUTTONS.to_vec()
        );
    }

    #[test]
    fn remapped_side_buttons_join_the_armed_set() {
        let model = SpyModel::for_hidpp_key(G502_X_PLUS_CONFIG_KEY).expect("G502 row");
        let bindings = BTreeMap::from([(ButtonId::Back, Binding::Single(Action::PreviousDesktop))]);
        assert_eq!(model.armed_buttons(&bindings), vec![ButtonId::Back]);
    }

    #[test]
    fn remapped_side_buttons_join_an_already_armed_cluster() {
        let model = SpyModel::for_hidpp_key(G502_X_PLUS_CONFIG_KEY).expect("G502 row");
        let bindings = BTreeMap::from([
            (ButtonId::DpiUp, Binding::Single(Action::Copy)),
            (ButtonId::Forward, Binding::Single(Action::NextDesktop)),
        ]);
        let mut expected = ButtonId::SPY_BUTTONS.to_vec();
        expected.push(ButtonId::Forward);
        assert_eq!(model.armed_buttons(&bindings), expected);
    }
}
