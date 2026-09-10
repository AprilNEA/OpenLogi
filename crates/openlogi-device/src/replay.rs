//! Strict cassette replay over the production device-layer contracts.
//!
//! This module owns mutable virtual topology, replay channels, response
//! barriers, and diagnostics. Persisted schemas and privacy verification live
//! in `openlogi-fixture`.

mod backend;
mod barrier;
mod channel;
mod slots;

use openlogi_fixture::FixtureError;
use thiserror::Error;

pub use backend::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayNode, ReplayTopology,
};
pub use barrier::ReplayResponseBarrier;
pub use channel::{
    ReplayChannelHandle, ReplayCompletion, ReplayMismatch, ReplayRawHidChannel, ReplayRawWriter,
    ReplayRawWriterHandle,
};

/// A replay topology, matching, or completion failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
    /// A persisted fixture contract is invalid.
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    /// Mutable replay state violates a runtime invariant.
    #[error("invalid {asset}: {message}")]
    InvalidState {
        /// Human-readable replay asset kind.
        asset: &'static str,
        /// Specific failed invariant.
        message: String,
    },
    /// No pending cassette exchange matched an outgoing report.
    #[error("unmatched HID request: actual={actual}, hidpp20_normalized={normalized}")]
    UnmatchedRequest {
        /// Exact outgoing bytes as lowercase hex.
        actual: String,
        /// The same bytes with only the HID++ 2.0 software-ID nibble cleared.
        normalized: String,
    },
    /// One or more required cassette exchanges were not consumed.
    #[error("required cassette exchanges were not consumed: {requests:?}")]
    UnconsumedExchanges {
        /// Normalized request keys that remained pending.
        requests: Vec<String>,
    },
    /// A topology operation named a node that does not exist.
    #[error("unknown replay node {0}")]
    UnknownNode(String),
    /// A topology operation named a logical channel that does not exist.
    #[error("unknown replay channel {0}")]
    UnknownChannel(String),
}

impl ReplayError {
    fn invalid(asset: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidState {
            asset,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod session_replay_tests;
#[cfg(test)]
mod tests;
