//! Typed functions exposed by native and sandboxed drivers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CapabilityId, ControlId, HidUsage, PeripheralError, ScopeKind, SettingField};
use crate::binding::{Action, ButtonId, Cid, KeyCombo, KeyboardUsage};
use crate::device::LightCapabilities;

/// A versioned capability with independent availability and evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRecord {
    /// Stable semantic identity.
    pub id: CapabilityId,
    /// Contract version, independent of package and config versions.
    pub version: u32,
    /// Typed controls and limits.
    pub capability: Capability,
    /// Scopes enforceable by this backend.
    pub scopes: Vec<ScopeKind>,
    /// An unavailable capability retains its identity and saved settings.
    pub unavailable: Option<PeripheralError>,
    /// Whether the values are declared, probed, or last-known.
    pub evidence: CapabilityEvidence,
    /// Driver observations, separate from desired settings and schema defaults.
    pub values: BTreeMap<String, super::SettingValue>,
}

/// Source of capability metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityEvidence {
    /// Static or plugin-declared metadata.
    Declared,
    /// Confirmed by a device probe.
    Probed,
    /// Retained after the device became unreachable.
    LastKnown,
}

/// Capability families rendered by shared application panels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Capability {
    /// Standard or protocol-specific button controls.
    InputRemap(InputRemapCapability),
    /// Existing normalized light contract.
    Light(LightCapabilities),
    /// Pointer resolution; detailed ranges are read by the owning driver.
    Pointer,
    /// Native wheel controls.
    Wheel(WheelCapability),
    /// Existing HID++ solid-color keyboard lighting contract.
    KeyboardLighting,
    /// Keyboard function-row inversion.
    FnLock,
    /// Existing haptic and gesture control contract.
    Haptics,
    /// Camera controls exposed by a UVC adapter.
    Camera(CameraCapability),
    /// Bounded custom scalar settings.
    Extension(BTreeMap<String, SettingField>),
    /// An optional future contract that this host cannot execute.
    Unsupported,
}

/// Independent native wheel features.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelCapability {
    /// Supports native reporting resolution changes.
    pub resolution: bool,
    /// Supports native vertical inversion.
    pub inversion: bool,
    /// Supplies a horizontal wheel.
    pub horizontal: bool,
}

/// Native camera identity and measured controls; capture stays in the media client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraCapability {
    /// Host metadata used by the preview client.
    pub camera: crate::camera::Camera,
    /// Last successful native read. Absence does not mean no controls exist.
    pub state: Option<crate::camera::CameraState>,
}

/// Input controls and the targets their backend can execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRemapCapability {
    /// Stable source controls.
    pub controls: Vec<InputControl>,
    /// Execution contract, used to restrict the existing action picker.
    pub targets: TargetKind,
    /// Whether bindings can change with the foreground application.
    pub per_app: bool,
}

/// What a backend can do with a configured input binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    /// A native one-to-one HID usage mapping; no modifiers or synthesis.
    KeyboardKey,
    /// The existing action dispatcher can execute configured actions.
    Action,
}

impl TargetKind {
    /// Validate an existing action against this capability's target contract.
    pub fn validate(self, action: &Action) -> Result<(), PeripheralError> {
        match self {
            Self::KeyboardKey => native_key(action).map(|_| ()),
            Self::Action => Ok(()),
        }
    }
}

/// Extract a single standard keyboard usage without creating another key table.
pub fn native_key(action: &Action) -> Result<KeyboardUsage, PeripheralError> {
    if let Action::CustomShortcut(combo) = action
        && combo.is_single_key()
    {
        return Ok(combo.key());
    }
    Err(PeripheralError::InvalidSettings(
        "native HID mappings require one keyboard key without modifiers".into(),
    ))
}

/// A stable control with localized labels and explicit trigger semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputControl {
    /// Configuration identity; independent of labels.
    pub id: ControlId,
    /// Locale labels; English is the required fallback at catalog validation.
    pub labels: BTreeMap<String, String>,
    /// Protocol-independent source identity.
    pub source: InputSource,
    /// The device's verified event behavior.
    pub trigger: Trigger,
    /// Suggested target; a suggestion never enables a mapping.
    pub recommended_key: Option<KeyCombo>,
}

/// Input identity vocabularies must never cross by raw integer conversion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    /// Standard HID usage pair.
    HidUsage(HidUsage),
    /// Logitech reprogrammable-control identity.
    HidppControl(Cid),
    /// Existing OS mouse-button identity.
    Mouse(ButtonId),
    /// A logical control emitted by executable driver code.
    Logical,
}

/// Verified trigger behavior. A pulse does not establish physical hold duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    /// A short press produces one pulse or rapid down/up pair.
    ShortPress,
    /// The driver observes physical press and release transitions.
    PressRelease,
}

/// A validated logical transition supplied by a selected device driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputTransition {
    /// Start a physical press.
    Press,
    /// End a physical press.
    Release,
    /// Execute one short-press action.
    Trigger,
}
