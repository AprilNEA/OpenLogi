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
        self.config
            .edit(|config| config.set_invert_scroll(&key, invert));
        self.persist_and_reload("invert scroll");
    }
    /// Whether hold-to-scroll-horizontally (issue #1053) is effective for the
    /// active device in the open profile scope: the per-app override when the
    /// binding panels are editing one, else the device default. `false` with
    /// no selected device.
    #[must_use]
    pub fn current_side_button_hscroll(&self) -> bool {
        let app = self.editing_app().map(str::to_string);
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| {
                self.config
                    .effective_side_button_horizontal_scroll(key, app.as_deref())
            })
    }
    /// The explicit per-app hold-to-scroll-horizontally override for the open
    /// profile scope, if the user stored one. `None` means the app inherits
    /// the device default — or that no app scope is open at all.
    #[must_use]
    pub fn current_side_button_hscroll_override(&self) -> Option<bool> {
        let app = self.editing_app()?;
        let key = self.current_record()?.persistent_config_key()?;
        self.config.per_app_side_button_hscroll(key, app)
    }
    /// The display name of the app whose profile scope is open, if any.
    #[must_use]
    pub fn side_button_hscroll_scope_name(&self) -> Option<String> {
        let app = self.editing_app()?;
        Some(
            self.recent_app_name(app)
                .map_or_else(|| app.to_string(), str::to_string),
        )
    }
    /// Commit hold-to-scroll-horizontally for the active device: into the open
    /// app profile's override when the binding panels are editing one, else
    /// into the device default. Persists and reloads the agent. No-op without
    /// a selected persistent device.
    pub fn commit_side_button_hscroll(&mut self, enabled: bool) {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        let app = self.editing_app().map(str::to_string);
        let Some(key) = key else {
            debug!("no persistent device key — side-button scroll change ignored");
            return;
        };
        self.config.edit(|config| match &app {
            Some(app) => config.set_per_app_side_button_hscroll(&key, app, Some(enabled)),
            None => config.set_side_button_horizontal_scroll(&key, enabled),
        });
        self.persist_and_reload("side-button horizontal scroll");
    }
    /// Clear the open app profile's hold-to-scroll-horizontally override so it
    /// inherits the device default again. No-op without an open app scope.
    pub fn clear_side_button_hscroll_override(&mut self) {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string);
        let app = self.editing_app().map(str::to_string);
        let (Some(key), Some(app)) = (key, app) else {
            debug!("no open app scope — side-button scroll reset ignored");
            return;
        };
        self.config.edit(|config| {
            config.set_per_app_side_button_hscroll(&key, &app, None);
        });
        self.persist_and_reload("side-button horizontal scroll");
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

    fn state_with_mouse() -> AppState {
        let mut config = Config::default();
        config
            .devices
            .insert("unit:6be9d300".to_string(), DeviceConfig::default());
        let mut state = test_state(config);
        state.set_current_record_for_test("unit:6be9d300", "receiver:AA00:slot:1");
        state
    }

    #[test]
    fn side_button_hscroll_commits_the_device_default() {
        let mut state = state_with_mouse();
        assert!(!state.current_side_button_hscroll());
        assert_eq!(state.current_side_button_hscroll_override(), None);

        state.commit_side_button_hscroll(true);
        assert!(state.current_side_button_hscroll());
        assert!(state.config.side_button_horizontal_scroll("unit:6be9d300"));
    }

    #[test]
    fn side_button_hscroll_in_an_app_scope_commits_an_override() {
        let mut state = state_with_mouse();
        state.set_editing_app(Some("com.example.Editor".to_string()));
        assert!(!state.current_side_button_hscroll());

        state.commit_side_button_hscroll(true);
        assert!(state.current_side_button_hscroll());
        assert_eq!(state.current_side_button_hscroll_override(), Some(true));
        assert!(
            !state.config.side_button_horizontal_scroll("unit:6be9d300"),
            "the device default must stay untouched"
        );

        state.clear_side_button_hscroll_override();
        assert_eq!(state.current_side_button_hscroll_override(), None);
        assert!(!state.current_side_button_hscroll());
    }

    #[test]
    fn side_button_hscroll_commits_are_no_ops_without_a_device() {
        let mut state = test_state(Config::default());
        state.commit_side_button_hscroll(true);
        state.clear_side_button_hscroll_override();
        assert!(!state.current_side_button_hscroll());
        assert_eq!(state.current_side_button_hscroll_override(), None);
        assert_eq!(state.side_button_hscroll_scope_name(), None);
    }
}
