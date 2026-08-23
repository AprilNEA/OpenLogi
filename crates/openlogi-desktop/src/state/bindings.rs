//! Mouse, gesture, and keyboard binding commits.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::bindings::{bindings_for, hidpp_gesture_maps_for, oshook_gestures_for};
use openlogi_core::config::KeyTrigger;
use tracing::debug;

use crate::features::mouse::thumbwheel::{ThumbwheelPair, ThumbwheelPreset};
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
    /// Disk failures restore the persisted projection and surface a config
    /// error instead of crashing the UI thread.
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
    /// The active device's bindings as this window edits them.
    ///
    /// Deliberately the *global* profile, not the one the agent has live: this
    /// is an editor, and following the foreground app would rewrite the panel
    /// every time the user tabbed away. What is live is reported separately —
    /// see [`AppState::active_profile_name`].
    pub(crate) fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record()
                .and_then(DeviceRecord::persistent_config_key),
            None,
        )
    }
    /// Per-direction display maps for every gesture-mode button of the current
    /// device, keyed by button — what each button's gesture menu edits and what
    /// the runtime dispatches for it. HID++ sources come fully seeded (matching
    /// the gesture watcher's projection); OS-hook buttons show their raw stored
    /// map (matching the OS hook's dispatch). Empty when no device is selected.
    #[must_use]
    pub fn current_gesture_maps(&self) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
        else {
            return BTreeMap::new();
        };
        // Both halves come from the same helpers the runtime dispatches with,
        // so the menus can never drift from what the agent actually does:
        // HID++ sources seeded like the gesture watcher, OS-hook buttons raw
        // like the hook (global view — no per-app overlay here).
        let mut maps = hidpp_gesture_maps_for(&self.config, Some(key));
        maps.extend(oshook_gestures_for(&self.config, Some(key), None));
        maps
    }

    /// Turn gesture mode on or off for one button of the current device —
    /// independently of every other button. Persists, tells the agent to
    /// rebuild, and refreshes the projected maps the UI reads.
    pub fn commit_gesture_mode(&mut self, button: ButtonId, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        if self.config.is_gesture_mode(&key, button) == enabled {
            return;
        }
        self.config.set_gesture_mode(&key, button, enabled);
        // The mode change shuffles bindings between the single + gesture maps.
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.current_gesture_maps();
        self.persist_and_reload("gesture-mode change");
    }

    /// Update one direction of `button`'s gesture binding in memory, on disk,
    /// and (via reload) in the maps the agent dispatches from.
    pub fn commit_gesture_binding(
        &mut self,
        button: ButtonId,
        direction: GestureDirection,
        action: Action,
    ) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(
                ?button,
                ?direction,
                "no persistent device key — gesture binding edit ignored"
            );
            return;
        };
        // A stray edit on a button not in gesture mode must NOT silently
        // promote it (the gesture editor shouldn't be reachable in that
        // state): no-op instead.
        if !self.config.is_gesture_mode(&key, button) {
            debug!(
                ?button,
                ?direction,
                "button is not in gesture mode — ignoring gesture binding edit"
            );
            return;
        }
        self.gesture_bindings
            .entry(button)
            .or_default()
            .insert(direction, action.clone());
        self.config
            .set_gesture_direction(&key, button, direction, action);
        // The agent owns the gesture watcher; have it rebuild from config.
        self.persist_and_reload("gesture binding");
    }
}
