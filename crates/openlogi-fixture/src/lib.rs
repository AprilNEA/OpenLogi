//! Host-free fixture schemas, synthetic identity policy, and verification.
//!
//! A semantic [`DeviceProfile`] and raw [`HidCassette`] are deliberately
//! separate assets. This crate validates both without file I/O, host access,
//! or async work. Replay transport and mutable topology live in
//! `openlogi-device`.

#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

mod generate;
mod identity;
mod manifest;
mod protocol_identity;
mod schema;
mod verify;

/// Canonical privacy-safe profile shared by the mock agent and projection tests.
///
/// The JSON stays embedded by this owning crate so packaged consumers do not
/// depend on a workspace-relative source path.
pub const CANONICAL_DEVICE_PROFILE_JSON: &str =
    include_str!("../fixtures/devices/openlogi-canonical-synthetic-001/profile.json");

/// Canonical profile-only manifest and identity ledger.
///
/// It declares no cassette cases because the repository does not yet contain
/// canonical captured traffic or hardware provenance.
pub const CANONICAL_FIXTURE_MANIFEST_JSON: &str =
    include_str!("../fixtures/devices/openlogi-canonical-synthetic-001/manifest.json");

pub use generate::FixtureCaseBinding;
pub use identity::{
    MAX_SYNTHETIC_IDENTITY_ORDINAL, SyntheticIdentityError, SyntheticIdentityKind,
    SyntheticIdentityOrdinal, SyntheticIdentityValue, classify_synthetic_identity_bytes,
    classify_synthetic_profile_identity, generate_synthetic_identity, unifying_receiver_route,
};
pub use manifest::{
    FixtureCase, FixtureCaseRelationship, FixtureDeviceRoute, FixtureManifest, FixturePrincipal,
    IdentityLedgerEntry, IdentityLocation, IdentityOccurrence, IdentityRepresentation,
    ProfileIdentityField,
};
pub use protocol_identity::{
    ProtocolExchangeIdentity, ProtocolIdentityError, ProtocolIdentityExtractor,
    ProtocolIdentityField, is_pairing_identity_traffic,
};
pub use schema::{
    CassetteExchange, DeviceProfile, FIXTURE_SCHEMA_VERSION, FixtureError, HidCassette,
    ProfileDeviceSettings, ProfileSetting, ProfileSupport, ReportSupport, ReportValidationError,
    RequestMatch,
};
pub use verify::{FixtureVerificationError, FixtureVerificationStage};

#[cfg(test)]
mod generate_tests;
#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod manifest_tests;
#[cfg(test)]
mod protocol_identity_tests;
