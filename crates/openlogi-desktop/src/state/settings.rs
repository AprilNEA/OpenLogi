//! App-level settings (launch-at-login, theme, assets, language).

use super::AppState;
use gpui::App;
use openlogi_core::config::{
    AppIcon, AppSettings, Appearance, AssetSourcePreference, DeviceViewMode, ThumbwheelSensitivity,
    UiScale, VerticalScrollSensitivity,
};

impl AppState {
    /// App-wide settings backing the Settings window (launch-at-login,
    /// update check). Read-only view; mutate via the setters below so the
    /// change is persisted.
    #[must_use]
    pub fn app_settings(&self) -> &AppSettings {
        &self.config.app_settings
    }
    /// Toggle launch-at-login, persist to `config.toml`, and reconcile the
    /// macOS `LaunchAgent` plist so the change takes effect without a
    /// restart. No-op when the value is unchanged. Disk failures restore the
    /// persisted value and surface a configuration error without crashing.
    pub fn set_launch_at_login(&mut self, enabled: bool) {
        if self.config.app_settings.launch_at_login == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.launch_at_login = enabled);
        // The agent owns autostart now; it reconciles its LaunchAgent (which
        // points at the agent, not the GUI) when it reloads the config.
        self.persist_and_reload("launch-at-login setting");
    }
    /// Toggle the menu-bar (status item) icon preference and persist it. The
    /// icon is hosted by the always-on agent, which reads this on startup and
    /// installs the status item only when enabled — so the change takes effect
    /// the next time the agent launches (a no-restart live toggle would need a
    /// main-thread hop from the agent's IPC reload). `ReloadConfig` keeps the
    /// agent's other config in sync meanwhile. No-op when unchanged.
    ///
    /// The callers are the menu-bar / notification-area toggle in Settings,
    /// shown only where there's a tray (macOS + Windows), so the setter is
    /// gated the same way to stay dead-code-clean on Linux.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn set_show_in_menu_bar(&mut self, enabled: bool) {
        if self.config.app_settings.show_in_menu_bar == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.show_in_menu_bar = enabled);
        self.persist_and_reload("show-in-menu-bar setting");
    }
    /// Toggle the opt-in update check and persist it. No immediate side
    /// effect beyond the next launch reading the new value. No-op when
    /// unchanged.
    pub fn set_check_for_updates(&mut self, enabled: bool) {
        if self.config.app_settings.check_for_updates == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.check_for_updates = enabled);
        self.persist_config("update-check setting");
    }
    /// Toggle opt-in automatic install and persist it. The launch-time updater
    /// observer reads this live, so a newer version found after this is enabled
    /// downloads and stages on its own; no immediate side effect here. No-op
    /// when unchanged.
    pub fn set_auto_install_updates(&mut self, enabled: bool) {
        if self.config.app_settings.auto_install_updates == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.auto_install_updates = enabled);
        self.persist_config("auto-install setting");
    }
    /// Persist the light/dark appearance preference. The caller re-applies the
    /// live theme via [`crate::ui::theme::apply_from_settings`]; this only writes the
    /// choice. No-op when unchanged.
    pub fn set_appearance(&mut self, appearance: Appearance) {
        if self.config.app_settings.appearance == appearance {
            return;
        }
        self.config
            .edit(|config| config.app_settings.appearance = appearance);
        self.persist_config("appearance setting");
    }
    /// Persist the text and interface scale. Open window roots apply the new
    /// rem size when the caller refreshes them. No-op when unchanged.
    pub fn set_ui_scale(&mut self, scale: UiScale) {
        if self.config.app_settings.ui_scale == scale {
            return;
        }
        self.config
            .edit(|config| config.app_settings.ui_scale = scale);
        self.persist_config("UI scale setting");
    }
    /// Persist the chosen theme name for one mode (`None` = the OpenLogi brand
    /// theme). No-op when unchanged.
    pub fn set_theme(&mut self, dark: bool, name: Option<String>) {
        let current = if dark {
            &self.config.app_settings.theme_dark
        } else {
            &self.config.app_settings.theme_light
        };
        if *current == name {
            return;
        }
        self.config.edit(|config| {
            let slot = if dark {
                &mut config.app_settings.theme_dark
            } else {
                &mut config.app_settings.theme_light
            };
            *slot = name;
        });
        self.persist_config("theme setting");
    }
    /// Persist the chosen app icon and wear it now. Unlike the theme settings
    /// this one leaves the process twice over: the icon is written onto the app
    /// bundle so it survives a quit, and the agent is told so it can restyle the
    /// menu-bar item — the one surface showing an icon that the GUI cannot
    /// reach. No-op when unchanged.
    pub fn set_app_icon(&mut self, icon: AppIcon) {
        if self.config.app_settings.app_icon == icon {
            return;
        }
        self.config
            .edit(|config| config.app_settings.app_icon = icon);
        // Only wear what the config kept: a failed write rolls the setting
        // back, and an icon applied over that would outlive the choice it came
        // from — Finder would show one thing and Settings another.
        if self.persist_and_reload("app icon setting") {
            crate::platform::app_icon::apply(icon);
        }
    }
    /// Persist the UI corner-radius override (`None` = each theme's own radius).
    /// No-op when unchanged.
    pub fn set_ui_radius(&mut self, radius: Option<u8>) {
        if self.config.app_settings.ui_radius == radius {
            return;
        }
        self.config
            .edit(|config| config.app_settings.ui_radius = radius);
        self.persist_config("UI radius setting");
    }
    /// Persist the Home device-gallery layout. No-op when unchanged.
    pub fn set_device_view_mode(&mut self, mode: DeviceViewMode) {
        if self.config.app_settings.device_view_mode == mode {
            return;
        }
        self.config
            .edit(|config| config.app_settings.device_view_mode = mode);
        self.persist_config("device view mode");
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
    pub fn set_device_custom_name(&mut self, record_key: &str, custom_name: &str) {
        let custom_name = match custom_name.trim() {
            "" => None,
            name => Some(name.to_string()),
        };
        if self.config.device_custom_name(record_key) == custom_name.as_deref() {
            return;
        }
        self.config.edit(|config| {
            config.set_device_custom_name(record_key, custom_name.clone());
        });
        if !self.persist_config("device name") {
            return;
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
    }

    /// Enable or disable OpenLogi's management of `key` and persist it. The
    /// agent tears down or re-arms the device's capture session on reload.
    pub fn set_device_enabled(&mut self, key: &str, enabled: bool) {
        if self.config.device_enabled(key) == enabled {
            return;
        }
        self.config
            .edit(|config| config.set_device_enabled(key, enabled));
        self.persist_and_reload("device enabled");
    }

    /// The effective thumb-wheel sensitivity for `key` (its per-device
    /// override, else the app-wide default).
    #[must_use]
    pub fn device_thumbwheel_sensitivity(&self, key: &str) -> ThumbwheelSensitivity {
        self.config.thumbwheel_sensitivity(key)
    }

    /// Set `key`'s per-device thumb-wheel sensitivity override and persist it.
    /// Committing the app-wide default *clears*
    /// the override — the slider is the device's only sensitivity control, so
    /// landing on the default is the "no override" gesture, and the device
    /// goes back to following Settings → General instead of pinning today's
    /// default forever. The agent picks the change up through the reloaded
    /// capture plans. No-op when the stored override would not change.
    pub fn set_device_thumbwheel_sensitivity(
        &mut self,
        key: &str,
        sensitivity: ThumbwheelSensitivity,
    ) {
        let override_value =
            (sensitivity != self.config.app_settings.thumbwheel_sensitivity).then_some(sensitivity);
        let stored = self
            .config
            .devices
            .get(key)
            .and_then(|d| d.thumbwheel_sensitivity);
        if stored == override_value {
            return;
        }
        self.config.edit(|config| {
            config.set_device_thumbwheel_sensitivity(key, override_value);
        });
        self.persist_and_reload("device thumbwheel sensitivity");
    }

    /// Set the app-wide default thumb-wheel sensitivity and persist it —
    /// devices without a per-device override follow it
    /// through the reloaded capture plans. No-op when unchanged. Disk failures
    /// restore the persisted value and surface a configuration error.
    pub fn set_thumbwheel_sensitivity(&mut self, sensitivity: ThumbwheelSensitivity) {
        if self.config.app_settings.thumbwheel_sensitivity == sensitivity {
            return;
        }
        self.config
            .edit(|config| config.app_settings.thumbwheel_sensitivity = sensitivity);
        self.persist_and_reload("thumbwheel sensitivity");
    }
    /// Toggle finite animation for traditional mouse-wheel input and persist
    /// it. The agent publishes the change to the scroll worker on config
    /// reload. No-op when unchanged; disk failures restore the persisted value.
    pub fn set_smooth_scroll(&mut self, enabled: bool) {
        if self.config.app_settings.smooth_scroll == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.smooth_scroll = enabled);
        self.persist_and_reload("smooth scroll");
    }
    /// Set traditional vertical mouse-wheel sensitivity and persist it. The
    /// agent publishes the value to its scroll worker on config reload. No-op
    /// when unchanged; disk failures restore the persisted value.
    pub fn set_vertical_scroll_sensitivity(&mut self, sensitivity: VerticalScrollSensitivity) {
        if self.config.app_settings.vertical_scroll_sensitivity == sensitivity {
            return;
        }
        self.config
            .edit(|config| config.app_settings.vertical_scroll_sensitivity = sensitivity);
        self.persist_and_reload("vertical scroll sensitivity");
    }
    pub fn set_auto_download_assets(&mut self, enabled: bool) {
        if self.config.app_settings.auto_download_assets == enabled {
            return;
        }
        self.config
            .edit(|config| config.app_settings.auto_download_assets = enabled);
        self.persist_config("auto-download-assets setting");
    }
    /// Persist the preferred device-asset source. The Settings view requests a
    /// refresh separately when automatic downloads are enabled, so this setter
    /// remains side-effect-free beyond configuration I/O.
    pub fn set_asset_source(&mut self, source: AssetSourcePreference) {
        if self.config.app_settings.asset_source == source {
            return;
        }
        self.config
            .edit(|config| config.app_settings.asset_source = source);
        self.persist_config("asset-source setting");
    }
    /// Record the answer to the first-run update-check prompt: enable (or leave
    /// disabled) the check, and mark the prompt as seen so it never reappears.
    /// Persists once.
    pub fn record_update_consent(&mut self, enabled: bool) {
        self.config.edit(|config| {
            config.app_settings.check_for_updates = enabled;
            config.app_settings.update_prompt_seen = true;
        });
        self.persist_config("update-check consent");
    }
    /// The stored UI-language preference: `Some(code)` for an explicit choice,
    /// `None` for "follow system". Distinct from the *active* locale that
    /// `None` resolves to at startup, so the Settings picker can show "Follow
    /// system" as the selected option.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.config.app_settings.language.as_deref()
    }
    /// Set the UI language (`None` = follow system), persist it, switch the
    /// process-global locale via [`openlogi_ui::locale`], and repaint open UI.
    /// No-op when unchanged.
    pub fn set_language(&mut self, language: Option<String>, cx: &mut App) {
        if self.config.app_settings.language == language {
            return;
        }
        self.config
            .edit(|config| config.app_settings.language = language);
        self.persist_config("language setting");
        openlogi_ui::locale::activate(self.config.app_settings.language.as_deref());
        // Locale lookup is process-global, so every open window must repaint.
        cx.refresh_windows();
        // Deferred: the Device menu reads this entity, whose lease is still
        // held here (callers switch language inside `AppState::update`), and a
        // re-entrant read panics.
        cx.defer(crate::app::menu::rebuild);
    }
}
