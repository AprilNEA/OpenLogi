//! The error contract between the HID++ layer and the HID stack beneath it.
//!
//! Everything above the `channel::transport` module speaks [`BackendError`],
//! never the backend's own error type. That keeps the choice of HID stack —
//! `async-hid` today — an implementation detail of this crate instead of part
//! of its public API, and it is the half of the backend seam that callers see.
//!
//! The conversion *into* [`BackendError`] deliberately lives with the backend
//! that raises it (`channel::transport`), not here: this module must stay
//! nameable by code that has no backend at all.

use thiserror::Error;

/// A failure raised by the HID backend beneath the HID++ channel layer.
///
/// Deliberately narrow. The only distinction anything above the transport
/// branches on is "the device is gone" versus everything else, so a backend
/// collapses its own error taxonomy into these two variants and every caller
/// stays backend-agnostic.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The device is unreachable — it vanished after being opened, or was
    /// already gone when the open was attempted.
    ///
    /// The two are one case here: nothing in the crate treats them
    /// differently, and a backend cannot always tell them apart.
    #[error("the HID device is not connected")]
    Disconnected,
    /// Any other backend failure, carried as its message.
    ///
    /// Backend error types are neither `Serialize` nor uniform across
    /// backends, so the text is the whole payload — nothing matches on it.
    #[error("{0}")]
    Backend(String),
}
