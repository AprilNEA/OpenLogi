//! App-level settings (launch-at-login, theme, assets, language).

use super::AppState;
use gpui::App;
use openlogi_core::config::{
    AppSettings, Appearance, AssetSourcePreference, clamp_thumbwheel_sensitivity,
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
        self.config.app_settings.launch_at_login = enabled;
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
        self.config.app_settings.show_in_menu_bar = enabled;
        self.persist_and_reload("show-in-menu-bar setting");
    }
    /// Toggle the opt-in update check and persist it. No immediate side
    /// effect beyond the next launch reading the new value. No-op when
    /// unchanged.
    pub fn set_check_for_updates(&mut self, enabled: bool) {
        if self.config.app_settings.check_for_updates == enabled {
            return;
        }
        self.config.app_settings.check_for_updates = enabled;
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
        self.config.app_settings.auto_install_updates = enabled;
        self.persist_config("auto-install setting");
    }
    /// Persist the light/dark appearance preference. The caller re-applies the
    /// live theme via [`crate::ui::theme::apply_from_settings`]; this only writes the
    /// choice. No-op when unchanged.
    pub fn set_appearance(&mut self, appearance: Appearance) {
        if self.config.app_settings.appearance == appearance {
            return;
        }
        self.config.app_settings.appearance = appearance;
        self.persist_config("appearance setting");
    }
    /// Persist the chosen theme name for one mode (`None` = the OpenLogi brand
    /// theme). No-op when unchanged.
    pub fn set_theme(&mut self, dark: bool, name: Option<String>) {
        let slot = if dark {
            &mut self.config.app_settings.theme_dark
        } else {
            &mut self.config.app_settings.theme_light
        };
        if *slot == name {
            return;
        }
        *slot = name;
        self.persist_config("theme setting");
    }
    /// Persist the UI corner-radius override (`None` = each theme's own radius).
    /// No-op when unchanged.
    pub fn set_ui_radius(&mut self, radius: Option<u8>) {
        if self.config.app_settings.ui_radius == radius {
            return;
        }
        self.config.app_settings.ui_radius = radius;
        self.persist_config("UI radius setting");
    }
    /// Whether OpenLogi manages `key` (capture + volatile re-apply).
    #[must_use]
    pub fn device_enabled(&self, key: &str) -> bool {
        self.config.device_enabled(key)
    }

    /// Enable or disable OpenLogi's management of `key` and persist it. The
    /// agent tears down or re-arms the device's capture session on reload.
    pub fn set_device_enabled(&mut self, key: &str, enabled: bool) {
        if self.config.device_enabled(key) == enabled {
            return;
        }
        self.config.set_device_enabled(key, enabled);
        self.persist_and_reload("device enabled");
    }

    /// The effective thumb-wheel sensitivity for `key` (its per-device
    /// override, else the app-wide default).
    #[must_use]
    pub fn device_thumbwheel_sensitivity(&self, key: &str) -> i32 {
        self.config.thumbwheel_sensitivity(key)
    }

    /// Set `key`'s per-device thumb-wheel sensitivity override (clamped to the
    /// valid range) and persist it. Committing the app-wide default *clears*
    /// the override — the slider is the device's only sensitivity control, so
    /// landing on the default is the "no override" gesture, and the device
    /// goes back to following Settings → General instead of pinning today's
    /// default forever. The agent picks the change up through the reloaded
    /// capture plans. No-op when the stored override would not change.
    pub fn set_device_thumbwheel_sensitivity(&mut self, key: &str, sensitivity: i32) {
        let sensitivity = clamp_thumbwheel_sensitivity(sensitivity);
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
        self.config
            .set_device_thumbwheel_sensitivity(key, override_value);
        self.persist_and_reload("device thumbwheel sensitivity");
    }

    /// Set the app-wide default thumb-wheel sensitivity (clamped to the valid
    /// range) and persist it — devices without a per-device override follow it
    /// through the reloaded capture plans. No-op when unchanged. Disk failures
    /// restore the persisted value and surface a configuration error.
    pub fn set_thumbwheel_sensitivity(&mut self, sensitivity: i32) {
        let sensitivity = clamp_thumbwheel_sensitivity(sensitivity);
        if self.config.app_settings.thumbwheel_sensitivity == sensitivity {
            return;
        }
        self.config.app_settings.thumbwheel_sensitivity = sensitivity;
        self.persist_and_reload("thumbwheel sensitivity");
    }
    pub fn set_auto_download_assets(&mut self, enabled: bool) {
        if self.config.app_settings.auto_download_assets == enabled {
            return;
        }
        self.config.app_settings.auto_download_assets = enabled;
        self.persist_config("auto-download-assets setting");
    }
    /// Persist the preferred device-asset source. The Settings view requests a
    /// refresh separately when automatic downloads are enabled, so this setter
    /// remains side-effect-free beyond configuration I/O.
    pub fn set_asset_source(&mut self, source: AssetSourcePreference) {
        if self.config.app_settings.asset_source == source {
            return;
        }
        self.config.app_settings.asset_source = source;
        self.persist_config("asset-source setting");
    }
    /// Record the answer to the first-run update-check prompt: enable (or leave
    /// disabled) the check, and mark the prompt as seen so it never reappears.
    /// Persists once.
    pub fn record_update_consent(&mut self, enabled: bool) {
        self.config.app_settings.check_for_updates = enabled;
        self.config.app_settings.update_prompt_seen = true;
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
    /// process-global locale via [`openlogi_gui::locale`], and repaint open UI.
    /// No-op when unchanged.
    pub fn set_language(&mut self, language: Option<String>, cx: &mut App) {
        if self.config.app_settings.language == language {
            return;
        }
        self.config.app_settings.language = language;
        self.persist_config("language setting");
        openlogi_gui::locale::activate(self.config.app_settings.language.as_deref());
        cx.refresh_windows();
        crate::app::menu::rebuild(cx);
    }
}
