//! Per-device click debounce for worn switches (#2182).

use tracing::debug;

use openlogi_core::config::ClickDebounce;

use crate::state::devices::DeviceRecord;

use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether this platform can filter one device's clicks. The agent filters
    /// them in its OS input hook, which on Windows cannot tell which device
    /// sent a click.
    #[must_use]
    pub const fn click_debounce_available() -> bool {
        openlogi_hook::BUTTON_SOURCES_ATTRIBUTED
    }

    /// The active device's click-debounce window, or `None` when its clicks
    /// are not filtered.
    #[must_use]
    pub fn current_click_debounce(&self) -> Option<ClickDebounce> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.click_debounce(key))
    }

    /// Set the active device's click-debounce window, or turn its filter off
    /// with `None`, then persist it and reload the agent. Turning it on needs
    /// [`Self::click_debounce_available`]; turning it off never does, so a
    /// window set on another platform can be removed. Committing the current
    /// value saves and reloads nothing.
    pub fn commit_click_debounce(&mut self, window: Option<ClickDebounce>) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        if window.is_some() && !Self::click_debounce_available() {
            debug!("this platform cannot filter one device's clicks");
            return events;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — click-debounce change ignored");
            return events;
        };
        if self.config.click_debounce(&key) == window {
            return events;
        }
        self.config
            .edit(|config| config.set_click_debounce(&key, window));
        self.persist_and_reload("click debounce");
        events
    }
}
