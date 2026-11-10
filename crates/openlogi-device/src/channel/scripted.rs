//! A scripted HID++ transport for tests: it answers requests from a per-device
//! responder instead of talking to hardware.
//!
//! Shared because more than one module needs a device with a feature table of
//! its choosing — `write` drives DPI and lighting against one, `host_switch`
//! needs a keyboard whose host slots it can dictate. Each module keeps its own
//! responder; only the plumbing lives here, plus the one device two modules
//! drive: the `scripted_mouse` whose `0x1b04` table capture arming and the
//! diagnostics both reconcile.

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

/// A HID++ 1.0 error response to `request`: sub id `0x8f`, then the request's
/// sub id and address, and the error code. A HID++ 1.0 device reads HID++
/// 2.0's protocol ping as sub id `0x00`, so this answers that ping too.
#[cfg(test)]
pub(crate) fn register_error(request: &[u8], error: u8) -> Vec<u8> {
    vec![0x10, request[1], 0x8f, request[2], request[3], error, 0x00]
}

/// A HID++ 4.5 device's answer to `request` when it is the protocol ping
/// (root feature `0x00`, function 1), else `None`. The answer echoes the ping's
/// header and its ping-data byte.
#[cfg(test)]
pub(crate) fn ping_reply(request: &[u8]) -> Option<Vec<u8>> {
    (request[2] == 0x00 && request[3] >> 4 == 1)
        .then(|| vec![0x10, request[1], 0x00, request[3], 0x04, 0x05, request[6]])
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

/// One scripted `0x1b04` control row: identity, capability flags, and the
/// volatile reporting byte a dead process may have left behind.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) struct ScriptedControl {
    pub(crate) cid: u16,
    pub(crate) task_id: u16,
    pub(crate) flags: u16,
    pub(crate) reporting: u8,
}

/// A HID++ 2.0 mouse whose `0x1b04` (at feature index `0x22`) exposes
/// `controls`; every `setCidReporting` is echoed back, except that writes to
/// `reject` fail with a feature error.
#[cfg(test)]
pub(crate) fn scripted_mouse(
    controls: Vec<ScriptedControl>,
    reject: Option<u16>,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => response[4] = 0x22,
            (0x22, 0) => response[4] = u8::try_from(controls.len()).expect("small table"),
            (0x22, 1) => {
                let row = controls[usize::from(request[4])];
                response[4..6].copy_from_slice(&row.cid.to_be_bytes());
                response[6..8].copy_from_slice(&row.task_id.to_be_bytes());
                let [additional, primary] = row.flags.to_be_bytes();
                response[8] = primary;
                response[12] = additional;
            }
            (0x22, 2) => {
                let cid = u16::from_be_bytes([request[4], request[5]]);
                let row = controls
                    .iter()
                    .find(|row| row.cid == cid)
                    .expect("reporting is only read for exposed controls");
                response[4..6].copy_from_slice(&request[4..6]);
                response[6] = row.reporting;
            }
            (0x22, 3) => {
                let cid = u16::from_be_bytes([request[4], request[5]]);
                if reject == Some(cid) {
                    return Some(feature_error(request, 0x08));
                }
                return Some(request.to_vec());
            }
            _ => return None,
        }
        Some(response)
    }
}

/// True for a `getCidReporting` request on the [`scripted_mouse`] for `cid`.
#[cfg(test)]
pub(crate) fn is_reporting_read_for(request: &[u8], cid: u16) -> bool {
    request.len() >= 6
        && request[2] == 0x22
        && request[3] >> 4 == 2
        && u16::from_be_bytes([request[4], request[5]]) == cid
}

/// `(cid, reporting byte)` of every `setCidReporting` the [`scripted_mouse`]
/// received, in order.
#[cfg(test)]
pub(crate) fn reporting_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<(u16, u8)> {
    handle
        .written_reports()
        .into_iter()
        .filter(|request| request[2] == 0x22 && request[3] >> 4 == 3)
        .map(|request| (u16::from_be_bytes([request[4], request[5]]), request[6]))
        .collect()
}

/// Outcome produced when the scripted backend opens a listed node.
///
/// The cases are distinct contracts the enumerator must not conflate: only
/// [`Self::Fails`] is a backend failure the ledger replays a last-good snapshot
/// through.
#[cfg(test)]
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
#[cfg(test)]
pub(crate) struct ScriptedBackend {
    nodes: Vec<(NodeInfo, ScriptedOpen)>,
}

#[cfg(test)]
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
