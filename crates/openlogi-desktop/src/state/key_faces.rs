//! A keypad's LCD key faces: the label, colours, picture, and icon the user
//! chose for one grid key on one binding page, and the live value a key may
//! show instead. The agent repaints the key when the reloaded config changes
//! its face.

use openlogi_core::binding::{ButtonId, GridSlot};
use openlogi_core::config::{KeyFaceStyle, LiveKey};
use tracing::debug;

use super::events::StateEvents;
use super::{AppState, DeviceRecord, StateEvent};

impl AppState {
    /// How the selected device's LCD key at `slot` looks: its saved style, or
    /// the default face (the action's icon, white on black).
    #[must_use]
    pub fn current_key_face(&self, slot: GridSlot) -> KeyFaceStyle {
        self.current_record()
            .and_then(|record| {
                self.config
                    .key_face_style(&record.config_key, ButtonId::Grid(slot))
            })
            .cloned()
            .unwrap_or_default()
    }

    /// The config key of the device whose key faces are edited and saved:
    /// the selected device, when it keeps settings. An edit that started for
    /// one device checks this before it saves, so it never lands on another.
    #[must_use]
    pub fn key_face_device(&self) -> Option<&str> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
    }

    /// Every picture a key face shows, for the Keys view to read the versions
    /// of off the UI thread. Touches no file.
    #[must_use]
    pub fn key_picture_paths(&self) -> std::collections::BTreeSet<std::path::PathBuf> {
        self.config.key_picture_paths()
    }

    /// Save `style` for the selected device's LCD key at `slot` and have the
    /// agent repaint it. A default style removes the entry; a style that
    /// would not change anything writes nothing and is still reported. A
    /// device without persistent settings keeps nothing.
    pub fn commit_key_face(&mut self, slot: GridSlot, style: KeyFaceStyle) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        let Some(key) = self.key_face_device().map(str::to_owned) else {
            debug!(?slot, "no persistent device key — key face not saved");
            return events;
        };
        let button = ButtonId::Grid(slot);
        let stored = self.config.key_face_style(&key, button).cloned();
        if stored.unwrap_or_default() == style {
            return events;
        }
        let style = (!style.is_default()).then_some(style);
        self.config
            .edit(|config| config.set_key_face_style(&key, button, style));
        self.persist_and_reload("key face");
        events
    }
}

impl AppState {
    /// The live key at `slot` on the selected device, if it shows one.
    #[must_use]
    pub fn current_live_key(&self, slot: GridSlot) -> Option<LiveKey> {
        let key = self.current_record()?.persistent_config_key()?;
        self.config
            .devices
            .get(key)?
            .live_keys
            .get(&ButtonId::Grid(slot))
            .cloned()
    }

    /// The devices a live key can show the battery of: each one with
    /// persistent settings, as its config key and its name.
    #[must_use]
    pub fn live_battery_devices(&self) -> Vec<(String, String)> {
        self.devices()
            .iter()
            .filter_map(|record| {
                record
                    .persistent_config_key()
                    .map(|key| (key.to_owned(), record.display_name.clone()))
            })
            .collect()
    }

    /// Make `slot` on the selected device a live key showing `live`, or a
    /// plain key again with `None`; the agent repaints it on reload.
    ///
    /// A live key's slot runs its activation instead of its binding
    /// (`button_bindings_for`), so the cached binding projection is rebuilt
    /// and the change announced as a binding change: the Action tab then
    /// shows what the key runs now, as the LCD preview does.
    pub fn commit_live_key(&mut self, slot: GridSlot, live: Option<LiveKey>) -> StateEvents {
        let events = self.for_current_device(StateEvent::BindingsChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_owned)
        else {
            return events;
        };
        if self.current_live_key(slot) == live {
            return events;
        }
        self.config.edit(|config| {
            let live_keys = &mut config.devices.entry(key).or_default().live_keys;
            match live {
                Some(live) => {
                    live_keys.insert(ButtonId::Grid(slot), live);
                }
                None => {
                    live_keys.remove(&ButtonId::Grid(slot));
                }
            }
        });
        self.refresh_binding_projections();
        self.persist_and_reload("live key");
        events
    }
}
