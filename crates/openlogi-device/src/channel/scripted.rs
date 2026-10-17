//! A scripted HID++ transport for tests: it answers requests from a per-device
//! responder instead of talking to hardware.
//!
//! Shared because more than one module needs a device with a feature table of
//! its choosing — `write` drives DPI and lighting against one, `host_switch`
//! needs a keyboard whose host slots it can dictate. Each module keeps its own
//! responder; only the plumbing lives here.

use std::sync::Arc;

use hidpp::channel::{HidppChannel, RawHidChannel};

use crate::backend::{BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter};
pub(crate) use crate::replay::ReplayRawHidChannel as ScriptedRawHidChannel;

/// Answers a HID++ request as a particular scripted device would.
pub(crate) type Responder = fn(&[u8]) -> Option<Vec<u8>>;

/// A HID++ 2.0 error response to `request`: feature index `0xff`, then the
/// addressed feature index, the function/software id, and the error code.
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
pub(crate) async fn scripted_channel(raw: impl RawHidChannel) -> Arc<HidppChannel> {
    Arc::new(
        HidppChannel::from_raw_channel(raw)
            .await
            .expect("the scripted transport speaks HID++"),
    )
}

/// Outcome produced when the scripted backend opens a listed node.
///
/// The cases are distinct contracts the enumerator must not conflate: only
/// [`Self::Fails`] is a backend failure the ledger replays a last-good snapshot
/// through.
pub(crate) enum ScriptedOpen {
    /// The backend cannot open the node at all — unplugged mid-tick, or denied.
    Fails,
    /// The node opens but carries no HID++ collection.
    NotHidpp,
    /// The node opens into a live HID++ channel that does not answer requests.
    UnresponsiveHidpp,
}

/// A [`HidBackend`] over scripted nodes.
///
/// Lets the enumerator, the probe and the write layer be driven end to end with
/// no HID stack under them — including the partial-failure paths (a node that
/// will not open, one that is not HID++ at all) that hardware cannot be asked
/// to reproduce on demand.
pub(crate) struct ScriptedBackend {
    nodes: Vec<(NodeInfo, ScriptedOpen)>,
}

impl ScriptedBackend {
    /// A backend presenting `nodes`, in the order given.
    pub(crate) fn new(nodes: Vec<(NodeInfo, ScriptedOpen)>) -> Arc<Self> {
        Arc::new(Self { nodes })
    }

    fn open_outcome(&self, id: &NodeId) -> Option<&ScriptedOpen> {
        self.nodes
            .iter()
            .find_map(|(info, node)| (info.id == *id).then_some(node))
    }
}

#[hidpp::async_trait]
impl HidBackend for ScriptedBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(self.nodes.iter().map(|(info, _)| info.clone()).collect())
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        self.enumerate().await
    }

    async fn open_hidpp(&self, node: &NodeInfo) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        match self.open_outcome(&node.id) {
            None | Some(ScriptedOpen::Fails) => Err(BackendError::Disconnected),
            Some(ScriptedOpen::NotHidpp) => Ok(None),
            Some(ScriptedOpen::UnresponsiveHidpp) => {
                let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
                Ok(Some(scripted_channel(raw).await))
            }
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
