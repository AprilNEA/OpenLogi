//! Persistent model identities and connection-local endpoint addresses.

use nutype::nutype;
use serde::{Deserialize, Serialize};

/// Invalid stable identifier supplied by configuration or a plugin.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("identifier must contain 1–128 ASCII letters, digits, '.', '-', or '_'")]
pub struct IdentifierError;

fn validate_id(value: &str) -> Result<(), IdentifierError> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
        && value != "."
        && value != ".."
    {
        Ok(())
    } else {
        Err(IdentifierError)
    }
}

/// Stable model identity, shared by every unit of that model.
#[nutype(validate(with = validate_id, error = IdentifierError), derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TryFrom, AsRef, Display, Serialize, Deserialize))]
pub struct ModelId(String);

/// Stable implementation identity, independent of the selected model.
#[nutype(validate(with = validate_id, error = IdentifierError), derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TryFrom, AsRef, Display, Serialize, Deserialize))]
pub struct DriverId(String);

/// Stable catalog registration identity.
#[nutype(validate(with = validate_id, error = IdentifierError), derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TryFrom, AsRef, Display, Serialize, Deserialize))]
pub struct DescriptorId(String);

/// Stable control identity within one capability.
#[nutype(validate(with = validate_id, error = IdentifierError), derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TryFrom, AsRef, Display, Serialize, Deserialize))]
pub struct ControlId(String);

/// Stable capability identity: a family and instance separated by '/'.
#[nutype(validate(with = validate_capability_id, error = IdentifierError), derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TryFrom, AsRef, Display, Serialize, Deserialize))]
pub struct CapabilityId(String);

fn validate_capability_id(value: &str) -> Result<(), IdentifierError> {
    let (family, instance) = value.split_once('/').ok_or(IdentifierError)?;
    validate_id(family)?;
    validate_id(instance)
}

/// Verified persistent unit identity. The driver supplies the evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalDeviceId {
    /// Existing namespaced physical key, such as a Logitech protocol unit ID.
    pub key: String,
    /// Why this value can identify one unit.
    pub evidence: IdentityEvidence,
}

/// Evidence required before treating metadata as a physical identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityEvidence {
    /// A protocol explicitly supplies a unique unit identity.
    Protocol,
    /// The driver has verified the model's serial-number uniqueness policy.
    VerifiedSerial,
}

/// OS-local endpoint address. This value must not become a physical config key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EndpointId(pub String);

/// One claim during one attachment. Generations never cross agent restarts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId {
    /// Primary endpoint in the claimed group.
    pub endpoint: EndpointId,
    /// Agent-assigned attachment generation.
    pub generation: u64,
}

/// Transport supported by a host discovery adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// A USB HID collection.
    UsbHid,
    /// A Bluetooth HID collection.
    BluetoothHid,
    /// A video control endpoint; media streaming stays outside the plugin ABI.
    Uvc,
}

/// A HID usage pair, distinct from a virtual keycode or a HID++ CID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidUsage {
    /// HID usage page.
    pub page: u16,
    /// HID usage within the page.
    pub usage: u16,
}

/// Host observation before a driver is selected. Unknown facts stay absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Connection-local address.
    pub id: EndpointId,
    /// Verified parent relation, when the discovery adapter supplies one.
    pub parent: Option<EndpointId>,
    /// Transport, absent when the host cannot distinguish USB from Bluetooth.
    pub transport: Option<Transport>,
    /// Manufacturer's USB/HID identifier.
    pub vendor_id: u16,
    /// Model's USB/HID identifier.
    pub product_id: u16,
    /// Top-level HID collection, absent for UVC.
    pub collection: Option<HidUsage>,
    /// USB interface number, if exposed by this host.
    pub interface: Option<u8>,
    /// Report IDs, if exposed by this host.
    pub report_ids: Vec<u8>,
    /// Maximum input report length including the report ID, if known.
    pub max_report_bytes: Option<u32>,
    /// Maximum output report length including the report ID, if known.
    pub max_output_report_bytes: Option<u32>,
    /// Maximum feature report length including the report ID, if known.
    pub max_feature_report_bytes: Option<u32>,
}

/// Configuration scope supported by a capability backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// All units of a model.
    Model,
    /// One verified unit.
    Physical,
    /// Only the current attachment.
    Session,
}

/// Explicit configuration target. Runtime addresses are never persisted as units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigScope {
    /// Apply to every matching model instance.
    Model(ModelId),
    /// Apply to a verified namespaced unit identity.
    Physical(String),
    /// Apply only while the selected attachment remains alive.
    Session(SessionId),
}

impl ConfigScope {
    /// The scope kind independently of its target.
    #[must_use]
    pub const fn kind(&self) -> ScopeKind {
        match self {
            Self::Model(_) => ScopeKind::Model,
            Self::Physical(_) => ScopeKind::Physical,
            Self::Session(_) => ScopeKind::Session,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ConfigScope", rename_all = "lowercase")]
enum ScopeWire {
    Model(ModelId),
    Physical(String),
    Session(SessionId),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum ScopeDocument {
    Model { model: ModelId },
    Physical { physical: String },
    Session { session: SessionId },
}

impl Serialize for ConfigScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !serializer.is_human_readable() {
            return ScopeWire::serialize(self, serializer);
        }
        let document = match self {
            Self::Model(model) => ScopeDocument::Model {
                model: model.clone(),
            },
            Self::Physical(physical) => ScopeDocument::Physical {
                physical: physical.clone(),
            },
            Self::Session(session) => ScopeDocument::Session {
                session: session.clone(),
            },
        };
        document.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ConfigScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            return ScopeWire::deserialize(deserializer);
        }
        Ok(match ScopeDocument::deserialize(deserializer)? {
            ScopeDocument::Model { model } => Self::Model(model),
            ScopeDocument::Physical { physical } => Self::Physical(physical),
            ScopeDocument::Session { session } => Self::Session(session),
        })
    }
}
