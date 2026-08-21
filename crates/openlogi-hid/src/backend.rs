//! The contract between the HID++ layer and the HID stack beneath it.
//!
//! Everything above the `channel::transport` module speaks these types, never
//! the backend's own. That keeps the choice of HID stack — `async-hid` today —
//! an implementation detail of this crate instead of part of its public API,
//! and it is the half of the backend seam that callers see.
//!
//! The conversions *from* a backend's types deliberately live with the backend
//! that raises them (`channel::transport`), not here: this module must stay
//! nameable by code that has no backend at all.

use std::fmt;

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

/// A HID node appeared on or vanished from the OS device tree.
///
/// Deliberately carries no identity: every consumer reacts by re-enumerating,
/// and a backend that can only report "something changed" must still be able
/// to raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugEvent {
    /// A device node was connected.
    Connected,
    /// A device node was disconnected.
    Disconnected,
}

/// Opaque identity of one HID node, as the backend that enumerated it names it.
///
/// Distinct per OS device node while that node exists, so it keys the open
/// channels and the per-node ledger. It is **not** a portable physical key —
/// a hidraw path on Linux, a device path on Windows, an IOKit registry entry
/// on macOS — and must never be persisted. Physical identity comes from the
/// device's own serial or HID++ model info instead.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(String);

impl From<String> for NodeId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One HID node as the backend reports it, before anything is opened.
///
/// These are the fields enumeration filters on and routes address by — the
/// intersection every HID backend can supply, which is also all the layers
/// above the transport ever read.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    /// Backend-assigned identity of this node.
    pub id: NodeId,
    /// HID vendor id of the device's manufacturer.
    pub vendor_id: u16,
    /// HID product id.
    pub product_id: u16,
    /// HID usage page of this node's top-level collection.
    pub usage_page: u16,
    /// HID usage id of this node's top-level collection.
    pub usage_id: u16,
    /// Human-readable device name.
    pub name: String,
    /// Human-readable manufacturer, when the backend reports one.
    pub manufacturer: Option<String>,
    /// Device serial number, when the device has one and the backend can read
    /// it.
    pub serial_number: Option<String>,
}

impl NodeInfo {
    /// Stable opaque identity used by raw-device routes.
    ///
    /// Prefers the HID serial; otherwise retains the backend's node id as a
    /// runtime identity. The latter is deliberately not a cross-machine
    /// portable key, but it is stronger than enumeration order and lets
    /// duplicate nodes be rejected deterministically.
    #[must_use]
    pub fn identity(&self) -> String {
        self.serial_number
            .as_deref()
            .filter(|serial| !serial.is_empty())
            .map_or_else(
                || format!("id:{}", self.id),
                |serial| format!("serial:{}", serial.to_ascii_lowercase()),
            )
    }
}
