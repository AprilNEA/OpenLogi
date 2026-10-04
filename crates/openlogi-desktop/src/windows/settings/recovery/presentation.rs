//! User-facing recovery summaries. TOML paths remain available in details.

use crate::ui::action::localized_action_label;
use gpui::SharedString;
use openlogi_core::{
    binding::{Action, ButtonId},
    config::{Appearance, ConfigChange, RecoveryPlan, UiScale},
};

pub(super) fn context(plan: &RecoveryPlan, change: &ConfigChange) -> String {
    let keys = &change.keys;
    if keys.first().is_some_and(|key| key == "devices") {
        let key = keys.get(1).map_or("", String::as_str);
        let profile = keys
            .iter()
            .position(|key| key == "per_app_bindings")
            .and_then(|index| keys.get(index + 1))
            .map_or_else(|| tr!("profiles.default_profile").to_string(), Clone::clone);
        format!("{} · {profile}", plan.device_name(key))
    } else {
        tr!("app.settings").to_string()
    }
}

pub(super) fn title(change: &ConfigChange) -> String {
    let keys = &change.keys;
    let start = keys
        .iter()
        .position(|key| key == "bindings")
        .map(|index| index + 1)
        .or_else(|| {
            keys.iter()
                .position(|key| key == "per_app_bindings")
                .map(|index| index + 2)
        })
        .unwrap_or_else(|| {
            usize::from(keys.first().is_some_and(|key| key == "app_settings"))
                + 2 * usize::from(keys.first().is_some_and(|key| key == "devices"))
        });
    keys.iter()
        .skip(start)
        .map(|key| field(key).to_string())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(super) fn value(value: Option<&toml::Value>, keys: &[String]) -> String {
    let Some(value) = value else {
        return tr!("recovery.absent").to_string();
    };
    if let [scope, setting] = keys
        && scope == "app_settings"
        && let Some(label) = setting_value(setting, value)
    {
        return label;
    }
    if keys
        .iter()
        .any(|key| matches!(key.as_str(), "bindings" | "per_app_bindings"))
        && let Ok(action) = value.clone().try_into::<Action>()
    {
        match &action {
            Action::RunShellCommand(command) | Action::RunAppleScript(command) => {
                return format!("{}: {command}", localized_action_label(&action));
            }
            // Steps must remain visible: equal step counts do not mean equal behavior.
            Action::Workflow(_) => {}
            _ => return localized_action_label(&action).to_string(),
        }
    }
    match value {
        toml::Value::Boolean(true) => tr!("common.on").to_string(),
        toml::Value::Boolean(false) => tr!("common.off").to_string(),
        toml::Value::String(text) => text.clone(),
        toml::Value::Array(items) => items
            .iter()
            .map(|item| self::value(Some(item), keys))
            .collect::<Vec<_>>()
            .join(", "),
        toml::Value::Table(table) => table
            .iter()
            .map(|(key, item)| format!("{}: {}", field(key), self::value(Some(item), keys)))
            .collect::<Vec<_>>()
            .join("; "),
        _ => value.to_string(),
    }
}

fn setting_value(setting: &str, value: &toml::Value) -> Option<String> {
    match setting {
        "appearance" => {
            let appearance = value.clone().try_into::<Appearance>().ok()?;
            Some(match appearance {
                Appearance::Light => tr!("common.light").to_string(),
                Appearance::Dark => tr!("appearance.dark").to_string(),
                Appearance::System => tr!("appearance.follow_system").to_string(),
            })
        }
        "ui_scale" => value
            .clone()
            .try_into::<UiScale>()
            .ok()
            .map(|scale| format!("{}%", scale.percent())),
        "language" => openlogi_core::locale::SUPPORTED
            .iter()
            .find_map(|(code, name)| (Some(*code) == value.as_str()).then(|| (*name).to_owned())),
        _ => None,
    }
}

fn field(key: &str) -> SharedString {
    if let Ok(button) = key.parse::<ButtonId>() {
        return tr!(button.translation_key());
    }
    match key {
        "dpi" | "current_dpi" => tr!("pointer.dpi"),
        "launch_at_login" => tr!("app.launch_at_login"),
        "appearance" | "theme_light" | "theme_dark" => tr!("appearance.theme"),
        "language" => tr!("appearance.language"),
        "ui_scale" => tr!("appearance.interface_scale"),
        "smooth_scroll" => tr!("pointer.smooth_scrolling"),
        "display_name" | "custom_name" => tr!("recovery.device_name"),
        "identity" => tr!("recovery.device_identity"),
        "Up" => tr!("common.up"),
        "Down" => tr!("common.down"),
        "Left" => tr!("common.left"),
        "Right" => tr!("common.right"),
        "Click" => tr!("common.click"),
        _ => {
            let text = key.replace('_', " ");
            let mut chars = text.chars();
            chars
                .next()
                .map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::i18n::LOCALE_LOCK;

    #[test]
    fn recovery_settings_use_readable_values_without_changing_unrelated_strings() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("zh-CN");
        for (setting, stored, display) in [
            ("appearance", "dark", "深色"),
            ("appearance", "light", "浅色"),
            ("ui_scale", "extra_large", "125%"),
            ("ui_scale", "normal", "100%"),
            ("language", "zh-CN", "简体中文"),
            ("language", "en", "English"),
            ("language", "unknown-locale", "unknown-locale"),
        ] {
            let keys = ["app_settings".into(), setting.into()];
            let stored = toml::Value::String(stored.into());
            assert_eq!(value(Some(&stored), &keys), display);
            assert_eq!(value(Some(&stored), &[]), stored.as_str().unwrap());
        }
        assert_eq!(field("ui_scale"), "界面缩放");
        rust_i18n::set_locale("en");
    }

    #[test]
    fn recovery_labels_keep_quoted_identity_separate_and_show_platform_shortcuts() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let change = ConfigChange {
            path: "devices.\"unit:with.dots\".per_app_bindings.\"com.apple.Safari\".Back".into(),
            keys: [
                "devices",
                "unit:with.dots",
                "per_app_bindings",
                "com.apple.Safari",
                "Back",
            ]
            .map(str::to_owned)
            .to_vec(),
            before: None,
            after: Some(
                toml::Value::try_from(Action::CustomShortcut("Super+C".parse().unwrap())).unwrap(),
            ),
        };
        assert_eq!(
            title(&change),
            tr!(openlogi_core::binding::ButtonId::Back.translation_key()).to_string()
        );
        assert_eq!(
            value(change.after.as_ref(), &change.keys),
            if cfg!(target_os = "macos") {
                "Cmd+C"
            } else {
                "Super+C"
            }
        );
        assert_eq!(value(Some(&toml::Value::Boolean(false)), &[]), "Off");
        let old = toml::Value::try_from(Action::RunShellCommand("echo old".into())).unwrap();
        let new = toml::Value::try_from(Action::RunShellCommand("echo new".into())).unwrap();
        assert_ne!(
            value(Some(&old), &change.keys),
            value(Some(&new), &change.keys)
        );
        assert!(value(Some(&new), &change.keys).contains("echo new"));
        rust_i18n::set_locale("zh-CN");
        assert_eq!(field("language"), tr!("appearance.language"));
        rust_i18n::set_locale("en");
    }
}
