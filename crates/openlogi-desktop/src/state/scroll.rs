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
        self.current_record().is_some_and(|record| {
            record
                .persistent_config_key()
                .and_then(|key| self.config.devices.get(key))
                .is_some_and(|device| device.effective_invert_scroll(&record.route_key))
        })
    }
    /// Whether the active device reports native HID++ wheel inversion support.
    #[must_use]
    pub fn current_scroll_inversion_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.scroll_inversion)
    }
    /// Whether the toggle should be offered at all — natively, or through the
    /// capture layer for a device whose firmware has no invert bit (#694,
    /// #776). Requires a device with resolved capabilities either way, so an
    /// unprobed device still reads as unavailable rather than silently
    /// offering a control that would do nothing.
    #[must_use]
    pub fn current_scroll_inversion_available(&self) -> bool {
        self.current_scroll_inversion_supported()
            || (openlogi_hook::SOFTWARE_SCROLL_INVERSION
                && self
                    .current_record()
                    .and_then(|record| record.capabilities)
                    .is_some())
    }
    /// Whether inverting the active device's wheel goes through the capture
    /// layer rather than the firmware — the case the GUI labels as a software
    /// fallback.
    #[must_use]
    pub fn current_scroll_inversion_is_software(&self) -> bool {
        self.current_scroll_inversion_available() && !self.current_scroll_inversion_supported()
    }
    /// Set the active device's scroll-wheel inversion, persist it, and reload
    /// the agent so it applies it — as a native HID++ write where the firmware
    /// supports it, otherwise through the capture layer. No-op when no device
    /// is selected or neither route is available.
    pub fn commit_invert_scroll(&mut self, invert: bool) {
        if !self.current_scroll_inversion_available() {
            debug!("active device cannot invert its scroll wheel");
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
        self.config
            .edit(|config| config.set_invert_scroll(&key, invert));
        self.persist_and_reload("invert scroll");
    }
    /// The active device's persisted wheel resolution, or `None` when OpenLogi
    /// leaves the device default untouched.
    #[must_use]
    pub fn current_scroll_resolution(&self) -> Option<openlogi_core::config::ScrollResolution> {
        self.current_record().and_then(|record| {
            record
                .persistent_config_key()
                .and_then(|key| self.config.devices.get(key))
                .and_then(|device| device.effective_scroll_resolution(&record.route_key))
        })
    }
    /// Whether the active device exposes HID++ `0x2121 HiResWheel`.
    #[must_use]
    pub fn current_hires_wheel_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.hires_wheel)
    }
    /// Whether some *other* link of the active device measured a hi-res wheel.
    ///
    /// A device may expose `0x2121` on one transport and not another, so an
    /// absent capability here is not the same claim as "this device cannot do
    /// it" — and telling a user their mouse lacks a feature it demonstrably
    /// has on its receiver is the confusing half of #660.
    #[must_use]
    pub fn hires_wheel_supported_on_another_link(&self) -> bool {
        let Some(record) = self.current_record() else {
            return false;
        };
        self.config
            .devices
            .get(record.config_key.as_str())
            .is_some_and(|device| {
                device.links.iter().any(|(route, link)| {
                    route != &record.route_key
                        && link.capabilities.is_some_and(|caps| caps.hires_wheel)
                })
            })
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
        if !self
            .config
            .edit(|config| set_scroll_resolution_if_supported(config, &key, supported, resolution))
        {
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

#[cfg(test)]
mod tests {
    use openlogi_core::config::{Config, DeviceConfig, LinkConfig, LinkOverrides};
    use openlogi_core::device::{Capabilities, DeviceKind};

    use crate::services::assets::AssetResolver;
    use crate::state::ConfigPersistence;
    use crate::state::devices::DeviceRecord;

    use super::AppState;

    impl AppState {
        /// Test-only: select a single synthetic record without going through
        /// inventory enumeration, so a test can pin `config_key` / `route_key`
        /// independently of any real HID++ probe.
        fn set_current_record_for_test(&mut self, config_key: &str, route_key: &str) {
            let record = DeviceRecord {
                config_key: config_key.to_string(),
                canonical_key: None,
                persistent: true,
                route_key: route_key.to_string(),
                model_key: config_key.to_string(),
                model_name: "test device".to_string(),
                display_name: "test device".to_string(),
                asset: None,
                model_info: None,
                codename: None,
                serial_number: None,
                unit_id: [0; 4],
                driver_id: None,
                registry_model_id: None,
                route: None,
                capture_id: None,
                kind: DeviceKind::Mouse,
                capabilities: None,
                light_capabilities: None,
                slot: 1,
                online: true,
                battery: None,
            };
            // #974 moved the record list and its selection into one store, so
            // the fixture installs both together.
            self.devices.replace(vec![record], 0);
        }
    }

    /// An in-memory-only `AppState` around `config`, with no live inventory.
    fn test_state(config: Config) -> AppState {
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        AppState::with_runtime(
            config,
            &[],
            &[],
            &cache,
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        )
    }

    /// An `AppState` whose selected device is on the **first** listed link and
    /// whose config records `hires_wheel` per link as given.
    fn state_with_links(links: &[(&str, bool)]) -> AppState {
        let mut device = DeviceConfig::default();
        for (route, hires_wheel) in links {
            device.links.insert(
                (*route).to_string(),
                LinkConfig {
                    capabilities: Some(Capabilities {
                        hires_wheel: *hires_wheel,
                        ..Capabilities::default()
                    }),
                    overrides: LinkOverrides::default(),
                },
            );
        }
        let mut config = Config::default();
        config.devices.insert("unit:6be9d300".to_string(), device);
        let mut state = test_state(config);
        state.set_current_record_for_test("unit:6be9d300", links[0].0);
        state
    }

    #[test]
    fn a_capability_present_on_another_link_is_distinguishable() {
        // A G502 has no hi-res wheel over USB and does over its receiver.
        // "This device does not support wheel resolution control" is wrong for
        // that device; it does, just not on this cable.
        let state = state_with_links(&[
            ("direct:046d:c08d", false),
            ("receiver:82839805:slot:1", true),
        ]);
        assert!(!state.current_hires_wheel_supported());
        assert!(state.hires_wheel_supported_on_another_link());
    }

    #[test]
    fn a_device_that_never_had_it_is_not_excused() {
        let state = state_with_links(&[("direct:046d:b012", false)]);
        assert!(!state.hires_wheel_supported_on_another_link());
    }
}
