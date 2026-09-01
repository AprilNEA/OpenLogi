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

use openlogi_device::DeviceIoGate;
use openlogi_device::backend::{
    BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter,
};

use super::{
    device_io_gate, device_io_suspended, enumerate_devices, is_hidpp_node, open_hidpp_channel,
    watch_nodes,
};

/// One logical top-level collection exposed by an OS HID node.
///
/// On macOS, `async-hid` emits one [`Device`] per usage pair, but every one of
/// those devices carries the same IOKit registry id. Keying the handle cache by
/// [`NodeId`] alone therefore lets the last generic collection overwrite the
/// HID++ collection selected by enumeration. Preserve the usage pair so an
/// open receives the same logical device metadata that was selected.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HandleKey {
    id: NodeId,
    usage_page: u16,
    usage_id: u16,
}

impl HandleKey {
    fn for_device(device: &Device) -> Self {
        Self {
            id: super::node_id(device),
            usage_page: device.usage_page,
            usage_id: device.usage_id,
        }
    }

    fn for_node(node: &NodeInfo) -> Self {
        Self {
            id: node.id.clone(),
            usage_page: node.usage_page,
            usage_id: node.usage_id,
        }
    }
}

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
pub(crate) struct NativeBackend {
    /// OS handles from the most recent enumeration, keyed by the node id and
    /// top-level usage pair that enumeration reported them under.
    ///
    /// `async_hid::Device` is an OS handle, not a value: it cannot be rebuilt
    /// from a [`NodeId`], and re-finding one costs another enumeration. Since
    /// the trait only defines opening a node that was just enumerated, keeping
    /// the handles from that enumeration is both cheaper and a truer model
    /// than looking them up again. Held behind an `Arc` so an open can borrow
    /// one without keeping the map locked across its await.
    nodes: Mutex<HashMap<HandleKey, Arc<Device>>>,
    device_io: DeviceIoGate,
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self {
            nodes: Mutex::new(HashMap::new()),
            device_io: device_io_gate(),
        }
    }
}

impl NativeBackend {
    /// Enumerate the host's HID nodes and refresh the handle cache.
    async fn refresh(&self) -> Result<Vec<Arc<Device>>, BackendError> {
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        let devices: Vec<Arc<Device>> = enumerate_devices()
            .await?
            .into_iter()
            .map(Arc::new)
            .collect();
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        let handles = devices
            .iter()
            .map(|device| (HandleKey::for_device(device), Arc::clone(device)))
            .collect();
        *self.nodes.lock().unwrap_or_else(PoisonError::into_inner) = handles;
        Ok(devices)
    }

    /// The cached OS handle for `node`, if it was in the last enumeration.
    fn handle(&self, node: &NodeInfo) -> Result<Arc<Device>, BackendError> {
        self.nodes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&HandleKey::for_node(node))
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
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        let device = self.handle(node)?;
        open_hidpp_channel(&device, self.device_io.clone()).await
    }

    async fn open_raw_writer(&self, node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        let (_reader, writer) = self
            .handle(node)?
            .open()
            .await
            .map_err(super::backend_error)?;
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        Ok(Box::new(NativeRawWriter {
            writer,
            device_io: self.device_io.clone(),
        }))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(watch_nodes()?))
    }
}

/// [`RawWriter`] over an `async-hid` output-report writer.
struct NativeRawWriter {
    writer: DeviceWriter,
    device_io: DeviceIoGate,
}

#[async_trait]
impl RawWriter for NativeRawWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        if !self.device_io.allows_io() {
            return Err(device_io_suspended());
        }
        self.writer
            .write_output_report(report)
            .await
            .map_err(super::backend_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_handle_keys_preserve_collections_on_the_same_os_node() {
        let os_node = NodeId::from("RegistryEntryId(42)".to_owned());
        let primary_mouse = NodeInfo {
            id: os_node.clone(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            usage_page: 0x0001,
            usage_id: 0x0002,
            name: "Test Mouse".to_owned(),
            manufacturer: Some("Test Vendor".to_owned()),
            serial_number: None,
        };
        let hidpp = NodeInfo {
            id: os_node,
            vendor_id: 0x1234,
            product_id: 0x5678,
            usage_page: 0xff43,
            usage_id: 0x0202,
            name: "Test Mouse".to_owned(),
            manufacturer: Some("Test Vendor".to_owned()),
            serial_number: None,
        };

        let hidpp_key = HandleKey::for_node(&hidpp);
        let handles = HashMap::from([
            (HandleKey::for_node(&primary_mouse), "primary mouse"),
            (hidpp_key.clone(), "hidpp"),
        ]);

        assert_eq!(handles.len(), 2);
        assert_eq!(handles.get(&hidpp_key), Some(&"hidpp"));
    }
}
