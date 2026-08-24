//! Mouse, gesture, and keyboard binding commits.

use std::collections::BTreeMap;

use gpui::App;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection};
use openlogi_core::bindings::{bindings_for, hidpp_gesture_maps_for, oshook_gestures_for};
use openlogi_core::config::KeyTrigger;
use tracing::debug;

use crate::features::mouse::thumbwheel::{ThumbwheelPair, ThumbwheelPreset};
use crate::state::devices::DeviceRecord;

use super::{AppState, StateEvent};

/// Write both halves of a thumb-wheel preset into `app`'s profile, or the
/// device's global bindings when `app` is `None`.
pub(super) fn apply_thumbwheel_pair(
    button_bindings: &mut BTreeMap<ButtonId, Action>,
    config: &mut openlogi_core::config::Config,
    persistent_key: Option<&str>,
    app: Option<&str>,
    pair: ThumbwheelPair,
) -> bool {
    button_bindings.insert(ButtonId::ThumbwheelScrollDown, pair.backward.clone());
    button_bindings.insert(ButtonId::ThumbwheelScrollUp, pair.forward.clone());

    let Some(key) = persistent_key else {
        return false;
    };
    for (button, action) in [
        (ButtonId::ThumbwheelScrollDown, pair.backward),
        (ButtonId::ThumbwheelScrollUp, pair.forward),
    ] {
        match app {
            Some(app) => config.set_per_app_binding(key, app, button, Some(action)),
            None => config.set_binding(key, button, Binding::Single(action)),
        }
    }
    true
}

impl AppState {
    /// Apply an active-device binding edit and notify every subscribed editor.
    pub(crate) fn update_bindings(cx: &mut App, update: impl FnOnce(&mut Self)) {
        Self::update(cx, |state, cx| {
            let key = state.current_record().map(DeviceRecord::device_key);
            update(state);
            if let Some(key) = key {
                cx.emit(StateEvent::BindingsChanged(key));
            }
        });
    }

    /// Update a single binding in memory, on disk, and in the shared hook
    /// map for the currently selected device — in whichever profile
    /// [`AppState::editing_app`] has open.
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
        match self.editing_app().map(str::to_string) {
            // A per-app entry is `Action`-valued, so an override always
            // replaces the whole button — which is exactly what picking one
            // action means, and why gesture mode is not offered in this scope.
            Some(app) => self
                .config
                .set_per_app_binding(&key, &app, button, Some(action)),
            None => self
                .config
                .set_binding(&key, button, Binding::Single(action)),
        }
        // The agent owns the hook; have it rebuild its live map from config.
        self.persist_and_reload("binding");
    }

    /// Drop `button`'s override in the open per-app profile, so it inherits the
    /// device's global binding again. A no-op in the global profile, which has
    /// nothing to inherit from.
    pub fn clear_app_binding(&mut self, button: ButtonId) {
        self.clear_app_bindings([button]);
    }

    /// Drop both halves of a thumb-wheel override together.
    pub fn clear_app_thumbwheel(&mut self) {
        self.clear_app_bindings([ButtonId::ThumbwheelScrollDown, ButtonId::ThumbwheelScrollUp]);
    }

    fn clear_app_bindings(&mut self, buttons: impl IntoIterator<Item = ButtonId>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        let Some(app) = self.editing_app().map(str::to_string) else {
            return;
        };
        for button in buttons {
            self.config.set_per_app_binding(&key, &app, button, None);
        }
        self.button_bindings = self.bindings_for_current();
        self.persist_and_reload("per-app binding");
    }

    /// Delete the open per-app profile outright and fall back to editing the
    /// device's global bindings.
    pub fn remove_editing_app_profile(&mut self) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        let Some(app) = self.editing_app().map(str::to_string) else {
            return;
        };
        self.config.remove_app_profile(&key, &app);
        self.set_editing_app(None);
        self.persist_and_reload("per-app profile");
    }

    /// The open per-app profile's overrides, so the panel can tell an override
    /// apart from a binding inherited from the global profile. `None` in the
    /// global profile, where there is nothing to distinguish.
    #[must_use]
    pub fn editing_app_overrides(&self) -> Option<&BTreeMap<ButtonId, Action>> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.editing_app()
            .and_then(|app| self.config.per_app_overrides(key, app))
    }

    /// Apply one paired thumb-wheel preset atomically. Both directional
    /// bindings are updated before the single config persistence/reload.
    pub fn commit_thumbwheel_preset(&mut self, preset: ThumbwheelPreset) {
        let pair = preset.pair();
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        let app = self.editing_app().map(str::to_string);
        if !apply_thumbwheel_pair(
            &mut self.button_bindings,
            &mut self.config,
            key.as_deref(),
            app.as_deref(),
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
    /// The active device's bindings in the profile this window has open —
    /// per-app overrides layered over the global bindings, exactly as the hook
    /// resolves them for that app.
    ///
    /// Keyed on the *edited* profile, never on the foreground app: an editor
    /// that rewrote itself every time the user tabbed away would be unusable.
    /// Which profile is live is reported separately — see
    /// [`AppState::active_profile_name`].
    pub(crate) fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record()
                .and_then(DeviceRecord::persistent_config_key),
            self.editing_app(),
        )
    }
    /// Per-direction maps for every gesture-mode button of the current device,
    /// keyed by button — what the runtime dispatches for it. HID++ sources come
    /// fully seeded (matching the gesture watcher's projection); OS-hook
    /// buttons show their raw stored map (matching the OS hook's dispatch).
    /// Empty when no device is selected.
    ///
    /// Device-level: direction maps live only in the global profile, so this
    /// does not vary with the profile this window has open.
    #[must_use]
    pub(crate) fn device_gesture_maps(
        &self,
    ) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
        else {
            return BTreeMap::new();
        };
        // Both halves come from the same helpers the runtime dispatches with,
        // so the menus can never drift from what the agent actually does:
        // HID++ sources seeded like the gesture watcher, OS-hook buttons raw
        // like the hook.
        let mut maps = hidpp_gesture_maps_for(&self.config, Some(key));
        maps.extend(oshook_gestures_for(&self.config, Some(key), None));
        maps
    }

    /// How many gesture directions the active device has bound, across every
    /// gesture-mode button. Device-level like [`Self::device_gesture_maps`].
    #[must_use]
    pub fn device_gesture_binding_count(&self) -> usize {
        self.device_gesture_maps().values().map(BTreeMap::len).sum()
    }

    /// The gesture menus the panel offers: [`Self::device_gesture_maps`], or
    /// nothing while a per-app profile is open.
    ///
    /// A per-app entry holds one `Action` and has no per-direction shape, so
    /// there is nothing to edit in that scope: every button falls through to
    /// the single-action picker, and overriding one is what stops it gesturing
    /// in that app. Offering the gesture menu instead would edit the global
    /// profile from a screen labelled with an application.
    #[must_use]
    #[cfg(test)]
    pub fn current_gesture_maps(&self) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
        if self.editing_app().is_some() {
            return BTreeMap::new();
        }
        self.device_gesture_maps()
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
        // Gesture mode is a property of the device's global bindings — a
        // per-app entry holds one `Action` and has no per-direction shape to
        // promote into. The picker hides the entry point in a per-app profile;
        // this is the backstop, because writing it here would silently change
        // every app instead of the one on screen.
        if self.editing_app().is_some() {
            debug!(?button, "gesture mode is not editable in a per-app profile");
            return;
        }
        if self.config.is_gesture_mode(&key, button) == enabled {
            return;
        }
        self.config.set_gesture_mode(&key, button, enabled);
        // The mode change shuffles bindings between the single + gesture maps.
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.device_gesture_maps();
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
        // Same backstop as `commit_gesture_mode`: direction maps live only in
        // the global profile, so an edit arriving while a per-app one is open
        // would change every app instead of the one on screen.
        if self.editing_app().is_some() {
            debug!(
                ?button,
                ?direction,
                "gestures are not editable in a per-app profile"
            );
            return;
        }
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
