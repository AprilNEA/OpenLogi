//! Host-free fixture schemas and strict HID replay.
//!
//! A semantic [`DeviceProfile`] and a raw [`HidCassette`] are deliberately
//! separate assets. [`ReplayBackend`] combines cassettes with a mutable
//! [`ReplayTopology`] for production device operations; lifecycle scenarios
//! remain hand-authored Rust rather than another serialized format.

mod backend;
mod channel;
mod schema;

pub use backend::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayNode, ReplayTopology,
};
pub use channel::{
    ReplayChannelHandle, ReplayCompletion, ReplayMismatch, ReplayRawHidChannel, ReplayRawWriter,
    ReplayRawWriterHandle,
};
pub use schema::{
    CassetteExchange, DeviceProfile, FIXTURE_SCHEMA_VERSION, FixtureError, HidCassette,
    ReportSupport, RequestMatch,
};

#[cfg(test)]
mod tests;
