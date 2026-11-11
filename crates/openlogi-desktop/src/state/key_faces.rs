//! A keypad's LCD key faces: the label, colours, picture, and icon the user
//! chose for one grid key on one binding page. The agent repaints the key
//! when the reloaded config changes its face.

use openlogi_core::binding::{ButtonId, GridSlot};
use openlogi_core::config::KeyFaceStyle;
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

    /// Save `style` for the selected device's LCD key at `slot` and have the
    /// agent repaint it. A default style removes the entry; a style that
    /// would not change anything writes nothing and is still reported. A
    /// device without persistent settings keeps nothing.
    pub fn commit_key_face(&mut self, slot: GridSlot, style: KeyFaceStyle) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_owned)
        else {
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
