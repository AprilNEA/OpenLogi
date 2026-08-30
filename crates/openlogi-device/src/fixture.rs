//! Host-free fixture schemas and strict HID replay.
//!
//! A semantic [`DeviceProfile`] and a raw [`HidCassette`] are deliberately
//! separate assets. [`ReplayBackend`] combines cassettes with a mutable
//! [`ReplayTopology`] for production device operations; lifecycle scenarios
//! remain hand-authored Rust rather than another serialized format.

mod backend;
mod barrier;
mod channel;
mod schema;

/// Canonical privacy-safe profile shared by the mock agent and projection tests.
///
/// The JSON stays embedded by this owning crate so packaged consumers do not
/// depend on a workspace-relative source path.
pub const CANONICAL_DEVICE_PROFILE_JSON: &str =
    include_str!("../fixtures/canonical-device-profile.json");

pub use backend::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayNode, ReplayTopology,
};
pub use barrier::ReplayResponseBarrier;
pub use channel::{
    ReplayChannelHandle, ReplayCompletion, ReplayMismatch, ReplayRawHidChannel, ReplayRawWriter,
    ReplayRawWriterHandle,
};
pub use schema::{
    CassetteExchange, DeviceProfile, FIXTURE_SCHEMA_VERSION, FixtureError, HidCassette,
    ProfileDeviceSettings, ProfileSetting, ProfileSupport, ReportSupport, RequestMatch,
};

#[cfg(test)]
mod tests;
