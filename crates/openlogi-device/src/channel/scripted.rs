//! A scripted HID++ transport for tests: it answers requests from a per-device
//! responder instead of talking to hardware.
//!
//! Shared because more than one module needs a device with a feature table of
//! its choosing — `write` drives DPI and lighting against one, `host_switch`
//! needs a keyboard whose host slots it can dictate. Each module keeps its own
//! responder; only the plumbing lives here.

use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::channel::{HidppChannel, RawHidChannel};
use tokio::sync::mpsc;

#[cfg(any(test, feature = "test-support"))]
use crate::backend::NodeId;
#[cfg(test)]
use crate::backend::{BackendError, HidBackend, HotplugStream, NodeInfo, RawWriter};
#[cfg(feature = "test-support")]
use crate::{ChannelRegistry, DeviceRoute, SharedChannel};

/// Captured reports written to a scripted HID++ channel.
#[derive(Clone)]
pub struct ScriptedRawHidHandle {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ScriptedRawHidHandle {
    /// Return an owned snapshot of every raw report written so far.
    pub fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Answers a HID++ request as a particular scripted device would.
#[cfg(test)]
pub(crate) type Responder = fn(&[u8]) -> Option<Vec<u8>>;
type DynamicResponder = Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// Decides whether a raw write fails at the transport rather than reaching the
/// device — the shape a node that has gone away takes.
#[cfg(test)]
pub(crate) type WriteFailure = fn(&[u8]) -> bool;

pub(crate) struct ScriptedRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    responder: DynamicResponder,
    #[cfg(test)]
    fails: Option<WriteFailure>,
}

impl ScriptedRawHidChannel {
    /// A channel answering as `responder`'s device.
    #[cfg(test)]
    pub(crate) fn with_responder(responder: Responder) -> (Self, ScriptedRawHidHandle) {
        Self::build(responder, None)
    }

    /// A channel whose responder needs per-test captured state.
    pub(crate) fn with_dynamic_responder(
        responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> (Self, ScriptedRawHidHandle) {
        #[cfg(test)]
        {
            Self::build(responder, None)
        }
        #[cfg(not(test))]
        {
            Self::build(responder)
        }
    }

    /// The same, except that a write `fails` selects errors at the transport
    /// instead of being answered: a device whose HID node disappears part-way
    /// through a conversation, which is a different failure from a device that
    /// answered with a refusal.
    #[cfg(test)]
    pub(crate) fn with_failing_writes(
        responder: Responder,
        fails: WriteFailure,
    ) -> (Self, ScriptedRawHidHandle) {
        Self::build(responder, Some(fails))
    }

    fn build(
        responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
        #[cfg(test)] fails: Option<WriteFailure>,
    ) -> (Self, ScriptedRawHidHandle) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written: Arc::clone(&written),
                responder: Arc::new(responder),
                #[cfg(test)]
                fails,
            },
            ScriptedRawHidHandle { written },
        )
    }
}

#[hidpp::async_trait]
impl RawHidChannel for ScriptedRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xb35b
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(src.to_vec());
        #[cfg(test)]
        if self.fails.is_some_and(|fails| fails(src)) {
            return Err(mock_error());
        }
        if let Some(response) = (self.responder)(src) {
            self.incoming_tx.send(response).map_err(|_| mock_error())?;
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(mock_error());
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        unreachable!("scripted channel declares HID++ support")
    }
}

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

pub(crate) fn mock_error() -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "scripted HID channel closed",
    ))
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
) -> (SharedChannel, ScriptedRawHidHandle) {
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
) -> (SharedChannel, ScriptedRawHidHandle) {
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
