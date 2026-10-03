//! Plugin requests and bounded scalar schemas. A request is never a grant.

use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::peripheral::{
    ControlId, DriverId, SettingAccess, SettingField, SettingType, SettingValue,
};
use serde::{Deserialize, Serialize};

use crate::{
    PluginError,
    descriptor::{Descriptor, Platform, validate_labels},
    limits,
};

/// The component world supported independently of OpenLogi's IPC version.
pub const WORLD: &str = "openlogi:peripheral/driver@1.0.0";

/// Distinct HID operations, with payload lengths excluding the report ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HidOperation {
    /// Subscribe to input reports.
    Input,
    /// Write an output report.
    Output,
    /// Read a feature report.
    FeatureRead,
    /// Write a feature report.
    FeatureWrite,
}

/// Requested access to a single declared endpoint role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "service", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Permission {
    /// Raw HID access remains exclusive to the claimed endpoint.
    Hid {
        /// Descriptor role.
        endpoint: ControlId,
        /// Separately granted operation kinds.
        operations: Vec<HidOperation>,
        /// Permitted report IDs. Zero means an unnumbered report.
        report_ids: Vec<u8>,
        /// Maximum length including the report ID.
        max_report_bytes: u32,
    },
    /// Typed native effects; source controls come from descriptor parameters.
    NativeRemap {
        /// Descriptor role.
        endpoint: ControlId,
        /// Approved native source controls; the guest cannot invent a source.
        controls: Vec<crate::descriptor::NativeControl>,
    },
}

impl Permission {
    /// Human-readable scope shown before the user grants this package access.
    #[must_use]
    pub fn disclosure(&self) -> String {
        match self {
            Self::Hid {
                endpoint,
                operations,
                report_ids,
                max_report_bytes,
            } => {
                let operations = operations
                    .iter()
                    .map(|operation| match operation {
                        HidOperation::Input => "receive input reports",
                        HidOperation::Output => "write output reports",
                        HidOperation::FeatureRead => "read feature reports",
                        HidOperation::FeatureWrite => "write feature reports",
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "HID endpoint {endpoint}: {operations}; report IDs {report_ids:?}; at most {max_report_bytes} bytes per report"
                )
            }
            Self::NativeRemap { endpoint, controls } => {
                let sources = controls
                    .iter()
                    .map(|control| {
                        let source = control.source.usage();
                        format!(
                            "{} ({:#06x}/{:#06x})",
                            control.id, source.page, source.usage
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Native key mappings for {endpoint}: {sources}; applies to all devices with the matched VID/PID"
                )
            }
        }
    }

    /// Endpoint role to which the permission is confined.
    #[must_use]
    pub fn endpoint(&self) -> &ControlId {
        match self {
            Self::Hid { endpoint, .. } | Self::NativeRemap { endpoint, .. } => endpoint,
        }
    }

    fn validate(&self) -> Result<(), PluginError> {
        if let Self::NativeRemap { endpoint, controls } = self {
            crate::descriptor::NativeParameters {
                endpoint: endpoint.clone(),
                write_scope: crate::descriptor::WriteScope::VidPid,
                controls: controls.clone(),
            }
            .validate()?;
        }
        if let Self::Hid {
            operations,
            report_ids,
            max_report_bytes,
            ..
        } = self
            && (operations.is_empty()
                || operations.len() != operations.iter().collect::<BTreeSet<_>>().len()
                || report_ids.is_empty()
                || report_ids.len() != report_ids.iter().collect::<BTreeSet<_>>().len()
                || !(1..=65536).contains(max_report_bytes))
        {
            return Err(PluginError::Invalid(
                "HID permissions require unique operations/report IDs and a 1–65536 byte limit"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    id: DriverId,
    version: String,
    world: String,
    entry: String,
    descriptors: Vec<String>,
    platforms: Vec<Platform>,
    parameter_schema: u32,
    settings_schema: u32,
    #[serde(default)]
    permissions: Vec<Permission>,
    #[serde(default)]
    parameters: BTreeMap<String, Field>,
    #[serde(default)]
    settings: BTreeMap<String, Field>,
}

/// Validated package metadata. Executable bytes are validated separately.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Stable implementation identity.
    pub id: DriverId,
    /// Package version.
    pub version: semver::Version,
    /// Relative component filename.
    pub entry: String,
    /// Complete list of referenced device descriptors.
    pub descriptors: Vec<String>,
    /// Supported platforms.
    pub platforms: Vec<Platform>,
    /// Descriptor parameter schema.
    pub parameter_schema: u32,
    /// Persisted plugin settings schema.
    pub settings_schema: u32,
    /// Requested device operations, inactive until granted for this digest.
    pub permissions: Vec<Permission>,
    /// Descriptor scalar parameters.
    pub parameters: BTreeMap<String, SettingField>,
    /// User scalar settings.
    pub settings: BTreeMap<String, SettingField>,
}

impl Manifest {
    /// Parse a manifest without opening or executing the component.
    pub fn parse(source: &str) -> Result<Self, PluginError> {
        if source.len() > limits::DESCRIPTOR_BYTES {
            return Err(PluginError::Invalid("manifest exceeds 64 KiB".into()));
        }
        let doc: Document = toml::from_str(source)?;
        if doc.schema != 1
            || doc.world != WORLD
            || doc.parameter_schema == 0
            || doc.settings_schema == 0
        {
            return Err(PluginError::Invalid(
                "unsupported manifest, component world, or settings schema".into(),
            ));
        }
        if doc.platforms.is_empty()
            || doc.platforms.len() != doc.platforms.iter().collect::<BTreeSet<_>>().len()
            || doc.descriptors.is_empty()
            || doc.descriptors.len() > limits::SELECTORS
            || doc.descriptors.len() != doc.descriptors.iter().collect::<BTreeSet<_>>().len()
            || doc.permissions.len() > limits::FIELDS
        {
            return Err(PluginError::Invalid(
                "platforms, descriptors, or permissions exceed the contract".into(),
            ));
        }
        validate_relative_path(&doc.entry)?;
        if std::path::Path::new(&doc.entry)
            .extension()
            .is_none_or(|ext| ext != "wasm")
        {
            return Err(PluginError::Invalid(
                "entry must be a .wasm component".into(),
            ));
        }
        for path in &doc.descriptors {
            validate_relative_path(path)?;
            if !path.ends_with(".device.toml") {
                return Err(PluginError::Invalid(
                    "descriptor must end in .device.toml".into(),
                ));
            }
        }
        let mut services = BTreeSet::new();
        let mut native_controls = BTreeSet::new();
        for permission in &doc.permissions {
            permission.validate()?;
            if let Permission::NativeRemap { controls, .. } = permission {
                if doc.platforms != [Platform::Macos] {
                    return Err(PluginError::Invalid(
                        "native mapping requires the macOS host service".into(),
                    ));
                }
                for control in controls {
                    if !native_controls.insert(&control.id) {
                        return Err(PluginError::Invalid(
                            "native control IDs must be unique across endpoint roles".into(),
                        ));
                    }
                }
            }
            let service = matches!(permission, Permission::Hid { .. });
            if !services.insert((permission.endpoint(), service)) {
                return Err(PluginError::Invalid(
                    "duplicate endpoint service request".into(),
                ));
            }
        }
        Ok(Self {
            id: doc.id,
            version: doc
                .version
                .parse()
                .map_err(|e| PluginError::Invalid(format!("plugin version: {e}")))?,
            entry: doc.entry,
            descriptors: doc.descriptors,
            platforms: doc.platforms,
            parameter_schema: doc.parameter_schema,
            settings_schema: doc.settings_schema,
            permissions: doc.permissions,
            parameters: fields(doc.parameters)?,
            settings: fields(doc.settings)?,
        })
    }

    /// Bind a descriptor to this exact implementation contract.
    pub fn validate_descriptor(
        &self,
        descriptor: &Descriptor,
    ) -> Result<BTreeMap<String, SettingValue>, PluginError> {
        if descriptor.driver().id != self.id
            || descriptor.driver().parameters != self.parameter_schema
            || descriptor
                .platforms()
                .iter()
                .any(|p| !self.platforms.contains(p))
            || self.permissions.iter().any(|p| {
                !descriptor
                    .selectors()
                    .iter()
                    .any(|s| &s.role == p.endpoint())
            })
            || descriptor.identity().strategy != crate::descriptor::IdentityStrategy::Unverified
        {
            return Err(PluginError::Invalid(
                "descriptor and plugin contracts disagree".into(),
            ));
        }
        for permission in &self.permissions {
            if let Permission::NativeRemap { endpoint, .. } = permission
                && (descriptor.identity().default_scope
                    != openlogi_core::peripheral::ScopeKind::Model
                    || descriptor
                        .selectors()
                        .iter()
                        .filter(|s| &s.role == endpoint)
                        .any(|s| s.transport != openlogi_core::peripheral::Transport::UsbHid))
            {
                return Err(PluginError::Invalid(
                    "native mapping requires a USB HID role and model scope".into(),
                ));
            }
        }
        let values = descriptor
            .parameters()
            .iter()
            .map(|(key, value)| Ok((key.clone(), scalar(value)?)))
            .collect::<Result<_, PluginError>>()?;
        validate_values(&self.parameters, &values, false)
    }
}

/// Reject absolute, parent, platform-prefix, and ambiguous package paths.
pub(crate) fn validate_relative_path(path: &str) -> Result<(), PluginError> {
    if path.is_empty()
        || path.len() > 256
        || path.contains(['\\', ':'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.chars().any(char::is_control)
    {
        return Err(PluginError::Invalid(
            "package paths must be relative without traversal".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    #[serde(rename = "type")]
    kind: String,
    minimum: Option<toml::Value>,
    maximum: Option<toml::Value>,
    unit: Option<String>,
    tags: Option<Vec<String>>,
    max_bytes: Option<u32>,
    labels: BTreeMap<String, String>,
    default: Option<toml::Value>,
    #[serde(default)]
    access: SettingAccess,
}

fn fields(raw: BTreeMap<String, Field>) -> Result<BTreeMap<String, SettingField>, PluginError> {
    if raw.len() > limits::FIELDS {
        return Err(PluginError::Invalid("more than 128 fields".into()));
    }
    raw.into_iter()
        .map(|(key, field)| {
            ControlId::try_new(key.clone())?;
            validate_labels(&field.labels)?;
            let value_type = field_type(&key, &field)?;
            let default = field.default.as_ref().map(scalar).transpose()?;
            let field = SettingField {
                value_type,
                labels: field.labels,
                default,
                access: field.access,
            };
            if let Some(value) = &field.default {
                field
                    .validate(value)
                    .map_err(|e| PluginError::Invalid(e.to_string()))?;
            }
            Ok((key, field))
        })
        .collect()
}

fn scalar(value: &toml::Value) -> Result<SettingValue, PluginError> {
    match value {
        toml::Value::Boolean(v) => Ok(SettingValue::Boolean(*v)),
        toml::Value::Integer(v) => Ok(SettingValue::Integer(*v)),
        toml::Value::Float(v) if v.is_finite() => Ok(SettingValue::Number(*v)),
        toml::Value::String(v) => Ok(SettingValue::Text(v.clone())),
        _ => Err(PluginError::Invalid(
            "parameters and settings must be bounded scalar values".into(),
        )),
    }
}

/// Validate user values and fill declared defaults at the trust boundary.
pub fn validate_values(
    fields: &BTreeMap<String, SettingField>,
    values: &BTreeMap<String, SettingValue>,
    user_write: bool,
) -> Result<BTreeMap<String, SettingValue>, PluginError> {
    if values.keys().any(|key| !fields.contains_key(key)) {
        return Err(PluginError::Invalid("unknown field".into()));
    }
    let mut resolved = BTreeMap::new();
    for (key, field) in fields {
        if user_write && field.access == SettingAccess::ReadOnly && values.contains_key(key) {
            return Err(PluginError::Invalid(format!("field {key} is read-only")));
        }
        let value = values.get(key).or(field.default.as_ref());
        if let Some(value) = value {
            field
                .validate(value)
                .map_err(|e| PluginError::Invalid(format!("{key}: {e}")))?;
            resolved.insert(key.clone(), value.clone());
        } else if field.access == SettingAccess::ReadWrite {
            return Err(PluginError::Invalid(format!(
                "required field {key} is missing"
            )));
        }
    }
    Ok(resolved)
}

fn field_type(key: &str, field: &Field) -> Result<SettingType, PluginError> {
    let invalid = || PluginError::Invalid(format!("invalid field schema: {key}"));
    Ok(match field.kind.as_str() {
        "boolean"
            if field.minimum.is_none()
                && field.maximum.is_none()
                && field.tags.is_none()
                && field.max_bytes.is_none()
                && field.unit.is_none() =>
        {
            SettingType::Boolean
        }
        "integer" if field.tags.is_none() && field.max_bytes.is_none() && field.unit.is_none() => {
            let minimum = field
                .minimum
                .as_ref()
                .and_then(toml::Value::as_integer)
                .ok_or_else(invalid)?;
            let maximum = field
                .maximum
                .as_ref()
                .and_then(toml::Value::as_integer)
                .ok_or_else(invalid)?;
            if minimum > maximum {
                return Err(invalid());
            }
            SettingType::Integer { minimum, maximum }
        }
        "number" if field.tags.is_none() && field.max_bytes.is_none() => {
            let number = |v: &toml::Value| match v {
                toml::Value::Float(n) => Some(*n),
                _ => None,
            };
            let minimum = field
                .minimum
                .as_ref()
                .and_then(number)
                .ok_or_else(invalid)?;
            let maximum = field
                .maximum
                .as_ref()
                .and_then(number)
                .ok_or_else(invalid)?;
            let unit = field.unit.clone().ok_or_else(invalid)?;
            if !minimum.is_finite()
                || !maximum.is_finite()
                || minimum > maximum
                || unit.len() > 32
                || unit.chars().any(char::is_control)
            {
                return Err(invalid());
            }
            SettingType::Number {
                minimum,
                maximum,
                unit,
            }
        }
        "enum"
            if field.minimum.is_none()
                && field.maximum.is_none()
                && field.max_bytes.is_none()
                && field.unit.is_none() =>
        {
            let tags = field.tags.clone().ok_or_else(invalid)?;
            if tags.is_empty()
                || tags.len() > limits::FIELDS
                || tags.len() != tags.iter().collect::<BTreeSet<_>>().len()
            {
                return Err(invalid());
            }
            for tag in &tags {
                ControlId::try_new(tag.clone())?;
            }
            SettingType::Enum(tags)
        }
        "text"
            if field.minimum.is_none()
                && field.maximum.is_none()
                && field.tags.is_none()
                && field.unit.is_none() =>
        {
            let max_bytes = field.max_bytes.ok_or_else(invalid)?;
            if !(1..=4096).contains(&max_bytes) {
                return Err(invalid());
            }
            SettingType::Text { max_bytes }
        }
        _ => return Err(invalid()),
    })
}
