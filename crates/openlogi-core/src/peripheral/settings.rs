//! Bounded scalar settings and desired peripheral state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CapabilityId, ConfigScope, ControlId, DescriptorId, PeripheralError};
use crate::binding::Action;

/// A scalar value at the plugin boundary. Nested command payloads are excluded.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    /// Boolean preference.
    Boolean(bool),
    /// Bounded signed integer.
    Integer(i64),
    /// Finite, bounded numeric quantity.
    Number(f64),
    /// Enum tag or bounded text, validated against its field.
    Text(String),
}

/// A declared setting's allowed values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SettingType {
    /// Boolean value.
    Boolean,
    /// Inclusive integer range.
    Integer {
        /// Minimum value.
        minimum: i64,
        /// Maximum value.
        maximum: i64,
    },
    /// Inclusive finite number range.
    Number {
        /// Minimum value.
        minimum: f64,
        /// Maximum value.
        maximum: f64,
        /// Human-readable unit.
        unit: String,
    },
    /// One of the declared unique tags.
    Enum(Vec<String>),
    /// UTF-8 text, bounded in bytes.
    Text {
        /// Maximum UTF-8 byte length.
        max_bytes: u32,
    },
}

/// Field access is enforced by the host before dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingAccess {
    /// Published by the driver; cannot be configured.
    ReadOnly,
    /// User-configurable setting.
    #[default]
    ReadWrite,
}

/// Validated field metadata used by descriptors and generic controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    /// Allowed value type and bounds.
    pub value_type: SettingType,
    /// Locale labels, including an English fallback.
    pub labels: BTreeMap<String, String>,
    /// Optional default, subject to the same bounds as user values.
    pub default: Option<SettingValue>,
    /// Whether a user can write the field.
    pub access: SettingAccess,
}

impl SettingField {
    /// Reject malformed values at the configuration or guest boundary.
    pub fn validate(&self, value: &SettingValue) -> Result<(), PeripheralError> {
        let valid = match (&self.value_type, value) {
            (SettingType::Boolean, SettingValue::Boolean(_)) => true,
            (SettingType::Integer { minimum, maximum }, SettingValue::Integer(value)) => {
                (minimum..=maximum).contains(&value)
            }
            (
                SettingType::Number {
                    minimum, maximum, ..
                },
                SettingValue::Number(value),
            ) => value.is_finite() && (minimum..=maximum).contains(&value),
            (SettingType::Enum(tags), SettingValue::Text(value)) => tags.contains(value),
            (SettingType::Text { max_bytes }, SettingValue::Text(value)) => {
                value.len() <= *max_bytes as usize && !value.chars().any(char::is_control)
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(PeripheralError::InvalidSettings(
                "value does not satisfy the declared field".into(),
            ))
        }
    }
}

/// Desired settings for one capability. Unknown versions remain inert and retained.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySettings {
    /// Driver contract version that interprets these values.
    pub version: u32,
    /// User-selected input bindings; labels and raw usage values are not keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<ControlId, Action>,
    /// Bounded custom scalar settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, SettingValue>,
}

/// A saved peripheral rule; disabling retains settings for the next enable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeripheralConfig {
    /// Stable rule identity.
    pub id: String,
    /// Whether desired settings should be applied.
    pub enabled: bool,
    /// Explicit model, unit, or attachment scope.
    pub scope: ConfigScope,
    /// Explicit selection; an unavailable descriptor never silently falls back.
    pub descriptor: DescriptorId,
    /// Desired settings keyed by semantic capability identity.
    #[serde(default)]
    pub capabilities: BTreeMap<CapabilityId, CapabilitySettings>,
}

/// Exact content selected for a dynamic driver and its settings schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSelection {
    /// Explicit user enablement; a pinned but disabled package cannot attach.
    pub enabled: bool,
    /// Package SHA-256 content identity; grants are stored separately by the agent.
    pub digest: String,
    /// Schema used by persisted settings.
    pub settings_schema: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_fields_reject_nonfinite_out_of_range_and_wrong_types() {
        let field = SettingField {
            value_type: SettingType::Number {
                minimum: 0.5,
                maximum: 2.0,
                unit: "s".into(),
            },
            labels: BTreeMap::new(),
            default: None,
            access: SettingAccess::ReadWrite,
        };
        field.validate(&SettingValue::Number(1.5)).unwrap();
        for value in [
            SettingValue::Number(f64::NAN),
            SettingValue::Number(f64::INFINITY),
            SettingValue::Number(3.0),
            SettingValue::Integer(1),
        ] {
            field
                .validate(&value)
                .expect_err("invalid plugin setting must not reach a driver");
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "SettingValue")]
enum ValueWire {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ValueDocument {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
}

impl Serialize for SettingValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !serializer.is_human_readable() {
            return ValueWire::serialize(self, serializer);
        }
        match self {
            Self::Boolean(value) => serializer.serialize_bool(*value),
            Self::Integer(value) => serializer.serialize_i64(*value),
            Self::Number(value) => serializer.serialize_f64(*value),
            Self::Text(value) => serializer.serialize_str(value),
        }
    }
}

impl<'de> Deserialize<'de> for SettingValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            return ValueWire::deserialize(deserializer);
        }
        Ok(match ValueDocument::deserialize(deserializer)? {
            ValueDocument::Boolean(value) => Self::Boolean(value),
            ValueDocument::Integer(value) => Self::Integer(value),
            ValueDocument::Number(value) => Self::Number(value),
            ValueDocument::Text(value) => Self::Text(value),
        })
    }
}
