//! Agent-side Flow: deciding when the cursor leaves this computer through a
//! display edge configured for a peer.
//!
//! The hook supplies cursor positions and the platform supplies display
//! bounds; the decisions here are Flow policy, so they live beside the rest of
//! the agent's orchestration rather than in the OS capture crate.

mod devices;
pub mod edge;
mod handoff;
#[expect(
    dead_code,
    reason = "the Flow runtime is the only user until the agent starts it"
)]
mod identity;
mod pointer;
#[expect(
    dead_code,
    reason = "the agent starts the Flow runtime later in this stack"
)]
mod runtime;
#[cfg(test)]
mod test_support;
