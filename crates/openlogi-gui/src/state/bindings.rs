//! Mouse, gesture, and keyboard binding commits.

use std::collections::BTreeMap;

use openlogi_agent_core::bindings::{bindings_for, gesture_bindings_for};
use openlogi_core::config::KeyTrigger;
use tracing::debug;

use crate::data::mouse_buttons::{Action, Binding, ButtonId, GestureDirection};
use crate::mouse_model::thumbwheel::{ThumbwheelPair, ThumbwheelPreset};
use crate::state::devices::DeviceRecord;

use super::AppState;

pub(super) fn apply_thumbwheel_pair(
    button_bindings: &mut BTreeMap<ButtonId, Action>,
    config: &mut openlogi_core::config::Config,
    persistent_key: Option<&str>,
    pair: ThumbwheelPair,
) -> bool {
    button_bindings.insert(ButtonId::ThumbwheelScrollDown, pair.backward.clone());
    button_bindings.insert(ButtonId::ThumbwheelScrollUp, pair.forward.clone());

    let Some(key) = persistent_key else {
        return false;
    };
    config.set_binding(
        key,
        ButtonId::ThumbwheelScrollDown,
        Binding::Single(pair.backward),
    );
    config.set_binding(
        key,
        ButtonId::ThumbwheelScrollUp,
        Binding::Single(pair.forward),
    );
    true
}

impl AppState {
    /// Update a single binding in memory, on disk, and in the shared hook
    /// map for the currently selected device.
    ///
    /// Disk failures and poisoned hook locks are logged at `warn` instead
    /// of bubbling up: the UI thread shouldn't crash because the user's
    /// home volume is full or because the hook thread panicked.
    pub fn commit_binding(&mut self, button: ButtonId, action: Action) {
        self.button_bindings.insert(button, action.clone());

        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(
                ?button,
                "no persistent device key — binding kept in memory only"
            );
            return;
        };
        self.config
            .set_binding(&key, button, Binding::Single(action));
        // The agent owns the hook; have it rebuild its live map from config.
        self.persist_and_reload("binding");
    }

    /// Apply one paired thumb-wheel preset atomically. Both directional
    /// bindings are updated before the single config persistence/reload.
    pub fn commit_thumbwheel_preset(&mut self, preset: ThumbwheelPreset) {
        let pair = preset.pair();
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        if !apply_thumbwheel_pair(
            &mut self.button_bindings,
            &mut self.config,
            key.as_deref(),
            pair,
        ) {
            debug!("no persistent device key — thumb-wheel pair kept in memory only");
            return;
        }
        self.persist_and_reload("thumb-wheel binding");
    }
    /// Records (or, with `action = None`, clears) the F-key `trigger` binding
    /// in the global `[keyboard]` map. Mirrors [`Self::commit_binding`] minus
    /// the device key — keyboard bindings are device-agnostic, so there's no
    /// `current_record()` dependency. The agent's `rebuild()` republishes its
    /// shared keyboard map on `reload_config`, so this lands live.
    pub fn commit_keyboard_binding(&mut self, trigger: KeyTrigger, action: Option<Action>) {
        match action {
            Some(ref a) => {
                self.keyboard_bindings.insert(trigger.clone(), a.clone());
            }
            None => {
                self.keyboard_bindings.remove(&trigger);
            }
        }
        self.config.set_keyboard_binding(trigger, action);
        self.persist_and_reload("keyboard binding");
    }
    pub(crate) fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record()
                .and_then(DeviceRecord::persistent_config_key),
            self.current_app_bundle.as_deref(),
        )
    }
    pub(crate) fn gesture_bindings_for_current(&self) -> BTreeMap<GestureDirection, Action> {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
        else {
            return BTreeMap::new();
        };
        match self.config.gesture_owner(key) {
            // The HID++ gesture button seeds every direction from the defaults.
            Some(ButtonId::GestureButton) => gesture_bindings_for(&self.config, Some(key)),
            // A promoted OS-hook button is shown from its raw stored map (which
            // `set_gesture_owner` seeds with full defaults), so the menu matches
            // exactly what `oshook_gestures_for` dispatches — no seeding here.
            Some(owner) => match self.config.bindings_for(key).get(&owner) {
                Some(Binding::Gesture(map)) => map.clone(),
                _ => BTreeMap::new(),
            },
            None => BTreeMap::new(),
        }
    }
    /// The current device's gesture button — the [`Binding::Gesture`] owner — or
    /// `None` when no button is in gesture mode. Drives which button's card opens
    /// the gesture menu rather than the single-action picker.
    #[must_use]
    pub fn current_gesture_owner(&self) -> Option<ButtonId> {
        let key = self.current_record()?.persistent_config_key()?;
        self.config.gesture_owner(key)
    }
    /// Make `button` the current device's gesture button (or clear it with
    /// `None`), enforcing the one-gesture-button-per-device lock. Persists, tells
    /// the agent to rebuild, and refreshes the projected maps the UI reads.
    pub fn commit_gesture_owner(&mut self, button: Option<ButtonId>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        match button {
            Some(b) => {
                self.config.set_gesture_owner(&key, b);
            }
            None => {
                self.config.disable_gestures(&key);
            }
        }
        // The owner change shuffles bindings between the single + gesture maps.
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        self.persist_and_reload("gesture-button change");
    }
    /// Update a single gesture-button sub-binding in memory, on disk, and in the
    /// shared gesture map the watcher thread reads.
    pub fn commit_gesture_binding(&mut self, direction: GestureDirection, action: Action) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(
                ?direction,
                "no persistent device key — gesture binding edit ignored"
            );
            return;
        };
        // Edit whichever button owns gestures — not always the HID++ gesture button. When
        // gestures are off, a stray edit must NOT silently re-enable them on the
        // default owner (the gesture editor shouldn't be reachable in that state):
        // no-op instead.
        let Some(owner) = self.config.gesture_owner(&key) else {
            debug!(
                ?direction,
                "gestures are off — ignoring gesture binding edit"
            );
            return;
        };
        self.gesture_bindings.insert(direction, action.clone());
        self.config
            .set_gesture_direction(&key, owner, direction, action);
        // The agent owns the gesture watcher; have it rebuild from config.
        self.persist_and_reload("gesture binding");
    }
}
