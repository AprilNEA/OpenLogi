//! Mouse, gesture, and keyboard binding commits.

use std::collections::BTreeMap;

use gpui::App;
use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, GestureResponseTime};
use openlogi_core::bindings::{bindings_for, hidpp_gesture_maps_for, oshook_gestures_for};
use openlogi_core::config::{Config, KeyTrigger};
use tracing::debug;

use crate::features::mouse::thumbwheel::{ThumbwheelPair, ThumbwheelPreset};
use crate::state::devices::DeviceRecord;

use super::{AppState, StateEvent};

/// The per-app profile the binding panels are editing, and the device it was
/// chosen for. Pairing them prevents a scope opened for one mouse from
/// carrying over when selection moves to another.
struct EditingScope {
    device_key: String,
    app: String,
}

/// Binding-editor state projected from the persisted configuration.
pub(super) struct BindingState {
    editing_scope: Option<EditingScope>,
    /// The hotspot the user most recently armed by clicking.
    active_button: Option<ButtonId>,
    /// Effective bindings for the selected device and open profile.
    button_bindings: BTreeMap<ButtonId, Action>,
    /// Device-global per-direction gesture bindings.
    gesture_bindings: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// Global keyboard F-key bindings (Esc + F1-F19).
    keyboard_bindings: BTreeMap<KeyTrigger, Action>,
}

impl BindingState {
    pub(super) fn new(config: &Config, persistent_key: Option<&str>) -> Self {
        let mut state = Self {
            editing_scope: None,
            active_button: None,
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            keyboard_bindings: config.keyboard.bindings.clone(),
        };
        state.refresh_device(config, persistent_key);
        state
    }

    fn editing_app<'a>(&'a self, persistent_key: Option<&str>) -> Option<&'a str> {
        let key = persistent_key?;
        self.editing_scope
            .as_ref()
            .filter(|scope| scope.device_key == key)
            .map(|scope| scope.app.as_str())
    }

    fn set_editing_app(
        &mut self,
        config: &Config,
        persistent_key: Option<&str>,
        app: Option<String>,
    ) {
        self.editing_scope = app
            .zip(persistent_key.map(str::to_string))
            .map(|(app, device_key)| EditingScope { device_key, app });
        self.refresh_device(config, persistent_key);
    }

    fn refresh_device(&mut self, config: &Config, persistent_key: Option<&str>) {
        let button_bindings =
            bindings_for(config, persistent_key, self.editing_app(persistent_key));
        let gesture_bindings = gesture_maps_for(config, persistent_key);
        self.button_bindings = button_bindings;
        self.gesture_bindings = gesture_bindings;
    }

    fn restore(&mut self, config: &Config, persistent_key: Option<&str>) {
        self.refresh_device(config, persistent_key);
        self.keyboard_bindings = config.keyboard.bindings.clone();
    }
}

fn gesture_maps_for(
    config: &Config,
    persistent_key: Option<&str>,
) -> BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
    let Some(key) = persistent_key else {
        return BTreeMap::new();
    };
    let mut maps = hidpp_gesture_maps_for(config, Some(key));
    maps.extend(oshook_gestures_for(config, Some(key), None));
    maps
}

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
    /// The application whose profile the binding panels are editing, or `None`
    /// for the device's global profile.
    #[must_use]
    pub fn editing_app(&self) -> Option<&str> {
        self.bindings.editing_app(
            self.current_record()
                .and_then(DeviceRecord::persistent_config_key),
        )
    }

    /// Edit `app`'s profile for the active device, or its global profile with
    /// `None`. Re-derives the editor projections without persisting this
    /// window-local choice.
    pub fn set_editing_app(&mut self, app: Option<String>) {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        self.bindings
            .set_editing_app(&self.config, key.as_deref(), app);
    }

    /// The hotspot most recently armed in the mouse editor.
    #[must_use]
    pub fn active_button(&self) -> Option<ButtonId> {
        self.bindings.active_button
    }

    /// Effective mouse bindings for the selected device and open profile.
    #[must_use]
    pub fn button_bindings(&self) -> &BTreeMap<ButtonId, Action> {
        &self.bindings.button_bindings
    }

    /// Device-global gesture direction maps for the selected device.
    #[must_use]
    pub fn gesture_bindings(&self) -> &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> {
        &self.bindings.gesture_bindings
    }

    /// Global keyboard F-key bindings.
    #[must_use]
    pub fn keyboard_bindings(&self) -> &BTreeMap<KeyTrigger, Action> {
        &self.bindings.keyboard_bindings
    }

    pub(super) fn refresh_binding_projections(&mut self) {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        self.bindings.refresh_device(&self.config, key.as_deref());
    }

    pub(super) fn restore_binding_projections(&mut self) {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        self.bindings.restore(&self.config, key.as_deref());
    }

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
        self.bindings.button_bindings.insert(button, action.clone());

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
        let app = self.editing_app().map(str::to_string);
        self.config.edit(|config| match app {
            // A per-app entry is `Action`-valued, so an override always
            // replaces the whole button — which is exactly what picking one
            // action means, and why gesture mode is not offered in this scope.
            Some(app) => config.set_per_app_binding(&key, &app, button, Some(action)),
            None => config.set_binding(&key, button, Binding::Single(action)),
        });
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
        self.config.edit(|config| {
            for button in buttons {
                config.set_per_app_binding(&key, &app, button, None);
            }
        });
        self.refresh_binding_projections();
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
        self.config
            .edit(|config| config.remove_app_profile(&key, &app));
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
        let changed = self.config.edit(|config| {
            apply_thumbwheel_pair(
                &mut self.bindings.button_bindings,
                config,
                key.as_deref(),
                app.as_deref(),
                pair,
            )
        });
        if !changed {
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
                self.bindings
                    .keyboard_bindings
                    .insert(trigger.clone(), a.clone());
            }
            None => {
                self.bindings.keyboard_bindings.remove(&trigger);
            }
        }
        self.config
            .edit(|config| config.set_keyboard_binding(trigger, action));
        self.persist_and_reload("keyboard binding");
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
        gesture_maps_for(&self.config, Some(key))
    }

    /// How many gesture directions the active device has bound, across every
    /// gesture-mode button. Device-level like [`Self::device_gesture_maps`].
    #[must_use]
    pub fn device_gesture_binding_count(&self) -> usize {
        self.device_gesture_maps().values().map(BTreeMap::len).sum()
    }

    /// Gesture response time for one control on the active physical device.
    #[must_use]
    pub fn gesture_response_time(&self, button: ButtonId) -> GestureResponseTime {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map_or_else(GestureResponseTime::default, |key| {
                self.config.gesture_response_time(key, button)
            })
    }

    /// Persist one control's gesture response time and reload the agent so its
    /// HID++ session or OS-hook map adopts it immediately.
    pub fn commit_gesture_response_time(
        &mut self,
        button: ButtonId,
        responsiveness: GestureResponseTime,
    ) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        if self.config.gesture_response_time(&key, button) == responsiveness {
            return;
        }
        self.config.edit(|config| {
            config.set_gesture_response_time(&key, button, responsiveness);
        });
        self.persist_and_reload("gesture response time");
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
        self.config
            .edit(|config| config.set_gesture_mode(&key, button, enabled));
        // The mode change shuffles bindings between the single + gesture maps.
        self.refresh_binding_projections();
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
        self.bindings
            .gesture_bindings
            .entry(button)
            .or_default()
            .insert(direction, action.clone());
        self.config
            .edit(|config| config.set_gesture_direction(&key, button, direction, action));
        // The agent owns the gesture watcher; have it rebuild from config.
        self.persist_and_reload("gesture binding");
    }
}
