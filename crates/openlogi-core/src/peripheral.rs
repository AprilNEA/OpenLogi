//! Device-independent identities, capabilities, settings, and operation evidence.
//!
//! Protocol drivers retain their own transport types. These records describe
//! the contract exposed to configuration and IPC clients.

pub mod builtin;
mod capability;
mod identity;
mod settings;

pub use capability::*;
pub use identity::*;
pub use settings::*;

use serde::{Deserialize, Serialize};

use crate::device::DeviceKind;

/// A device session as published by the agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeripheralRecord {
    /// Model identity; never a physical-unit identity.
    pub model: ModelId,
    /// Verified unit identity, when supplied by the driver.
    pub physical: Option<PhysicalDeviceId>,
    /// Connection-local identity and attachment generation.
    pub session: SessionId,
    /// Endpoints owned by the selected driver.
    pub endpoints: Vec<Endpoint>,
    /// Default product name, independent of the configuration key.
    pub name: String,
    /// Presentation hint. Capability records decide available controls.
    pub kind: DeviceKind,
    /// Selection and package provenance.
    pub driver: DriverSelection,
    /// Selection or execution failure, including failures before capability discovery.
    pub driver_error: Option<PeripheralError>,
    /// Whether this record came from a completed discovery pass.
    pub connection: ConnectionStatus,
    /// Functions this driver exposes for the device.
    pub capabilities: Vec<CapabilityRecord>,
    /// Configuration scopes the backend can enforce, with the preferred scope first.
    pub scopes: Vec<ScopeKind>,
    /// Per-effect application and verification evidence.
    pub operations: Vec<OperationStatus>,
}

/// One accepted peripheral inventory and its independently fallible extension sources.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeripheralSnapshot {
    /// Normalized device sessions, including retained offline records.
    pub devices: Vec<PeripheralRecord>,
    /// Rejected source updates and unavailable implementations.
    pub diagnostics: Vec<CatalogDiagnostic>,
    /// Installed packages and their desired versus active content identities.
    pub plugins: Vec<PluginPackageRecord>,
}

impl PeripheralSnapshot {
    /// Connected camera metadata for native preview and asset clients.
    pub fn cameras(&self) -> impl Iterator<Item = &crate::camera::Camera> {
        self.devices
            .iter()
            .filter(|record| record.connection == ConnectionStatus::Online)
            .flat_map(|record| &record.capabilities)
            .filter_map(|record| match &record.capability {
                Capability::Camera(camera) => Some(&camera.camera),
                _ => None,
            })
    }
}

/// An extension source diagnostic; rejection does not imply an older generation stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDiagnostic {
    /// Descriptor path or package digest.
    pub source: String,
    /// Whether a previous accepted generation remains active.
    pub retained: bool,
    /// Typed rejection.
    pub error: PeripheralError,
}

/// Package metadata presented before explicit permission grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPackageRecord {
    /// Implementation identity.
    pub driver: DriverId,
    /// Semantic package version.
    pub version: String,
    /// Exact installed content identity.
    pub digest: String,
    /// Catalog selections the grant can authorize.
    pub descriptors: std::collections::BTreeMap<DescriptorId, DescriptorDisclosure>,
    /// Bounded human-readable device operations requested by this content.
    pub permissions: Vec<String>,
    /// Desired selection of this exact digest, independent of live execution.
    pub selection: PluginSelectionStatus,
    /// Whether at least one current session executes this digest.
    pub active: bool,
    /// A retained previous selection is available for this selected content.
    pub rollback_available: bool,
}

/// Desired package selection, separate from whether a device session is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginSelectionStatus {
    /// Installed content is not selected by configuration.
    Unselected,
    /// Configuration selects this content but disables execution.
    Disabled,
    /// Configuration selects and enables this content.
    Enabled,
}

impl PluginSelectionStatus {
    /// Whether configuration selects this exact content.
    #[must_use]
    pub const fn is_selected(self) -> bool {
        !matches!(self, Self::Unselected)
    }

    /// Whether configuration permits execution of this exact content.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Explicit package lifecycle operations. Installation does not imply enablement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginCommand {
    /// Stage and validate local package content without device access.
    Install {
        /// Directory chosen by the user.
        path: String,
    },
    /// Grant requested operations for exact content and descriptor selections.
    Enable {
        /// Content identity reviewed by the user.
        digest: String,
        /// Explicit descriptor selections.
        descriptors: std::collections::BTreeMap<DescriptorId, String>,
    },
    /// Revoke execution while preserving desired device settings.
    Disable {
        /// Implementation to disable.
        driver: DriverId,
    },
    /// Remove installed content after cleanup; retain saved settings and recovery obligations.
    Remove {
        /// Exact content to remove.
        digest: String,
    },
    /// Restore the previous package and its settings with a guarded config save.
    Rollback {
        /// Implementation to restore.
        driver: DriverId,
    },
}

/// Descriptor content shown before a package receives device access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorDisclosure {
    /// Exact normalized content approved by the user.
    pub fingerprint: String,
    /// Product and endpoint matching constraints.
    pub matching: String,
    /// Whether the grant store approves this exact descriptor content.
    pub granted: bool,
}

/// Exact driver selection, independent of product identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverSelection {
    /// Catalog descriptor.
    pub descriptor: DescriptorId,
    /// Implementation identifier.
    pub driver: DriverId,
    /// Executable content identity, absent for built-in implementations.
    pub digest: Option<String>,
    /// Source shown in diagnostics.
    pub source: DriverSource,
}

/// Where a catalog registration originated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverSource {
    /// Compiled hardware metadata.
    Builtin,
    /// A validated local descriptor file.
    Descriptor(String),
    /// An installed, immutable plugin package.
    Plugin(String),
}

/// Discovery evidence; a failed scan is not a disconnect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// Present in the last successful scan.
    Online,
    /// Absent from a successful scan.
    Offline,
    /// Discovery failed; retained facts may be stale.
    Unavailable,
}

/// Application result for one capability, separate from event verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationStatus {
    /// Capability that owns the operation.
    pub capability: CapabilityId,
    /// Configuration revision this result belongs to.
    pub revision: u64,
    /// Native or device application result.
    pub application: ApplicationStatus,
    /// Evidence about actual input delivery.
    pub verification: VerificationStatus,
}

/// Desired-state application and conditional cleanup outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplicationStatus {
    /// No setting is enabled.
    Disabled,
    /// A saved setting awaits reconciliation.
    Pending,
    /// The backend confirmed the setting, not application input delivery.
    Applied,
    /// Cleanup awaits the target or an available host service.
    RestorePending,
    /// The original owned effect was restored.
    Restored,
    /// The operation failed with an actionable cause.
    Failed(PeripheralError),
}

/// Independent observations, never inferred from a successful property write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    /// No event diagnostic was requested.
    #[default]
    NotObserved,
    /// Configuration is applied; the user can short-press the control.
    WaitingForPress,
    /// A diagnostic saw the declared device input.
    RawEventObserved,
    /// A diagnostic saw the resulting OS key.
    SystemKeyObserved,
    /// The user confirmed delivery in the target application.
    UserConfirmed,
}

/// A failure that clients can distinguish without parsing diagnostic text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PeripheralError {
    /// The matching device is absent.
    #[error("device is offline")]
    Offline,
    /// The host could not enumerate devices.
    #[error("device discovery is unavailable: {0}")]
    DiscoveryUnavailable(String),
    /// The requested implementation cannot be activated.
    #[error("selected driver is unavailable: {0}")]
    DriverUnavailable(String),
    /// No unique owner can be selected.
    #[error("conflicting device drivers: {0}")]
    DriverConflict(String),
    /// The requested host service is not implemented on this platform.
    #[error("host service is unavailable: {0}")]
    Unsupported(String),
    /// An operation exceeds the selected descriptor or grant.
    #[error("device operation is not permitted: {0}")]
    PermissionDenied(String),
    /// A command or descriptor violates the advertised contract.
    #[error("invalid device settings: {0}")]
    InvalidSettings(String),
    /// The host property could not be read.
    #[error("could not read device configuration: {0}")]
    ReadFailed(String),
    /// The host rejected a configuration write.
    #[error("could not write device configuration: {0}")]
    WriteFailed(String),
    /// A write completed without the requested readback.
    #[error("device configuration readback does not match the requested value")]
    ReadbackMismatch,
    /// Another writer now owns the source; automatic writes are suspended.
    #[error(
        "the mapping was modified outside OpenLogi; resolve the conflict before applying again"
    )]
    ExternalModification,
    /// A model-wide setter cannot preserve all matched service arrays.
    #[error("matching HID services have different mappings or restoration requirements")]
    MappingScopeConflict,
    /// The recovery journal could not be committed.
    #[error("could not save device recovery state: {0}")]
    JournalFailed(String),
    /// A replaced session must not accept old work.
    #[error("the device attachment changed before the operation completed")]
    StaleSession,
    /// The component trapped or violated its contract.
    #[error("plugin fault: {0}")]
    PluginFault(String),
    /// A bounded queue, call, or allocation exceeded the host budget.
    #[error("plugin resource limit reached: {0}")]
    ResourceLimit(String),
    /// Saving desired state failed independently of device application.
    #[error("could not save configuration: {0}")]
    ConfigWriteFailed(String),
    /// Another configuration writer changed the loaded revision.
    #[error("configuration changed on disk; reload before saving")]
    ConfigConflict,
    /// Host activity currently prohibits device access.
    #[error("device operations are suspended until the host session resumes")]
    Suspended,
}
