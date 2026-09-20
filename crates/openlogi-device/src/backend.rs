//! The contract between OpenLogi's HID++ layer and the HID stack beneath it.
//!
//! [`HidBackend`] is the seam. Above it sits everything that knows HID++ and
//! nothing about a host; below it sits one implementation per host HID API —
//! `openlogi-hid` over `async-hid` today, a scripted device tree in tests, and
//! WebHID under wasm if that is ever built.
//!
//! Its own dependencies stay host-free for the same reason, which CI's
//! `wasm (portable crates)` job checks rather than trusts. The conversions
//! *from* a backend's own types belong with that backend, never here.

#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

use std::fmt;
use std::sync::Arc;

use futures_lite::Stream;
use hidpp::async_trait;
use hidpp::channel::HidppChannel;
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

/// A stream of [`HotplugEvent`]s, boxed so [`HidBackend`] stays object-safe.
pub type HotplugStream = Box<dyn Stream<Item = HotplugEvent> + Send + Unpin>;

/// A raw output-report sink, for reports the HID++ framing cannot model.
///
/// The HID++ channel covers reports `0x10`/`0x11`/`0x12` with request/response
/// correlation. A few devices need a bare output report written with no reply
/// expected — Logitech's Litra lights, driven over their own vendor protocol —
/// and that is all this is for.
#[async_trait]
pub trait RawWriter: Send + Sync {
    /// Write one output report, report id included as the first byte.
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError>;
}

/// The HID stack beneath OpenLogi's HID++ layer.
///
/// One implementation per host HID API. Everything above it — enumeration
/// policy, the probe, the write layer, capture sessions — is expressed against
/// this trait and holds none of the backend's own types, which is what lets a
/// second implementation (a scripted device tree in tests, WebHID under wasm)
/// drop in without touching that code.
///
/// Opening is only defined for a node a previous [`Self::enumerate`] reported:
/// a backend may hold OS handles from that enumeration rather than re-finding
/// the node, so an unknown [`NodeInfo`] is [`BackendError::Disconnected`].
#[async_trait]
pub trait HidBackend: Send + Sync {
    /// Every HID node the host currently reports.
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError>;

    /// The subset of [`Self::enumerate`] that can carry HID++ traffic.
    ///
    /// Separate from filtering in the caller because part of the answer is
    /// platform knowledge the backend owns — on Linux the `hid-logitech-dj`
    /// driver publishes a per-device child node that exposes the same vendor
    /// collection as its receiver but must never be addressed directly.
    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError>;

    /// Open `node` as a HID++ channel, or `None` if it does not speak HID++.
    ///
    /// The backend owns the framing details behind this: which report widths
    /// the node carries, and on Windows the pairing of the separate short- and
    /// long-report interfaces into one channel.
    async fn open_hidpp(&self, node: &NodeInfo) -> Result<Option<Arc<HidppChannel>>, BackendError>;

    /// Open `node` for raw output reports.
    async fn open_raw_writer(&self, node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError>;

    /// Subscribe to node connect/disconnect events.
    fn watch(&self) -> Result<HotplugStream, BackendError>;
}

/// Carries a backend failure across the IPC boundary as text.
///
/// [`WriteError`](openlogi_core::hid::WriteError) is `Serialize` and
/// [`BackendError`] is not, so the message is the payload; the typed error is
/// never matched on downstream.
///
/// The impl lives here rather than beside `WriteError` because [`BackendError`]
/// is the local half — the orphan rule allows exactly one of the two homes, and
/// `openlogi-core` must never depend on a backend.
impl From<BackendError> for openlogi_core::hid::WriteError {
    fn from(error: BackendError) -> Self {
        Self::Hid(error.to_string())
    }
}

/// Carries a backend failure across the IPC boundary as text, as
/// [`From<BackendError> for WriteError`](BackendError) does for writes.
impl From<BackendError> for openlogi_core::hid::PairingError {
    fn from(error: BackendError) -> Self {
        Self::Hid(error.to_string())
    }
}
