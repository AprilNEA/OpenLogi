//! App-level settings (launch-at-login, theme, assets, language).

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::{AppState, StateEvent};
use crate::platform::app_icon::AppIconExt as _;
use gpui_component::ThemeMode;
use openlogi_core::config::{
    AppIcon, AppSettings, Appearance, AssetSourcePreference, DeviceViewMode, MouseProfileTarget,
    ThumbwheelSensitivity, UiScale, VerticalScrollSensitivity,
};

impl AppState {
    /// App-wide settings backing the Settings window (launch-at-login,
    /// update check). Read-only view; mutate via the `commit_*` methods below
    /// so the change is persisted.
    #[must_use]
    pub fn app_settings(&self) -> &AppSettings {
        &self.config.app_settings
    }
    /// Toggle launch-at-login by persisting it to `config.toml` — which *is*
    /// the switch: the agent reads it (see `platform::registration`), and on
    /// Linux/Windows reconciles its autostart on config reload. Registration
    /// is only ensured opportunistically here, healing drift without
    /// re-prompting — and giving a dev build its explicit way in. Disk
    /// failures restore the persisted value and surface a config error.
    pub fn commit_launch_at_login(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.launch_at_login == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.launch_at_login = enabled);
        if self.persist_and_reload("launch-at-login setting")
            && let Err(error) = crate::platform::registration::ensure_registered()
        {
            tracing::warn!(error, enabled, "service registration failed");
        }
        StateEvent::SettingsChanged.into()
    }
    /// Toggle the menu-bar (status item) icon preference and persist it. The
    /// icon is hosted by the always-on agent, which reads this on startup and
    /// installs the status item only when enabled — so the change takes effect
    /// the next time the agent launches (a no-restart live toggle would need a
    /// main-thread hop from the agent's IPC reload). `ReloadConfig` keeps the
    /// agent's other config in sync meanwhile. An already-set value writes
    /// nothing and is still reported.
    ///
    /// The callers are the menu-bar / notification-area toggle in Settings,
    /// shown only where there's a tray (macOS + Windows), so the setter is
    /// gated the same way to stay dead-code-clean on Linux.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn commit_show_in_menu_bar(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.show_in_menu_bar == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.show_in_menu_bar = enabled);
        self.persist_and_reload("show-in-menu-bar setting");
        StateEvent::SettingsChanged.into()
    }
    /// Toggle the opt-in update check and persist it. No immediate side effect
    /// beyond the next launch reading the new value. An already-set value
    /// writes nothing and is still reported.
    pub fn commit_check_for_updates(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.check_for_updates == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.check_for_updates = enabled);
        self.persist_config("update-check setting");
        StateEvent::SettingsChanged.into()
    }
    /// Toggle opt-in automatic install and persist it. The launch-time updater
    /// observer reads this live, so a newer version found after this is enabled
    /// downloads and stages on its own; no immediate side effect here. An
    /// already-set value writes nothing and is still reported.
    pub fn commit_auto_install_updates(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.auto_install_updates == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.auto_install_updates = enabled);
        self.persist_config("auto-install setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the light/dark appearance preference. The caller re-applies the
    /// live theme via [`crate::ui::theme::apply_from_settings`]; this only
    /// writes the choice. An already-set value writes nothing and is still
    /// reported.
    pub fn commit_appearance(&mut self, appearance: Appearance) -> StateEvents {
        if self.config.app_settings.appearance == appearance {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.appearance = appearance);
        self.persist_config("appearance setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the text and interface scale. Open window roots apply the new
    /// rem size when the caller refreshes them. An already-set value writes
    /// nothing and is still reported.
    pub fn commit_ui_scale(&mut self, scale: UiScale) -> StateEvents {
        if self.config.app_settings.ui_scale == scale {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.ui_scale = scale);
        self.persist_config("UI scale setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the chosen theme name for one mode's slot (`None` = the OpenLogi
    /// brand theme). An already-set value writes nothing and is still reported.
    pub fn commit_theme(&mut self, mode: ThemeMode, name: Option<String>) -> StateEvents {
        let current = match mode {
            ThemeMode::Light => &self.config.app_settings.theme_light,
            ThemeMode::Dark => &self.config.app_settings.theme_dark,
        };
        if *current == name {
            return StateEvent::SettingsChanged.into();
        }
        self.config.edit(|config| {
            let slot = match mode {
                ThemeMode::Light => &mut config.app_settings.theme_light,
                ThemeMode::Dark => &mut config.app_settings.theme_dark,
            };
            *slot = name;
        });
        self.persist_config("theme setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the chosen app icon and wear it now. Unlike the theme settings
    /// this one leaves the process twice over: the icon is written onto the app
    /// bundle so it survives a quit, and the agent is told so it can restyle
    /// the menu-bar item — the one surface showing an icon that the GUI cannot
    /// reach. An already-set value writes nothing and is still reported.
    pub fn commit_app_icon(&mut self, icon: AppIcon) -> StateEvents {
        if self.config.app_settings.app_icon == icon {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.app_icon = icon);
        // Only wear what the config kept: a failed write rolls the setting
        // back, and an icon applied over that would outlive the choice it came
        // from — Finder would show one thing and Settings another.
        if self.persist_and_reload("app icon setting") {
            icon.apply();
        }
        StateEvent::SettingsChanged.into()
    }
    /// Persist the UI corner-radius override (`None` = each theme's own
    /// radius). An already-set value writes nothing and is still reported.
    pub fn commit_ui_radius(&mut self, radius: Option<u8>) -> StateEvents {
        if self.config.app_settings.ui_radius == radius {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.ui_radius = radius);
        self.persist_config("UI radius setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the Home device-gallery layout. An already-set value writes
    /// nothing and is still reported.
    pub fn commit_device_view_mode(&mut self, mode: DeviceViewMode) -> StateEvents {
        if self.config.app_settings.device_view_mode == mode {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.device_view_mode = mode);
        self.persist_config("device view mode");
        StateEvent::SettingsChanged.into()
    }
    /// Whether OpenLogi manages `key` (capture + volatile re-apply).
    #[must_use]
    pub fn device_enabled(&self, key: &str) -> bool {
        self.config.device_enabled(key)
    }

    /// Set the user-facing name of one gallery device. Whitespace-only names
    /// clear the alias and restore the hardware model name. `record_key` is
    /// distinct per live serial-less camera even though their hardware settings
    /// intentionally share a model-scoped config key.
    pub fn commit_device_custom_name(
        &mut self,
        record_key: &str,
        custom_name: &str,
    ) -> StateEvents {
        let custom_name = match custom_name.trim() {
            "" => None,
            name => Some(name.to_string()),
        };
        if self.config.device_custom_name(record_key) == custom_name.as_deref() {
            return StateEvent::InventoryChanged.into();
        }
        self.config.edit(|config| {
            config.set_device_custom_name(record_key, custom_name.clone());
        });
        if !self.persist_config("device name") {
            return StateEvent::InventoryChanged.into();
        }
        for record in self
            .devices
            .records
            .iter_mut()
            .filter(|record| record.record_key() == record_key)
        {
            record.display_name = custom_name
                .clone()
                .unwrap_or_else(|| record.model_name.clone());
        }
        StateEvent::InventoryChanged.into()
    }

    /// Enable or disable OpenLogi's management of `key` and persist it. The
    /// agent tears down or re-arms the device's capture session on reload.
    pub fn commit_device_enabled(&mut self, key: &DeviceKey, enabled: bool) -> StateEvents {
        let events = StateEvent::DeviceConfigChanged(key.clone()).into();
        let key = key.as_str();
        if self.config.device_enabled(key) == enabled {
            return events;
        }
        self.config
            .edit(|config| config.set_device_enabled(key, enabled));
        self.persist_and_reload("device enabled");
        events
    }

    /// The effective thumb-wheel sensitivity for `key` (its per-device
    /// override, else the app-wide default).
    #[must_use]
    pub fn device_thumbwheel_sensitivity(&self, key: &str) -> ThumbwheelSensitivity {
        self.config.thumbwheel_sensitivity(key)
    }

    /// Set `key`'s per-device thumb-wheel sensitivity override and persist it.
    /// Committing the app-wide default *clears* the override — the slider is
    /// the device's only sensitivity control, so landing on the default is the
    /// "no override" gesture, and the device goes back to following Settings →
    /// General instead of pinning today's default forever. The agent picks the
    /// change up through the reloaded capture plans. A stored override that
    /// would not change writes nothing and is still reported.
    pub fn commit_device_thumbwheel_sensitivity(
        &mut self,
        key: &DeviceKey,
        sensitivity: ThumbwheelSensitivity,
    ) -> StateEvents {
        let events = StateEvent::DeviceConfigChanged(key.clone()).into();
        let key = key.as_str();
        let override_value =
            (sensitivity != self.config.app_settings.thumbwheel_sensitivity).then_some(sensitivity);
        let stored = self
            .config
            .devices
            .get(key)
            .and_then(|d| d.thumbwheel_sensitivity);
        if stored == override_value {
            return events;
        }
        self.config.edit(|config| {
            config.set_device_thumbwheel_sensitivity(key, override_value);
        });
        self.persist_and_reload("device thumbwheel sensitivity");
        events
    }

    /// Set the app-wide default thumb-wheel sensitivity and persist it —
    /// devices without a per-device override follow it through the reloaded
    /// capture plans. An already-set value writes nothing and is still
    /// reported. Disk failures restore the persisted value and surface a
    /// configuration error.
    pub fn commit_thumbwheel_sensitivity(
        &mut self,
        sensitivity: ThumbwheelSensitivity,
    ) -> StateEvents {
        if self.config.app_settings.thumbwheel_sensitivity == sensitivity {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.thumbwheel_sensitivity = sensitivity);
        self.persist_and_reload("thumbwheel sensitivity");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the application target for mouse button profiles and reload the
    /// agent. An unchanged value writes nothing; failed saves restore the
    /// previous selection through the shared configuration rollback boundary.
    pub fn commit_mouse_profile_target(&mut self, target: MouseProfileTarget) -> StateEvents {
        if self.config.app_settings.mouse_profile_target == target {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.mouse_profile_target = target);
        self.persist_and_reload("mouse profile target");
        StateEvent::SettingsChanged.into()
    }
    /// Toggle finite animation for traditional mouse-wheel input and persist
    /// it. The agent publishes the change to the scroll worker on config
    /// reload. An already-set value writes nothing and is still reported; disk
    /// failures restore the persisted value.
    pub fn commit_smooth_scroll(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.smooth_scroll == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.smooth_scroll = enabled);
        self.persist_and_reload("smooth scroll");
        StateEvent::SettingsChanged.into()
    }
    /// Set traditional vertical mouse-wheel sensitivity and persist it. The
    /// agent publishes the value to its scroll worker on config reload. An
    /// already-set value writes nothing and is still reported; disk failures
    /// restore the persisted value.
    pub fn commit_vertical_scroll_sensitivity(
        &mut self,
        sensitivity: VerticalScrollSensitivity,
    ) -> StateEvents {
        if self.config.app_settings.vertical_scroll_sensitivity == sensitivity {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.vertical_scroll_sensitivity = sensitivity);
        self.persist_and_reload("vertical scroll sensitivity");
        StateEvent::SettingsChanged.into()
    }
    pub fn commit_auto_download_assets(&mut self, enabled: bool) -> StateEvents {
        if self.config.app_settings.auto_download_assets == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.auto_download_assets = enabled);
        self.persist_config("auto-download-assets setting");
        StateEvent::SettingsChanged.into()
    }
    /// Persist the preferred device-asset source. The Settings view requests a
    /// refresh separately when automatic downloads are enabled, so this setter
    /// remains side-effect-free beyond configuration I/O.
    pub fn commit_asset_source(&mut self, source: AssetSourcePreference) -> StateEvents {
        if self.config.app_settings.asset_source == source {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.asset_source = source);
        self.persist_config("asset-source setting");
        StateEvent::SettingsChanged.into()
    }
    /// Record the answer to the first-run update-check prompt: enable (or leave
    /// disabled) the check, and mark the prompt as seen so it never reappears.
    /// Persists once.
    pub fn record_update_consent(&mut self, enabled: bool) -> StateEvents {
        self.config.edit(|config| {
            config.app_settings.check_for_updates = enabled;
            config.app_settings.update_prompt_seen = true;
        });
        self.persist_config("update-check consent");
        StateEvent::SettingsChanged.into()
    }
    /// The stored UI-language preference: `Some(code)` for an explicit choice,
    /// `None` for "follow system". Distinct from the *active* locale that
    /// `None` resolves to at startup, so the Settings picker can show "Follow
    /// system" as the selected option.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.config.app_settings.language.as_deref()
    }
    /// Set the UI language (`None` = follow system), persist it, and switch the
    /// process-global locale via [`openlogi_core::locale`]. Emitting the
    /// [`StateEvent::LanguageChanged`] this reports is what repaints open UI.
    /// An already-set value writes nothing and is still reported.
    pub fn commit_language(&mut self, language: Option<String>) -> StateEvents {
        if self.config.app_settings.language == language {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.app_settings.language = language);
        // Reload, not just persist: the agent renders the menu-bar tray in the
        // configured language and relocalizes it on config reload.
        self.persist_and_reload("language setting");
        openlogi_core::locale::activate(self.config.app_settings.language.as_deref());
        StateEvents::from(StateEvent::LanguageChanged).and(StateEvent::SettingsChanged)
    }
}
