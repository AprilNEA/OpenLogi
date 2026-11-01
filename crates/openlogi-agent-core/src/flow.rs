//! Agent-side Flow: deciding when the cursor leaves this computer through a
//! display edge configured for a peer, and handing the devices over.
//!
//! The hook supplies cursor positions and the platform supplies display
//! bounds; the decisions here are Flow policy, so they live beside the rest of
//! the agent's orchestration rather than in the OS capture crate.
//! [`FlowController`] is the agent's entry point.

mod controller;
mod devices;
pub mod edge;
mod handoff;
mod identity;
mod pointer;
mod runtime;
#[cfg(test)]
mod test_support;

pub use controller::{FlowController, FlowInput};
pub(crate) use devices::InventoryDevice;
