//! Normalize persisted capability groups into the existing typed protocol settings.
//! The protocol orchestrator remains the only dispatcher for these settings.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};

use super::DeviceConfig;
use crate::peripheral::CapabilityId;

const GROUPS: &[(&str, &[&str])] = &[
    (
        "input-remap/main",
        &[
            "bindings",
            "disabled_gestures",
            "per_app_bindings",
            "action_ring",
        ],
    ),
    ("pointer/main", &["dpi", "dpi_presets"]),
    (
        "wheel/main",
        &[
            "smartshift",
            "invert_scroll",
            "scroll_resolution",
            "thumbwheel_sensitivity",
        ],
    ),
    ("keyboard-lighting/main", &["lighting"]),
    ("light/main", &["light"]),
    (
        "camera/main",
        &["camera_controls", "camera_profiles", "camera_profile"],
    ),
    ("fn-lock/main", &["fn_lock"]),
    ("host-switch/main", &["host_switch_targets"]),
];

pub(super) fn serialize<S>(
    devices: &BTreeMap<String, DeviceConfig>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let documents = devices
        .iter()
        .map(|(key, device)| {
            let mut document = toml::Table::try_from(device).map_err(ser::Error::custom)?;
            let mut capabilities: toml::Table = device
                .retained_capabilities
                .iter()
                .map(|(id, value)| (id.clone(), toml::Value::Table(value.clone())))
                .collect();
            for &(id, fields) in GROUPS {
                let mut group: toml::Table = fields
                    .iter()
                    .filter_map(|field| {
                        document
                            .remove(*field)
                            .map(|value| ((*field).to_owned(), value))
                    })
                    .collect();
                if group.is_empty() {
                    continue;
                }
                if capabilities.contains_key(id) {
                    return Err(ser::Error::custom(format!(
                        "cannot replace unsupported capability {id}"
                    )));
                }
                group.insert("version".into(), toml::Value::Integer(1));
                capabilities.insert(id.into(), toml::Value::Table(group));
            }
            if !capabilities.is_empty() {
                document.insert("capabilities".into(), toml::Value::Table(capabilities));
            }
            Ok((key, document))
        })
        .collect::<Result<BTreeMap<_, _>, S::Error>>()?;
    documents.serialize(serializer)
}

pub(super) fn deserialize<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, DeviceConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let documents = BTreeMap::<String, toml::Table>::deserialize(deserializer)?;
    documents
        .into_iter()
        .map(|(key, mut document)| {
            let mut retained = BTreeMap::new();
            if let Some(value) = document.remove("capabilities") {
                let toml::Value::Table(groups) = value else {
                    return Err(de::Error::custom("capabilities must be a table"));
                };
                if groups.len() > 128 {
                    return Err(de::Error::custom("too many device capabilities"));
                }
                for (id, value) in groups {
                    CapabilityId::try_new(&id).map_err(de::Error::custom)?;
                    let toml::Value::Table(mut group) = value else {
                        return Err(de::Error::custom("capability must be a versioned table"));
                    };
                    let version = group
                        .get("version")
                        .and_then(toml::Value::as_integer)
                        .filter(|v| (1..=i64::from(u32::MAX)).contains(v))
                        .ok_or_else(|| {
                            de::Error::custom(format!("{id} needs a positive version"))
                        })?;
                    let fields = GROUPS
                        .iter()
                        .find(|(known, _)| *known == id)
                        .map(|(_, fields)| *fields);
                    if let Some(fields) = fields
                        && fields.iter().any(|field| document.contains_key(*field))
                    {
                        return Err(de::Error::custom(format!(
                            "{id} duplicates legacy device settings"
                        )));
                    }
                    if version != 1 || fields.is_none() {
                        retained.insert(id, group);
                        continue;
                    }
                    group.remove("version");
                    for (field, value) in group {
                        if !fields.is_some_and(|fields| fields.contains(&field.as_str())) {
                            return Err(de::Error::custom(format!("unknown {id} setting {field}")));
                        }
                        document.insert(field, value);
                    }
                }
            }
            let mut config: DeviceConfig = document.try_into().map_err(de::Error::custom)?;
            config.retained_capabilities = retained;
            Ok((key, config))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn migrates_settings_once_and_preserves_routes_profiles_and_future_contracts() {
        let source = r#"
schema_version = 7
[devices.mouse]
dpi = 1200
bindings = { Back = "Copy" }
per_app_bindings = { app = { Back = "Paste" } }
[devices.mouse.links.usb.overrides]
dpi = 1600
[devices.camera]
camera_controls = { brightness = 32 }
camera_profiles = { custom = { brightness = 48 } }
[devices.light.capabilities."light/main"]
version = 2
zones = ["desk", "wall"]
"#;
        let config: Config = toml::from_str(source).unwrap();
        assert!(config.devices["light"].light.is_none());
        let stored = toml::to_string(&config).unwrap();
        let document: toml::Table = stored.parse().unwrap();
        assert!(document["devices"]["mouse"].get("dpi").is_none());
        assert_eq!(
            document["devices"]["mouse"]["capabilities"]["pointer/main"]["dpi"].as_integer(),
            Some(1200)
        );
        let loaded: Config = toml::from_str(&stored).unwrap();
        assert_eq!(config.devices, loaded.devices);
        assert_eq!(
            loaded.devices["mouse"]
                .effective_dpi("usb")
                .unwrap()
                .into_inner(),
            1600
        );
        assert_eq!(
            loaded.devices["light"].retained_capabilities["light/main"]["zones"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn refuses_two_setting_owners_and_invalid_known_values() {
        for device in [
            "dpi = 800\n[devices.mouse.capabilities.\"pointer/main\"]\nversion = 1\ndpi = 1200",
            "[devices.mouse.capabilities.\"pointer/main\"]\nversion = 1\ndpi = 999999",
            "[devices.mouse.capabilities.\"wheel/main\"]\nversion = 1\nunknown = true",
        ] {
            let source = format!("schema_version = 8\n[devices.mouse]\n{device}");
            assert!(
                toml::from_str::<Config>(&source).is_err(),
                "ambiguous or invalid settings must fail: {source}"
            );
        }
    }
}
