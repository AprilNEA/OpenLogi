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

/// Cache key for one enumerated HID node: its reported id plus the usage pair
/// that distinguishes the collections sharing that id. See [`NativeBackend`].
#[derive(Clone, PartialEq, Eq, Hash)]
struct NodeKey {
    id: NodeId,
    usage_page: u16,
    usage_id: u16,
}

impl NodeKey {
    fn of(info: &NodeInfo) -> Self {
        Self {
            id: info.id.clone(),
            usage_page: info.usage_page,
            usage_id: info.usage_id,
        }
    }
}

/// [`HidBackend`] over `async-hid`.
#[derive(Default)]
pub(crate) struct NativeBackend {
    /// OS handles from the most recent enumeration, keyed by the id that
    /// enumeration reported them under *plus its usage pair*.
    ///
    /// `async_hid::Device` is an OS handle, not a value: it cannot be rebuilt
    /// from a [`NodeId`], and re-finding one costs another enumeration. Since
    /// the trait only defines opening a node that was just enumerated, keeping
    /// the handles from that enumeration is both cheaper and a truer model
    /// than looking them up again. Held behind an `Arc` so an open can borrow
    /// one without keeping the map locked across its await.
    ///
    /// The usage pair belongs in the key because macOS reports one
    /// `DeviceInfo` per usage pair while giving them all the *same*
    /// `DeviceId::RegistryEntryId`. Keying on the id alone collapses every
    /// collection of a multi-collection device onto one entry and the last
    /// enumerated one wins, so `handle()` can hand back a different collection
    /// than the caller enumerated. That mis-sourced `usage_page`/`usage_id`
    /// then flows into `is_long_only_collection()` in `open_hidpp_channel`,
    /// which decides whether the channel advertises HID++ *short* report
    /// support. A BLE-direct mouse (e.g. MX Master 4) is long-only, but if the
    /// generic-desktop or haptics collection wins the key it is judged
    /// short-capable, the `hidpp` channel skips up-converting short messages,
    /// and every write goes out as report `0x10` — a report id that does not
    /// exist on that device — which macOS rejects with `kIOReturnNotFound`
    /// (0xE00002F0) before it ever reaches the radio.
    nodes: Mutex<HashMap<NodeKey, Arc<Device>>>,
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
            .map(|device| (NodeKey::of(&super::node_info(device)), Arc::clone(device)))
            .collect();
        *self.nodes.lock().unwrap_or_else(PoisonError::into_inner) = handles;
        Ok(devices)
    }

    /// The cached OS handle for `node`, if it was in the last enumeration.
    fn handle(&self, node: &NodeInfo) -> Result<Arc<Device>, BackendError> {
        self.nodes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&NodeKey::of(node))
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
