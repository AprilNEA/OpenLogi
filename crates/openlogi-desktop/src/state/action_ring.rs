//! Actions Ring settings and editor scope for the selected device.

use openlogi_core::binding::{
    ActionRingConfig, ActionRingIcon, ActionRingLayout, ActionRingSlot, RingAction,
};

use super::{AppState, DeviceRecord};

impl AppState {
    /// Actions Ring settings for the active device, including its implicit
    /// default layout when nothing has been persisted yet.
    #[must_use]
    pub fn current_action_ring(&self) -> ActionRingConfig {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.action_ring(key))
            .unwrap_or_default()
    }

    /// Application profile open in the Actions Ring editor, or `None` for the
    /// default layout.
    #[must_use]
    pub fn editing_action_ring_app(&self) -> Option<&str> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.action_ring_editing_apps.get(key).map(String::as_str)
    }

    /// Open an application-specific Actions Ring layout, or the default
    /// layout with `None`. This window-local choice is not persisted.
    pub fn set_editing_action_ring_app(&mut self, app: Option<String>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        if let Some(app) = app {
            self.action_ring_editing_apps.insert(key, app);
        } else {
            self.action_ring_editing_apps.remove(&key);
        }
    }

    /// Complete layout shown by the Actions Ring editor. An application with
    /// no saved profile inherits the default layout until its first edit.
    #[must_use]
    pub fn current_action_ring_layout(&self) -> ActionRingLayout {
        let ring = self.current_action_ring();
        ring.effective_layout(self.editing_action_ring_app())
    }

    /// Replace or clear one slot in the open Actions Ring layout.
    pub fn commit_action_ring_slot(&mut self, slot: ActionRingSlot, action: Option<RingAction>) {
        self.edit_action_ring_layout("Actions Ring slot", |layout| {
            layout.set_action(slot, action);
        });
    }

    /// Set or restore the action-derived icon for one slot in the open layout.
    pub fn commit_action_ring_icon(&mut self, slot: ActionRingSlot, icon: Option<ActionRingIcon>) {
        self.edit_action_ring_layout("Actions Ring icon", |layout| {
            layout.set_icon(slot, icon);
        });
    }

    fn edit_action_ring_layout(&mut self, what: &str, edit: impl FnOnce(&mut ActionRingLayout)) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        let app = self.editing_action_ring_app().map(str::to_string);
        self.config.edit(|config| {
            let ring = &mut config.devices.entry(key).or_default().action_ring;
            let layout = match app {
                Some(app) => {
                    let inherited = ring.default.clone();
                    ring.per_app.entry(app).or_insert(inherited)
                }
                None => &mut ring.default,
            };
            edit(layout);
        });
        self.persist_and_reload(what);
    }

    /// Delete the open application-specific Actions Ring layout and return to
    /// the default layout. Button bindings for the application are untouched.
    pub fn remove_editing_action_ring_profile(&mut self) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        let Some(app) = self.editing_action_ring_app().map(str::to_string) else {
            return;
        };
        self.config.edit(|config| {
            if let Some(device) = config.devices.get_mut(&key) {
                device.action_ring.per_app.remove(&app);
            }
        });
        self.set_editing_action_ring_app(None);
        self.persist_and_reload("Actions Ring application profile");
    }

    /// Enable or disable the active device's Actions Ring.
    pub fn commit_action_ring_enabled(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_enabled(&key, enabled));
        self.persist_and_reload("Actions Ring enabled state");
    }

    /// Enable or disable hover and activation haptics for the active ring.
    pub fn commit_action_ring_haptics(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config
            .edit(|config| config.set_action_ring_haptics(&key, enabled));
        self.persist_and_reload("Actions Ring haptics");
    }
}
