//! Read-only onboard-profile (HID++ `0x8100`) button-binding display state.
//! The query itself is an swr-backed read owned by the device-read service —
//! this module only starts it for the selected device and exposes the
//! result. There is no write path: G-series gaming mice expose no
//! `ReprogControls`, so there is nothing here to remap yet, only to show.

use gpui::Context;

use super::AppState;
use super::devices::DeviceRecord;
use super::load::OnboardProfileBindingsLoad;

impl AppState {
    pub(super) fn load_current_onboard_profile_bindings(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_onboard_profile_bindings(key, route, self.ipc_sender(), cx);
    }

    /// The active device's onboard-profile bindings load state. UI helper.
    #[must_use]
    pub fn onboard_profile_bindings(&self) -> OnboardProfileBindingsLoad {
        self.current_record()
            .map(DeviceRecord::device_key)
            .map_or(OnboardProfileBindingsLoad::Unknown, |key| {
                self.pointer.reads.onboard_profile_bindings_status(&key)
            })
    }
}
