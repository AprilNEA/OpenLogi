//! The `async-hid` implementation of [`HidBackend`].
//!
//! Everything platform-specific about talking to the host HID stack is reached
//! through this type. It is the only implementor in the tree today; a scripted
//! one for tests and a WebHID one under wasm are the reasons the trait exists.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use async_hid::{AsyncHidWrite as _, Device, DeviceWriter};
use hidpp::async_trait;
use hidpp::channel::HidppChannel;

use openlogi_device::backend::{
    BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter,
};

use super::{enumerate_devices, is_hidpp_node, open_hidpp_channel, watch_nodes};

/// The process-wide native backend.
///
/// One instance, not one per caller: it owns the handle cache below, and the
/// `IOHIDManager` underneath must not be rebuilt on every enumeration (issue
/// #99 — see [`super::HID_BACKEND`]). Handed out as an `Arc` so a long-lived
/// holder (the inventory enumerator, a channel pool) can keep it in a field
/// typed against the trait rather than against this implementation.
static NATIVE_BACKEND: LazyLock<Arc<NativeBackend>> =
    LazyLock::new(|| Arc::new(NativeBackend::default()));

/// The native HID backend this build talks to hardware through.
pub(crate) fn native_backend() -> Arc<dyn HidBackend> {
    Arc::clone(&NATIVE_BACKEND) as Arc<dyn HidBackend>
}

/// [`HidBackend`] over `async-hid`.
#[derive(Default)]
pub(crate) struct NativeBackend {
    /// OS handles from the most recent enumeration, keyed by the id that
    /// enumeration reported them under.
    ///
    /// `async_hid::Device` is an OS handle, not a value: it cannot be rebuilt
    /// from a [`NodeId`], and re-finding one costs another enumeration. Since
    /// the trait only defines opening a node that was just enumerated, keeping
    /// the handles from that enumeration is both cheaper and a truer model
    /// than looking them up again. Held behind an `Arc` so an open can borrow
    /// one without keeping the map locked across its await.
    nodes: Mutex<HashMap<NodeId, Arc<Device>>>,
}

impl NativeBackend {
    /// Enumerate the host's HID nodes and refresh the handle cache.
    async fn refresh(&self) -> Result<Vec<Arc<Device>>, BackendError> {
        let devices: Vec<Arc<Device>> = enumerate_devices()
            .await?
            .into_iter()
            .map(Arc::new)
            .collect();
        let handles = devices
            .iter()
            .map(|device| (super::node_id(device), Arc::clone(device)))
            .collect();
        *self.nodes.lock().unwrap_or_else(PoisonError::into_inner) = handles;
        Ok(devices)
    }

    /// The cached OS handle for `node`, if it was in the last enumeration.
    fn handle(&self, node: &NodeInfo) -> Result<Arc<Device>, BackendError> {
        self.nodes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&node.id)
            .map(Arc::clone)
            .ok_or(BackendError::Disconnected)
    }
}

#[async_trait]
impl HidBackend for NativeBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(self
            .refresh()
            .await?
            .iter()
            .map(|device| super::node_info(device))
            .collect())
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(self
            .refresh()
            .await?
            .iter()
            .filter(|device| is_hidpp_node(device))
            .map(|device| super::node_info(device))
            .collect())
    }

    async fn open_hidpp(&self, node: &NodeInfo) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        let device = self.handle(node)?;
        open_hidpp_channel(&device).await
    }

    async fn open_raw_writer(&self, node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        let (_reader, writer) = self
            .handle(node)?
            .open()
            .await
            .map_err(super::backend_error)?;
        Ok(Box::new(NativeRawWriter(writer)))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(watch_nodes()?))
    }
}

/// [`RawWriter`] over an `async-hid` output-report writer.
struct NativeRawWriter(DeviceWriter);

#[async_trait]
impl RawWriter for NativeRawWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        self.0
            .write_output_report(report)
            .await
            .map_err(super::backend_error)
    }
}
