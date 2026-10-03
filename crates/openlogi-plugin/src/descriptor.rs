//! Versioned TOML descriptors, validated without device access.

use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::peripheral::{
    ControlId, DescriptorId, DriverId, HidUsage, InputControl, InputSource, ModelId, ScopeKind,
    Transport, Trigger,
};
use openlogi_device_registry::driver::BuiltinDriver;
use openlogi_device_registry::native_remap::{NATIVE_REMAP_DRIVER_ID, NativeRemapDevice};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{PluginError, limits, selector::Selector};

/// A supported desktop host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Apple macOS.
    Macos,
    /// Linux.
    Linux,
    /// Microsoft Windows.
    Windows,
}

impl Platform {
    /// Host platform, unavailable for non-desktop parser targets.
    #[must_use]
    pub const fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else {
            None
        }
    }
}

/// Identity policy declared by a descriptor; runtime evidence comes from its driver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityPolicy {
    /// Permitted evidence source.
    pub strategy: IdentityStrategy,
    /// Disclosed default configuration scope.
    pub default_scope: ScopeKind,
}

/// Descriptor identity policy. Plugins cannot declare an arbitrary serial trustworthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityStrategy {
    /// No verified physical identity.
    Unverified,
    /// A compiled driver verifies protocol identity.
    Builtin,
}

/// Implementation and parameter schema referenced by a descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverBinding {
    /// Installed implementation identity.
    pub id: DriverId,
    /// Parameter-schema version.
    pub parameters: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    id: DescriptorId,
    revision: String,
    model: ModelId,
    name: String,
    driver: DriverBinding,
    platforms: Vec<Platform>,
    identity: IdentityPolicy,
    matches: Vec<Selector>,
    #[serde(default)]
    parameters: toml::Table,
}

/// A validated descriptor envelope. Bind driver parameters before attachment.
#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor(Document);

impl Descriptor {
    /// Bind a grant to the complete normalized descriptor, including matching and parameters.
    pub fn fingerprint(&self) -> Result<String, PluginError> {
        let source =
            toml::to_string(&self.0).map_err(|error| PluginError::Invalid(error.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(source.as_bytes())))
    }
    /// Parse and validate one source transaction.
    pub fn parse(source: &str) -> Result<Self, PluginError> {
        if source.len() > limits::DESCRIPTOR_BYTES {
            return Err(PluginError::Invalid("descriptor exceeds 64 KiB".into()));
        }
        Self::validate(toml::from_str(source)?)
    }

    fn validate(document: Document) -> Result<Self, PluginError> {
        if document.schema != 1 || document.driver.parameters == 0 {
            return Err(PluginError::Invalid(
                "unsupported descriptor or driver parameter schema".into(),
            ));
        }
        document
            .revision
            .parse::<semver::Version>()
            .map_err(|error| PluginError::Invalid(format!("descriptor revision: {error}")))?;
        validate_label(&document.name)?;
        if document.platforms.is_empty()
            || document.platforms.len() != document.platforms.iter().collect::<BTreeSet<_>>().len()
        {
            return Err(PluginError::Invalid(
                "platforms must be nonempty and unique".into(),
            ));
        }
        if document.matches.is_empty() || document.matches.len() > limits::SELECTORS {
            return Err(PluginError::Invalid(
                "descriptor requires 1–64 selectors".into(),
            ));
        }
        for selector in &document.matches {
            selector.validate()?;
        }
        if document.identity.strategy == IdentityStrategy::Unverified
            && document.identity.default_scope == ScopeKind::Physical
        {
            return Err(PluginError::Invalid(
                "an unverified identity cannot use physical scope".into(),
            ));
        }
        Ok(Self(document))
    }

    /// Catalog identity.
    #[must_use]
    pub fn id(&self) -> &DescriptorId {
        &self.0.id
    }
    /// Model identity.
    #[must_use]
    pub fn model(&self) -> &ModelId {
        &self.0.model
    }
    /// Product label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }
    /// Descriptor semantic version.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.0.revision
    }
    /// Selected implementation and parameter schema.
    #[must_use]
    pub fn driver(&self) -> &DriverBinding {
        &self.0.driver
    }
    /// Supported host platforms.
    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        &self.0.platforms
    }
    /// Identity and default scope policy.
    #[must_use]
    pub fn identity(&self) -> &IdentityPolicy {
        &self.0.identity
    }
    /// Exact selector alternatives.
    #[must_use]
    pub fn selectors(&self) -> &[Selector] {
        &self.0.matches
    }
    /// Parameters to validate against the selected driver's schema.
    #[must_use]
    pub fn parameters(&self) -> &toml::Table {
        &self.0.parameters
    }

    /// Normalize a compiled protocol registration through the descriptor validator.
    /// Product facts come from host discovery; driver support comes from the registry.
    pub fn protocol(driver: BuiltinDriver, selector: Selector) -> Result<Self, PluginError> {
        let model = format!("usb.{:04x}.{:04x}", selector.vendor_id, selector.product_id);
        let suffix = if driver == BuiltinDriver::Hidpp {
            format!(
                ".{:04x}.{:04x}.{}",
                selector.usage_page.unwrap_or(0),
                selector.usage.unwrap_or(0),
                match selector.transport {
                    Transport::BluetoothHid => "bluetooth",
                    _ => "usb",
                }
            )
        } else {
            String::new()
        };
        let descriptor = Self::validate(Document {
            schema: 1,
            id: DescriptorId::try_new(format!("{}.{model}{suffix}", driver.id()))?,
            revision: "1.0.0".into(),
            model: ModelId::try_new(model)?,
            name: match driver {
                BuiltinDriver::Hidpp => "Logitech HID++",
                BuiltinDriver::Litra => "Logitech Litra",
                BuiltinDriver::Camera => "UVC camera",
                BuiltinDriver::NativeRemap => {
                    return Err(PluginError::Invalid(
                        "native mapper requires its control parameters".into(),
                    ));
                }
            }
            .into(),
            driver: DriverBinding {
                id: DriverId::try_new(driver.id())?,
                parameters: 1,
            },
            platforms: vec![Platform::Macos, Platform::Linux, Platform::Windows],
            identity: IdentityPolicy {
                strategy: IdentityStrategy::Builtin,
                default_scope: ScopeKind::Physical,
            },
            matches: vec![selector],
            parameters: toml::Table::new(),
        })?;
        descriptor.protocol_parameters()?;
        Ok(descriptor)
    }

    /// Bind an existing protocol implementation without creating another session manager.
    /// V1 accepts no protocol recipes; firmware capabilities remain probe-derived.
    pub fn protocol_parameters(&self) -> Result<BuiltinDriver, PluginError> {
        let driver = BuiltinDriver::find(self.driver().id.as_ref())
            .filter(|driver| *driver != BuiltinDriver::NativeRemap)
            .ok_or_else(|| PluginError::Invalid("unknown built-in protocol".into()))?;
        let role = &self.selectors()[0].role;
        if self.driver().parameters != 1
            || !self.parameters().is_empty()
            || self.identity().strategy != IdentityStrategy::Builtin
            || self.selectors().iter().any(|selector| {
                selector.role != *role
                    || match driver {
                        BuiltinDriver::Camera => selector.transport != Transport::Uvc,
                        BuiltinDriver::Hidpp | BuiltinDriver::Litra => {
                            selector.transport == Transport::Uvc
                                || BuiltinDriver::for_hid(
                                    selector.vendor_id,
                                    selector.product_id,
                                    selector.usage_page.unwrap_or(0),
                                    selector.usage.unwrap_or(0),
                                ) != Some(driver)
                        }
                        BuiltinDriver::NativeRemap => true,
                    }
            })
        {
            return Err(PluginError::Invalid("built-in protocol requires schema 1, empty parameters, built-in identity, and one supported endpoint role".into()));
        }
        Ok(driver)
    }

    /// Produce a descriptor from the single compiled owner of native mapping facts.
    pub fn native(device: &NativeRemapDevice) -> Result<Self, PluginError> {
        let control = NativeControl {
            id: ControlId::try_new(device.control_id)?,
            labels: BTreeMap::from([
                ("en".into(), device.label.into()),
                ("zh-CN".into(), device.label_zh_cn.into()),
            ]),
            source: NativeSource::HidUsage {
                page: device.source_page,
                usage: device.source_usage,
            },
            trigger: Trigger::ShortPress,
            recommended_key: device
                .recommended_key
                .parse()
                .map_err(|error| PluginError::Invalid(format!("recommended key: {error}")))?,
        };
        let parameters = NativeParameters {
            endpoint: ControlId::try_new("controls")?,
            write_scope: WriteScope::VidPid,
            controls: vec![control],
        };
        let parameters = toml::Value::try_from(parameters)
            .map_err(|error| PluginError::Invalid(error.to_string()))?
            .as_table()
            .cloned()
            .ok_or_else(|| PluginError::Invalid("native parameters are not a table".into()))?;
        Self::validate(Document {
            schema: 1,
            id: DescriptorId::try_new(device.descriptor_id)?,
            revision: "1.0.0".into(),
            model: ModelId::try_new(device.model_id)?,
            name: device.name.into(),
            driver: DriverBinding {
                id: DriverId::try_new(NATIVE_REMAP_DRIVER_ID)?,
                parameters: 1,
            },
            platforms: vec![Platform::Macos],
            identity: IdentityPolicy {
                strategy: IdentityStrategy::Unverified,
                default_scope: ScopeKind::Model,
            },
            matches: vec![Selector {
                role: ControlId::try_new("controls")?,
                transport: Transport::UsbHid,
                vendor_id: device.vendor_id,
                product_id: device.product_id,
                usage_page: Some(device.usage_page),
                usage: Some(device.usage),
                interface: None,
                report_id: None,
            }],
            parameters,
        })
    }

    /// Bind native mapping parameters. A descriptor cannot widen the write scope.
    pub fn native_parameters(&self) -> Result<NativeParameters, PluginError> {
        if self.driver().id.as_ref() != NATIVE_REMAP_DRIVER_ID || self.driver().parameters != 1 {
            return Err(PluginError::Invalid(
                "native mapping driver or parameter schema unavailable".into(),
            ));
        }
        let parameters: NativeParameters = self.0.parameters.clone().try_into()?;
        if self.identity().default_scope != ScopeKind::Model
            || self.identity().strategy != IdentityStrategy::Unverified
            || self
                .selectors()
                .iter()
                .any(|s| s.role != parameters.endpoint || s.transport != Transport::UsbHid)
        {
            return Err(PluginError::Invalid(
                "native HID mapping V1 requires one USB HID role and disclosed model scope".into(),
            ));
        }
        parameters.validate()?;
        Ok(parameters)
    }
}

/// Native mapper's supported write scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteScope {
    /// Match every service with this VID/PID; heterogeneous arrays are refused.
    VidPid,
}

/// Validated native mapper parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeParameters {
    /// The descriptor role from which VID/PID are derived.
    pub endpoint: ControlId,
    /// Backend scope that the user must see.
    pub write_scope: WriteScope,
    /// Uniquely sourced short-press controls.
    pub controls: Vec<NativeControl>,
}

impl NativeParameters {
    pub(crate) fn validate(&self) -> Result<(), PluginError> {
        if self.controls.is_empty() || self.controls.len() > limits::FIELDS {
            return Err(PluginError::Invalid(
                "native mapper requires 1–128 controls".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut sources = BTreeSet::new();
        for control in &self.controls {
            if !ids.insert(&control.id) || !sources.insert(control.source.usage()) {
                return Err(PluginError::Invalid(
                    "duplicate control ID or source usage".into(),
                ));
            }
            validate_labels(&control.labels)?;
            if control.trigger != Trigger::ShortPress || !control.recommended_key.is_single_key() {
                return Err(PluginError::Invalid(
                    "native mapper requires short-press controls and single-key targets".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Descriptor source encoding; this format does not cross the internal IPC wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NativeSource {
    /// Standard HID usage.
    HidUsage {
        /// HID usage page.
        page: u16,
        /// HID usage.
        usage: u16,
    },
}

impl NativeSource {
    /// Typed HID pair.
    #[must_use]
    pub const fn usage(self) -> HidUsage {
        match self {
            Self::HidUsage { page, usage } => HidUsage { page, usage },
        }
    }
}

/// Native short-press source and its presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeControl {
    /// Stable control identity.
    pub id: ControlId,
    /// Locale labels with an English fallback.
    pub labels: BTreeMap<String, String>,
    /// Standard HID source, not a virtual keycode.
    pub source: NativeSource,
    /// Verified trigger semantics.
    pub trigger: Trigger,
    /// Recommendation that does not enable writes.
    pub recommended_key: openlogi_core::binding::KeyCombo,
}

impl From<&NativeControl> for InputControl {
    fn from(control: &NativeControl) -> Self {
        Self {
            id: control.id.clone(),
            labels: control.labels.clone(),
            source: InputSource::HidUsage(control.source.usage()),
            trigger: control.trigger,
            recommended_key: Some(control.recommended_key.clone()),
        }
    }
}

pub(crate) fn validate_label(value: &str) -> Result<(), PluginError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(PluginError::Invalid(
            "labels must contain 1–256 bytes without control characters".into(),
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), PluginError> {
    if !labels.contains_key("en") || labels.len() > 32 {
        return Err(PluginError::Invalid(
            "labels require English and at most 32 locales".into(),
        ));
    }
    for (locale, label) in labels {
        if locale.len() > 35
            || locale.is_empty()
            || !locale
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(PluginError::Invalid("invalid label locale".into()));
        }
        validate_label(label)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
schema = 1
id = "example.mic"
revision = "1.0.0"
model = "example.mic"
name = "Test microphone"
driver = { id = "org.openlogi.native-hid-remap", parameters = 1 }
platforms = ["macos"]
identity = { strategy = "unverified", default_scope = "model" }
[[matches]]
role = "controls"
transport = "usb-hid"
vendor_id = 65535
product_id = 1
usage_page = 12
usage = 1
[parameters]
endpoint = "controls"
write_scope = "vid-pid"
[[parameters.controls]]
id = "trigger"
labels = { en = "Trigger" }
source = { kind = "hid-usage", page = 12, usage = 233 }
trigger = "short-press"
recommended_key = "F18"
"#;

    #[test]
    fn native_descriptor_rejects_unsupported_behavior_before_attachment() {
        let control = Descriptor::parse(SOURCE)
            .unwrap()
            .native_parameters()
            .unwrap()
            .controls
            .remove(0);
        assert_eq!(control.recommended_key.rendered_label(), "F18");
        for invalid in [
            SOURCE.replace("short-press", "press-release"),
            SOURCE.replace("F18", "Ctrl+F18"),
            SOURCE.replace("default_scope = \"model\"", "default_scope = \"session\""),
        ] {
            Descriptor::parse(&invalid)
                .unwrap()
                .native_parameters()
                .expect_err("unsupported native behavior must fail before I/O");
        }
        Descriptor::parse(&SOURCE.replace("schema = 1", "schema = 99"))
            .expect_err("future schema rejected");
        Descriptor::parse(&SOURCE.replace("vendor_id = 65535", "vendor_id = 65536"))
            .expect_err("overflow rejected");
    }
}
