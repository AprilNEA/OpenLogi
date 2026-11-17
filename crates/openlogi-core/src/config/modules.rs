//! The `[modules]` table: which installed key modules run, and the settings
//! each one is handed.

use serde::{Deserialize, Serialize};

use super::Config;
use crate::module::ModuleId;

/// One module's `[modules."<id>"]` table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleConfig {
    /// Whether the agent runs the module. Absent means off: a module runs
    /// only once the user turns it on.
    #[serde(default)]
    pub enabled: bool,
    /// Free-form settings, handed to the module verbatim in its `hello`.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub settings: toml::Table,
}

impl Config {
    /// Whether the module `id` is turned on.
    #[must_use]
    pub fn module_enabled(&self, id: &ModuleId) -> bool {
        self.modules.get(id).is_some_and(|module| module.enabled)
    }

    /// Turn the module `id` on or off, keeping its settings. A module turned
    /// off with no settings leaves no table behind.
    pub fn set_module_enabled(&mut self, id: &ModuleId, enabled: bool) {
        let module = self.modules.entry(id.clone()).or_default();
        module.enabled = enabled;
        if !enabled && module.settings.is_empty() {
            self.modules.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> ModuleId {
        ModuleId::try_new(text.to_owned()).expect("valid module id")
    }

    #[test]
    fn a_modules_table_round_trips_with_its_settings() {
        let source = r#"schema_version = 12

[modules."com.example.spotify"]
enabled = true

[modules."com.example.weather"]
enabled = false

[modules."com.example.weather".settings]
city = "Lynchburg"
units = "imperial"
refresh = 300
"#;
        let config: Config = toml::from_str(source).expect("the table parses");
        assert!(config.module_enabled(&id("com.example.spotify")));
        assert!(!config.module_enabled(&id("com.example.weather")));
        assert!(!config.module_enabled(&id("com.example.absent")));
        let weather = &config.modules[&id("com.example.weather")];
        assert_eq!(weather.settings["city"].as_str(), Some("Lynchburg"));
        assert_eq!(weather.settings["refresh"].as_integer(), Some(300));

        let encoded = toml::to_string(&config).expect("config serializes");
        let again: Config = toml::from_str(&encoded).expect("its own output parses");
        assert_eq!(again.modules, config.modules);
    }

    #[test]
    fn a_module_table_refuses_unknown_keys_and_bad_ids() {
        toml::from_str::<Config>(
            "schema_version = 12\n[modules.\"com.example.x\"]\nenable = true\n",
        )
        .expect_err("a misspelt key is refused");
        toml::from_str::<Config>("schema_version = 12\n[modules.Spotify]\nenabled = true\n")
            .expect_err("an id that is not reverse-DNS is refused");
    }

    #[test]
    fn turning_a_module_off_keeps_its_settings_and_drops_an_empty_table() {
        let mut config = Config::default();
        let spotify = id("com.example.spotify");
        config.set_module_enabled(&spotify, true);
        assert!(config.module_enabled(&spotify));
        config.set_module_enabled(&spotify, false);
        assert!(config.modules.is_empty(), "nothing left to keep");

        config.set_module_enabled(&spotify, true);
        config
            .modules
            .get_mut(&spotify)
            .expect("enabled above")
            .settings
            .insert("volume".into(), toml::Value::Integer(3));
        config.set_module_enabled(&spotify, false);
        assert!(!config.module_enabled(&spotify));
        assert_eq!(config.modules[&spotify].settings.len(), 1);
    }

    #[test]
    fn a_config_without_modules_writes_no_table() {
        let encoded = toml::to_string(&Config::default()).expect("config serializes");
        assert!(!encoded.contains("modules"), "{encoded}");
    }
}
