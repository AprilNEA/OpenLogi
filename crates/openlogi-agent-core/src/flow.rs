//! Agent-side Flow: deciding when the cursor leaves this computer through a
//! display edge configured for a peer.
//!
//! The hook supplies cursor positions and the platform supplies display
//! bounds; the decisions here are Flow policy, so they live beside the rest of
//! the agent's orchestration rather than in the OS capture crate.

pub mod edge;
