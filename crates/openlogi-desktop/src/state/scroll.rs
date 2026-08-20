//! Per-device scroll inversion and wheel resolution.

use tracing::debug;

use openlogi_core::config::{Config, HorizontalScrollSensitivity};
use openlogi_core::hid::DeviceRoute;

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
    /// Whether the active device reports native HID++ wheel inversion support.
    #[must_use]
    pub fn current_scroll_inversion_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.scroll_inversion)
    }
    /// Set the active device's scroll-wheel inversion, persist it, and reload
    /// the agent so it writes the device's native HID++ wheel inversion. No-op
    /// when no device is selected or the active device does not report support.
    pub fn commit_invert_scroll(&mut self, invert: bool) {
        if !self.current_scroll_inversion_supported() {
            debug!("active device does not support native scroll inversion");
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
    /// Effective native horizontal-scroll sensitivity for the active device.
    /// The default `20` preserves the device's incoming speed.
    #[must_use]
    pub fn current_horizontal_scroll_sensitivity(&self) -> HorizontalScrollSensitivity {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map_or(HorizontalScrollSensitivity::DEFAULT, |key| {
                self.config.horizontal_scroll_sensitivity(key)
            })
    }
    /// Whether the active device's native horizontal axis is reversed.
    #[must_use]
    pub fn current_invert_horizontal_scroll(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.invert_horizontal_scroll(key))
    }
    /// Whether macOS can attribute native horizontal events to this device.
    /// Receiver events expose the receiver identity, not a paired slot, so only
    /// direct devices can safely receive per-device settings. The surrounding
    /// Pointer tab already owns capability gating; this predicate only answers
    /// whether the event tap can route an observed Axis 2 event back to the
    /// selected physical device.
    #[must_use]
    pub fn current_horizontal_scroll_supported(&self) -> bool {
        cfg!(target_os = "macos")
            && self.current_record().is_some_and(|record| {
                record.persistent_config_key().is_some()
                    && matches!(record.route, Some(DeviceRoute::Direct { .. }))
            })
    }
    /// Persist the active device's native horizontal-scroll sensitivity and
    /// reload the agent. No-op when macOS cannot attribute this device.
    pub fn commit_horizontal_scroll_sensitivity(
        &mut self,
        sensitivity: HorizontalScrollSensitivity,
    ) {
        let Some((key, supported)) = self.current_record().and_then(|record| {
            Some((
                record.persistent_config_key()?.to_string(),
                self.current_horizontal_scroll_supported(),
            ))
        }) else {
            debug!("no persistent device key — horizontal-scroll change ignored");
            return;
        };
        if !set_horizontal_scroll_sensitivity_if_supported(
            &mut self.config,
            &key,
            supported,
            sensitivity,
        ) {
            debug!("native horizontal scroll is not attributable to the active device");
            return;
        }
        self.persist_and_reload("horizontal scroll sensitivity");
    }
    /// Persist horizontal direction for the active direct mouse and reload the
    /// agent. Vertical wheel inversion remains independent.
    pub fn commit_invert_horizontal_scroll(&mut self, invert: bool) {
        let Some((key, supported)) = self.current_record().and_then(|record| {
            Some((
                record.persistent_config_key()?.to_string(),
                self.current_horizontal_scroll_supported(),
            ))
        }) else {
            debug!("no persistent device key — horizontal-scroll change ignored");
            return;
        };
        if !set_invert_horizontal_scroll_if_supported(&mut self.config, &key, supported, invert) {
            debug!("native horizontal scroll is not attributable to the active device");
            return;
        }
        self.persist_and_reload("horizontal scroll direction");
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

pub(crate) fn set_horizontal_scroll_sensitivity_if_supported(
    config: &mut Config,
    key: &str,
    supported: bool,
    sensitivity: HorizontalScrollSensitivity,
) -> bool {
    if !supported {
        return false;
    }
    config.set_horizontal_scroll_sensitivity(key, sensitivity);
    true
}

pub(crate) fn set_invert_horizontal_scroll_if_supported(
    config: &mut Config,
    key: &str,
    supported: bool,
    invert: bool,
) -> bool {
    if !supported {
        return false;
    }
    config.set_invert_horizontal_scroll(key, invert);
    true
}
