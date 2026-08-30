//! Host-free fixture schemas and strict HID replay.
//!
//! A semantic [`DeviceProfile`] and a raw [`HidCassette`] are deliberately
//! separate assets. [`ReplayBackend`] combines cassettes with a mutable
//! [`ReplayTopology`] for production device operations; lifecycle scenarios
//! remain hand-authored Rust rather than another serialized format.

mod backend;
mod barrier;
mod channel;
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
    include_str!("../fixtures/canonical-device-profile.json");

/// Canonical profile-only manifest and identity ledger.
///
/// It declares no cassette cases because the repository does not yet contain
/// canonical captured traffic or hardware provenance.
pub const CANONICAL_FIXTURE_MANIFEST_JSON: &str =
    include_str!("../fixtures/canonical-fixture-manifest.json");

pub use backend::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayNode, ReplayTopology,
};
pub use barrier::ReplayResponseBarrier;
pub use channel::{
    ReplayChannelHandle, ReplayCompletion, ReplayMismatch, ReplayRawHidChannel, ReplayRawWriter,
    ReplayRawWriterHandle,
};
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
    ProfileDeviceSettings, ProfileSetting, ProfileSupport, ReportSupport, RequestMatch,
};

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod manifest_tests;
#[cfg(test)]
mod protocol_identity_tests;
#[cfg(test)]
mod session_replay_tests;
#[cfg(test)]
mod tests;
