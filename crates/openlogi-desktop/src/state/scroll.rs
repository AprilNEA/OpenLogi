//! Per-device scroll inversion and wheel resolution.

use tracing::debug;

use openlogi_core::config::Config;

use crate::state::devices::DeviceRecord;

use super::AppState;

impl AppState {
    /// Whether the active device's scroll wheel is inverted (issue #126).
    /// `false` when no device is selected or the device hasn't opted in.
    #[must_use]
    pub fn current_invert_scroll(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.invert_scroll(key))
    }
    /// Whether the active device's wheel can be inverted at all — natively over
    /// HID++ `0x2121`, or in software where the hook can rewrite scroll deltas.
    ///
    /// macOS rewrites the `CGEvent`, so inversion is offered for any pointing
    /// device there, including firmware with no `0x2121` (MX Vertical, whose
    /// toggle would otherwise read "Unavailable" forever). evdev and
    /// `WH_MOUSE_LL` have no rewrite path, so there this stays gated on the
    /// native capability. No capability snapshot yet means `false` everywhere.
    #[must_use]
    pub fn current_scroll_inversion_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| {
                capabilities.scroll_inversion || (cfg!(target_os = "macos") && capabilities.pointer)
            })
    }
    /// Set the active device's scroll-wheel inversion, persist it, and reload the
    /// agent so it either writes the device's native HID++ wheel inversion or
    /// republishes the hook's software-inversion set. No-op when no device is
    /// selected or the active device does not support inversion either way.
    pub fn commit_invert_scroll(&mut self, invert: bool) {
        if !self.current_scroll_inversion_supported() {
            debug!("active device supports neither native nor software scroll inversion");
            return;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — invert-scroll change ignored");
            return;
        };
        self.config.set_invert_scroll(&key, invert);
        self.persist_and_reload("invert scroll");
    }
    /// The active device's persisted wheel resolution, or `None` when OpenLogi
    /// leaves the device default untouched.
    #[must_use]
    pub fn current_scroll_resolution(&self) -> Option<openlogi_core::config::ScrollResolution> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.scroll_resolution(key))
    }
    /// Whether the active device exposes HID++ `0x2121 HiResWheel`.
    #[must_use]
    pub fn current_hires_wheel_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.hires_wheel)
    }
    /// Persist the active device's wheel resolution and ask the agent to reload
    /// it. `None` removes OpenLogi's override. No-op without a selected,
    /// HiResWheel-capable device.
    pub fn commit_scroll_resolution(
        &mut self,
        resolution: Option<openlogi_core::config::ScrollResolution>,
    ) {
        let Some((key, supported)) = self.current_record().and_then(|record| {
            let key = record.persistent_config_key()?.to_string();
            Some((
                key,
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.hires_wheel),
            ))
        }) else {
            debug!("no persistent device key — wheel-resolution change ignored");
            return;
        };
        if !set_scroll_resolution_if_supported(&mut self.config, &key, supported, resolution) {
            debug!("active device does not support HiResWheel");
            return;
        }
        self.persist_and_reload("wheel resolution");
    }
}

pub(crate) fn set_scroll_resolution_if_supported(
    config: &mut Config,
    key: &str,
    supported: bool,
    resolution: Option<openlogi_core::config::ScrollResolution>,
) -> bool {
    if !supported {
        return false;
    }
    config.set_scroll_resolution(key, resolution);
    true
}
