//! A scripted HID++ transport for tests: it answers requests from a per-device
//! responder instead of talking to hardware.
//!
//! Shared because more than one module needs a device with a feature table of
//! its choosing — `write` drives DPI and lighting against one, `host_switch`
//! needs a keyboard whose host slots it can dictate. Each module keeps its own
//! responder; only the plumbing lives here.

use std::sync::Arc;

use hidpp::channel::{HidppChannel, RawHidChannel};

#[cfg(any(test, feature = "test-support"))]
use crate::backend::NodeId;
#[cfg(test)]
use crate::backend::{BackendError, HidBackend, HotplugStream, NodeInfo, RawWriter};
#[cfg(feature = "test-support")]
use crate::replay::ReplayChannelHandle;
#[cfg(test)]
pub(crate) use crate::replay::ReplayChannelHandle as ScriptedRawHidHandle;
pub(crate) use crate::replay::ReplayRawHidChannel as ScriptedRawHidChannel;
#[cfg(feature = "test-support")]
use crate::{ChannelRegistry, DeviceRoute, SharedChannel};

/// Answers a HID++ request as a particular scripted device would.
#[cfg(test)]
pub(crate) type Responder = fn(&[u8]) -> Option<Vec<u8>>;

/// A HID++ 2.0 error response to `request`: feature index `0xff`, then the
/// addressed feature index, the function/software id, and the error code.
#[cfg(test)]
pub(crate) fn feature_error(request: &[u8], error: u8) -> Vec<u8> {
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1] = request[1];
    response[2] = 0xff;
    response[3] = request[2];
    response[4] = request[3];
    response[5] = error;
    response
}

/// A live channel over `raw`.
#[expect(
    clippy::expect_used,
    reason = "the test-only scripted transport declares HID++ support and cannot recover usefully"
)]
pub(crate) async fn scripted_channel(raw: impl RawHidChannel) -> Arc<HidppChannel> {
    Arc::new(
        HidppChannel::from_raw_channel(raw)
            .await
            .expect("the scripted transport speaks HID++"),
    )
}

/// Build a route-bound scripted channel for a higher-layer test.
#[cfg(feature = "test-support")]
pub async fn scripted_shared_channel(
    route: DeviceRoute,
    responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> (SharedChannel, ReplayChannelHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(responder);
    let channel = scripted_channel(raw).await;
    (SharedChannel::new(channel, route), handle)
}

/// Publish a route-bound scripted channel as the registry's current owner.
#[cfg(feature = "test-support")]
pub async fn publish_scripted_channel(
    registry: &ChannelRegistry,
    node_id: &str,
    route: DeviceRoute,
    responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> (SharedChannel, ReplayChannelHandle) {
    let (shared, handle) = scripted_shared_channel(route.clone(), responder).await;
    registry.replace_node(
        NodeId::from(node_id.to_owned()),
        [route],
        Arc::clone(shared.channel()),
    );
    (shared, handle)
}

/// How a scripted node behaves when the layers above ask to open it.
///
/// The two cases are distinct contracts the enumerator must not conflate: only
/// [`Self::OpenFails`] is a failure the ledger replays a last-good snapshot
/// through. A node that opens into a live HID++ channel belongs here too — add
/// that variant with the test that drives it, so it never sits unconstructed.
#[cfg(test)]
pub(crate) enum ScriptedNode {
    /// The backend cannot open the node at all — unplugged mid-tick, or denied.
    OpenFails,
    /// The node opens but carries no HID++ collection.
    NotHidpp,
}

/// A [`HidBackend`] over scripted nodes.
///
/// Lets the enumerator, the probe and the write layer be driven end to end with
/// no HID stack under them — including the partial-failure paths (a node that
/// will not open, one that is not HID++ at all) that hardware cannot be asked
/// to reproduce on demand.
#[cfg(test)]
pub(crate) struct ScriptedBackend {
    nodes: Vec<(NodeInfo, ScriptedNode)>,
}

#[cfg(test)]
impl ScriptedBackend {
    /// A backend presenting `nodes`, in the order given.
    pub(crate) fn new(nodes: Vec<(NodeInfo, ScriptedNode)>) -> Arc<Self> {
        Arc::new(Self { nodes })
    }

    fn node(&self, id: &NodeId) -> Option<&ScriptedNode> {
        self.nodes
            .iter()
            .find_map(|(info, node)| (info.id == *id).then_some(node))
    }
}

#[cfg(test)]
#[hidpp::async_trait]
impl HidBackend for ScriptedBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(self.nodes.iter().map(|(info, _)| info.clone()).collect())
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        self.enumerate().await
    }

    async fn open_hidpp(&self, node: &NodeInfo) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        match self.node(&node.id) {
            None | Some(ScriptedNode::OpenFails) => Err(BackendError::Disconnected),
            Some(ScriptedNode::NotHidpp) => Ok(None),
        }
    }

    async fn open_raw_writer(&self, _node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        Err(BackendError::Backend(
            "scripted backend has no raw writer".into(),
        ))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

/// A scripted node's descriptor, identified by `id` and otherwise a plausible
/// Logitech HID++ collection.
#[cfg(test)]
pub(crate) fn scripted_node_info(id: &str) -> NodeInfo {
    NodeInfo {
        id: NodeId::from(id.to_owned()),
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: 0xb35b,
        usage_page: 0xff00,
        usage_id: 0x0002,
        name: format!("scripted node {id}"),
        manufacturer: Some("Logitech".into()),
        serial_number: None,
    }
}
