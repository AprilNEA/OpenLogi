//! Keeping a multi-platform keyboard on the host OS's key layout (HID++
//! `0x4531`), the "keep keyboard in OS layout" setting of Logi Options+.
//!
//! A per-device setting the agent acts on: it sets the host's layout when the
//! keyboard connects, when a reload turns the setting on, and after the
//! keyboard reports that a layout shortcut switched it. The app only persists
//! the choice and reloads the agent; there is no keyboard reading to show.

use tracing::debug;

use super::events::StateEvents;
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

impl AppState {
    /// Whether the active keyboard lets software set its key layout.
    #[must_use]
    pub fn current_os_layout_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.os_layout)
    }

    /// Whether the agent keeps the active keyboard on the host OS's layout:
    /// on unless the user turned it off, as in Logi Options+.
    #[must_use]
    pub fn current_keep_os_layout(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_none_or(|key| self.config.keep_os_layout(key))
    }

    /// Persist whether the agent keeps the active keyboard on the host OS's
    /// layout, and reload the agent, which sets the layout when the setting
    /// turns on. No-op when no device is selected, the keyboard does not let
    /// software set its layout, or it has no persistent key.
    pub fn commit_keep_os_layout(&mut self, keep: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        if !self.current_os_layout_supported() {
            debug!("active device cannot have its key layout set");
            return events;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — keep-OS-layout change ignored");
            return events;
        };
        self.config
            .edit(|config| config.set_keep_os_layout(&key, keep));
        self.persist_and_reload("keep OS layout");
        events
    }
}
