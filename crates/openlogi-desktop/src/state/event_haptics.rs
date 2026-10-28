//! Per-device button and wheel haptics.

use openlogi_core::config::{EventHapticFeedback, EventHapticInput, EventHapticsConfig};
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device has a haptic engine (`0x19b0`) for event
    /// haptics to play on. An unprobed device falls back to its kind.
    #[must_use]
    pub fn current_haptic_feedback_supported(&self) -> bool {
        self.current_record().is_some_and(|record| {
            record
                .capabilities
                .unwrap_or_else(|| {
                    openlogi_core::device::Capabilities::presumed_from_kind(record.kind)
                })
                .haptic_feedback
        })
    }

    /// The active device's event haptics settings: what each button and the
    /// wheel play. The defaults when no device is selected.
    #[must_use]
    pub fn current_event_haptics_config(&self) -> EventHapticsConfig {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.event_haptics(key))
            .unwrap_or_default()
    }

    /// Turn the active device's button and wheel haptics on or off, persist
    /// it, and reload the agent so it starts or stops playing them.
    /// Committing the current value saves and reloads nothing.
    pub fn commit_event_haptics(&mut self, enabled: bool) -> StateEvents {
        self.edit_event_haptics(
            "event haptics",
            |haptics| haptics.enabled == enabled,
            |config, key| config.set_event_haptics_enabled(key, enabled),
        )
    }

    /// Choose what `input` plays on the active device, and play it once
    /// there so the choice is felt, not read.
    pub fn commit_event_haptic(
        &mut self,
        input: EventHapticInput,
        feedback: EventHapticFeedback,
    ) -> StateEvents {
        if feedback.plays()
            && let Some(route) = self
                .current_record()
                .and_then(|record| record.route.clone())
        {
            self.send_ipc(crate::services::ipc::PlayHaptic { route, feedback });
        }
        self.edit_event_haptics(
            "event haptics feedback",
            |haptics| haptics.feedback(input) == feedback,
            |config, key| config.set_event_haptic_feedback(key, input, feedback),
        )
    }

    /// Apply `edit` to the active device's event haptics unless `unchanged`
    /// says it already holds, then persist and reload the agent.
    fn edit_event_haptics(
        &mut self,
        what: &'static str,
        unchanged: impl FnOnce(&EventHapticsConfig) -> bool,
        edit: impl FnOnce(&mut openlogi_core::config::Config, &str),
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(what, "no persistent device key — change ignored");
            return events;
        };
        if unchanged(&self.config.event_haptics(&key)) {
            return events;
        }
        self.config.edit(|config| edit(config, &key));
        self.persist_and_reload(what);
        events
    }
}
