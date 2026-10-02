//! Device-scoped native HID mapping access. This module never synthesizes input.

use std::collections::BTreeMap;

use openlogi_core::binding::KeyboardUsage;
use openlogi_core::peripheral::{HidUsage, PeripheralError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use macos::NativeMapping;
#[cfg(not(target_os = "macos"))]
pub use unsupported::NativeMapping;

const SOURCE: &str = "HIDKeyboardModifierMappingSrc";
const DESTINATION: &str = "HIDKeyboardModifierMappingDst";

/// The complete device scope accepted by the native setter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MappingScope {
    /// Exact USB/HID vendor ID.
    pub vendor_id: u16,
    /// Exact USB/HID product ID.
    pub product_id: u16,
}

/// Native UserKeyMapping encoding, separate from every OS virtual keycode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MappingUsage(u64);

impl From<HidUsage> for MappingUsage {
    fn from(value: HidUsage) -> Self {
        Self((u64::from(value.page) << 32) | u64::from(value.usage))
    }
}

impl From<KeyboardUsage> for MappingUsage {
    fn from(value: KeyboardUsage) -> Self {
        Self::from(HidUsage {
            page: 7,
            usage: u16::from(value.code()),
        })
    }
}

/// One native entry, including fields owned by other software.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Value", into = "Value")]
pub struct MappingEntry {
    source: MappingUsage,
    destination: MappingUsage,
    fields: Map<String, Value>,
}

impl MappingEntry {
    /// Construct an entry for a typed source and keyboard target.
    #[must_use]
    pub fn new(source: MappingUsage, destination: MappingUsage) -> Self {
        Self {
            source,
            destination,
            fields: Map::new(),
        }
    }

    /// Source effect key.
    #[must_use]
    pub const fn source(&self) -> MappingUsage {
        self.source
    }

    /// Preserve all entry fields while changing only the destination.
    #[must_use]
    pub fn with_destination(&self, destination: MappingUsage) -> Self {
        let mut next = self.clone();
        next.destination = destination;
        next
    }
}

impl TryFrom<Value> for MappingEntry {
    type Error = PeripheralError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let Value::Object(mut fields) = value else {
            return Err(PeripheralError::ReadFailed(
                "native mapping entry is not a dictionary".into(),
            ));
        };
        let source = fields.remove(SOURCE).and_then(|value| value.as_u64());
        let destination = fields.remove(DESTINATION).and_then(|value| value.as_u64());
        match (source, destination) {
            (Some(source), Some(destination)) => Ok(Self {
                source: MappingUsage(source),
                destination: MappingUsage(destination),
                fields,
            }),
            _ => Err(PeripheralError::ReadFailed(
                "native mapping source or destination is not an unsigned integer".into(),
            )),
        }
    }
}

impl From<MappingEntry> for Value {
    fn from(mut value: MappingEntry) -> Self {
        value
            .fields
            .insert(SOURCE.into(), Value::from(value.source.0));
        value
            .fields
            .insert(DESTINATION.into(), Value::from(value.destination.0));
        Self::Object(value.fields)
    }
}

/// Current ordered mapping array. Unrelated entries survive each transformation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MappingArray(Vec<MappingEntry>);

impl MappingArray {
    /// Look up one effect, refusing duplicate entries for the owned source.
    pub fn entry(&self, source: MappingUsage) -> Result<Option<&MappingEntry>, PeripheralError> {
        let mut entries = self.0.iter().filter(|entry| entry.source == source);
        let entry = entries.next();
        if entries.next().is_some() {
            return Err(PeripheralError::MappingScopeConflict);
        }
        Ok(entry)
    }

    /// Replace or remove one source without changing other entries or their order.
    pub fn replacing(
        &self,
        source: MappingUsage,
        value: Option<MappingEntry>,
    ) -> Result<Self, PeripheralError> {
        self.entry(source)?;
        if value.as_ref().is_some_and(|entry| entry.source != source) {
            return Err(PeripheralError::InvalidSettings(
                "replacement mapping has a different source".into(),
            ));
        }
        let mut next = self.clone();
        match (
            next.0.iter().position(|entry| entry.source == source),
            value,
        ) {
            (Some(index), Some(entry)) => next.0[index] = entry,
            (Some(index), None) => {
                next.0.remove(index);
            }
            (None, Some(entry)) => next.0.push(entry),
            (None, None) => {}
        }
        Ok(next)
    }
}

/// Temporary evidence for a native effect's lifetime, never a configuration key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEvidence {
    /// Boot identity supplied by the OS.
    pub boot: String,
    /// Live service IDs and their current arrays, including every scoped service.
    pub services: BTreeMap<u64, MappingArray>,
}

impl ServiceEvidence {
    /// Return the shared array only when a model-scoped write can preserve all services.
    pub fn common(&self) -> Result<&MappingArray, PeripheralError> {
        let mut arrays = self.services.values();
        let first = arrays.next().ok_or(PeripheralError::Offline)?;
        if arrays.any(|array| array != first) {
            return Err(PeripheralError::MappingScopeConflict);
        }
        Ok(first)
    }
}

/// Host service used by the agent and deterministic recovery tests.
pub trait MappingBackend: Send {
    /// Read every service the setter can affect.
    fn read(
        &mut self,
        scope: MappingScope,
    ) -> impl Future<Output = Result<ServiceEvidence, PeripheralError>> + Send;
    /// Replace the scoped property. The agent journals and verifies this write.
    fn write(
        &mut self,
        scope: MappingScope,
        mappings: &MappingArray,
    ) -> impl Future<Output = Result<(), PeripheralError>> + Send;
}
