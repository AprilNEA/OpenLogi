//! Keeping a multi-platform keyboard on the host OS's key layout (HID++
//! `0x4531`), the "keep keyboard in OS layout" setting of Logi Options+.
//!
//! A per-device setting the agent acts on: it sets the host's layout when the
//! keyboard connects, when a reload turns the setting on, and after the
//! keyboard reports that a layout shortcut switched it. The app only persists
//! the choice and reloads the agent; there is no keyboard reading to show.

use openlogi_core::config::{Config, PinnedOs};
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

    /// The OS the active keyboard's layout is pinned to, if any.
    #[must_use]
    pub fn current_os_layout_pin(&self) -> Option<PinnedOs> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.os_layout_pin(key))
    }

    /// Persist whether the agent keeps the active keyboard on the host OS's
    /// layout, and reload the agent, which sets the layout when the setting
    /// turns on. No-op when no device is selected, the keyboard does not let
    /// software set its layout, or it has no persistent key.
    pub fn commit_keep_os_layout(&mut self, keep: bool) -> StateEvents {
        self.commit_os_layout_setting("keep OS layout", |config, key| {
            config.set_keep_os_layout(key, keep);
        })
    }

    /// Persist the OS the active keyboard's layout is pinned to, and reload the
    /// agent, which sets the layout when the pin changes. Same guards as
    /// [`Self::commit_keep_os_layout`].
    pub fn commit_os_layout_pin(&mut self, pin: Option<PinnedOs>) -> StateEvents {
        self.commit_os_layout_setting("keyboard layout pin", |config, key| {
            config.set_os_layout_pin(key, pin);
        })
    }

    /// Apply `edit` to the active keyboard's config and reload the agent.
    fn commit_os_layout_setting(
        &mut self,
        what: &str,
        edit: impl FnOnce(&mut Config, &str),
    ) -> StateEvents {
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
            debug!("no persistent device key — {what} change ignored");
            return events;
        };
        self.config.edit(|config| edit(config, &key));
        self.persist_and_reload(what);
        events
    }
}
