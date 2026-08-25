//! Per-device close-button hover haptics — independent of the Actions Ring
//! haptics toggle (`state/action_ring` territory) and not per-app-profile.

use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::AppState;

impl AppState {
    /// Whether the active device reports haptic-feedback hardware
    /// (`HapticFeedback`, HID++ `0x19b0`). Shared by every haptics toggle —
    /// the Actions Ring's and this one — so both agree on what "supported"
    /// means.
    #[must_use]
    pub fn current_haptics_supported(&self) -> bool {
        self.current_record().is_some_and(|record| {
            record
                .capabilities
                .unwrap_or_else(|| {
                    openlogi_core::device::Capabilities::presumed_from_kind(record.kind)
                })
                .haptic_feedback
        })
    }

    /// Whether the active device plays a haptic pulse when the cursor hovers
    /// a window's close button. `false` when no device is selected, it lacks
    /// haptic feedback, or the user hasn't opted in.
    #[must_use]
    pub fn current_close_button_haptics(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.close_button_haptics(key))
    }

    /// Set whether the active device plays the close-button haptic pulse,
    /// persist it, and reload the agent. No-op when no device is selected or
    /// the active device does not report haptic-feedback hardware.
    pub fn commit_close_button_haptics(&mut self, enabled: bool) {
        if !self.current_haptics_supported() {
            debug!("active device does not support haptic feedback");
            return;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — close-button haptics change ignored");
            return;
        };
        self.config.set_close_button_haptics(&key, enabled);
        self.persist_and_reload("close-button haptics");
    }
}
