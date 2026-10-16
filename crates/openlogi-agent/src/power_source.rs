//! Opt-in publication of receiver accessory batteries to the macOS Batteries
//! widget. One module per OS, both exposing the same [`PowerSources`] API:
//!
//! - **macOS** ([`macos`]): the publisher policy and its private IOKit backend.
//! - **Other platforms** ([`unsupported`]): a no-op that never wakes the run loop.
//!
//! The lifecycle drives it through [`PowerSources::wake`] and
//! [`PowerSources::reconcile`] without platform checks of its own.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub(crate) use macos::PowerSources;
#[cfg(not(target_os = "macos"))]
pub(crate) use unsupported::PowerSources;
