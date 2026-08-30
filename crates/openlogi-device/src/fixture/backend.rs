use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use hidpp::channel::HidppChannel;
use tokio::sync::mpsc;

use crate::backend::{BackendError, HidBackend, HotplugEvent, HotplugStream, NodeId, NodeInfo};

use super::barrier::{ReplayResponseBarrier, ResponseGates};
use super::channel::{
    CassetteState, ReplayCompletion, ReplayRawHidChannel, ReplayRawWriter, ReplayRawWriterHandle,
};
use super::schema::{FixtureError, HidCassette, ReportSupport, RequestMatch, validate_report};

/// Whether an OS HID node appears in enumeration snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodePresence {
    /// The host currently enumerates the node.
    Present,
    /// The node is absent even if a previous channel lifetime still exists.
    Absent,
}

/// Outcome of asking the backend to open one present node as HID++.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Open a channel over the node's logical cassette.
    Hidpp,
    /// The node opens but does not expose HID++ reports.
    NotHidpp,
    /// Opening is refused, independently of node presence.
    Denied,
    /// Opening reports that the node disappeared.
    Disconnected,
}

/// Whether one logical HID channel accepts traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelConnection {
    /// New channel lifetimes start connected and accept writes.
    Connected,
    /// Current channel lifetimes reject writes and report disconnected.
    Disconnected,
}

/// Whether a node can be opened as a bare output-report sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawWriterAvailability {
    /// Return a writer that captures successful reports.
    Capture,
    /// Reject attempts to open a raw writer.
    Unavailable,
}

/// Link liveness for a paired receiver slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverLinkState {
    /// The paired device currently answers through the receiver.
    Online,
    /// The pairing remains, but the device link is asleep or unavailable.
    Offline,
}

/// Pairing and link state of one receiver slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverSlotState {
    /// The receiver has no pairing in this slot.
    Empty,
    /// The slot is paired; link liveness remains an independent value.
    Paired(ReceiverLinkState),
}

/// Mutable state of one receiver pairing slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverSlot {
    /// Receiver-assigned slot number.
    pub slot: u8,
    /// Pairing and link state.
    pub state: ReceiverSlotState,
}

/// One logical replay channel and the cassette attached to it.
#[derive(Clone, Debug)]
pub struct ReplayChannel {
    /// Stable channel identifier used by nodes and cassettes.
    pub id: String,
    /// Initial connection state for new channel lifetimes.
    pub connection: ChannelConnection,
    /// HID++ report widths exposed by the channel.
    pub report_support: ReportSupport,
}

/// One virtual OS HID node.
#[derive(Clone, Debug)]
pub struct ReplayNode {
    /// Sanitized host-facing node metadata.
    pub info: NodeInfo,
    /// Initial presence in enumeration snapshots.
    pub presence: NodePresence,
    /// Initial result of opening the node as HID++.
    pub open_outcome: OpenOutcome,
    /// Logical channel used when [`OpenOutcome::Hidpp`] is selected.
    pub channel: Option<String>,
    /// Whether raw output-report opens are available.
    pub raw_writer: RawWriterAvailability,
    /// Receiver pairing/link state, separate from node and channel state.
    pub receiver_slots: Vec<ReceiverSlot>,
}

/// Initial virtual hardware snapshot used to construct a [`ReplayBackend`].
#[derive(Clone, Debug)]
pub struct ReplayTopology {
    /// Virtual OS nodes.
    pub nodes: Vec<ReplayNode>,
    /// Logical channels shared by the routes that traverse them.
    pub channels: Vec<ReplayChannel>,
}

struct NodeRuntime {
    node: ReplayNode,
    open_count: usize,
    raw_written: Arc<Mutex<Vec<Vec<u8>>>>,
    raw_connected: Arc<AtomicBool>,
}

struct ChannelRuntime {
    report_support: ReportSupport,
    connection: ChannelConnection,
    cassette: Arc<CassetteState>,
    response_gates: Arc<ResponseGates>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    lifetimes: Vec<ChannelLifetime>,
    open_count: usize,
}

struct ChannelLifetime {
    connected: Weak<AtomicBool>,
    incoming: mpsc::UnboundedSender<Vec<u8>>,
}

struct BackendState {
    nodes: Vec<NodeRuntime>,
    channels: HashMap<String, ChannelRuntime>,
}

/// A mutable virtual [`HidBackend`] backed by strict raw-HID cassettes.
///
/// Topology mutation and hotplug delivery are intentionally separate: tests
/// may change one without the other to reproduce missed or stale host events.
pub struct ReplayBackend {
    state: Mutex<BackendState>,
    watchers: Mutex<Vec<mpsc::UnboundedSender<HotplugEvent>>>,
}

impl ReplayBackend {
    /// Validate and build a replay backend from topology plus named cassettes.
    pub fn new(
        topology: ReplayTopology,
        cassettes: Vec<HidCassette>,
    ) -> Result<Self, FixtureError> {
        let channels = build_channels(topology.channels, cassettes)?;
        let nodes = build_nodes(topology.nodes, &channels)?;

        Ok(Self {
            state: Mutex::new(BackendState { nodes, channels }),
            watchers: Mutex::new(Vec::new()),
        })
    }

    /// Change whether `node` appears in future enumeration snapshots.
    pub fn set_node_presence(
        &self,
        node: &NodeId,
        presence: NodePresence,
    ) -> Result<(), FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = find_node_mut(&mut state.nodes, node)?;
        runtime.node.presence = presence;
        Ok(())
    }

    /// Change the HID++ open result independently of node presence.
    pub fn set_open_outcome(
        &self,
        node: &NodeId,
        outcome: OpenOutcome,
    ) -> Result<(), FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = find_node_mut(&mut state.nodes, node)?;
        if outcome == OpenOutcome::Hidpp && runtime.node.channel.is_none() {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("node {node} has no logical channel"),
            ));
        }
        runtime.node.open_outcome = outcome;
        Ok(())
    }

    /// Change a logical channel's state.
    ///
    /// Disconnecting also marks all current lifetimes disconnected. Reconnecting
    /// affects only subsequent opens, so a stale lifetime cannot revive.
    pub fn set_channel_connection(
        &self,
        channel: &str,
        connection: ChannelConnection,
    ) -> Result<(), FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .channels
            .get_mut(channel)
            .ok_or_else(|| FixtureError::UnknownChannel(channel.to_string()))?;
        runtime.connection = connection;
        runtime.lifetimes.retain(|lifetime| {
            let Some(connection_flag) = lifetime.connected.upgrade() else {
                return false;
            };
            if connection == ChannelConnection::Disconnected {
                connection_flag.store(false, Ordering::SeqCst);
            }
            true
        });
        Ok(())
    }

    /// Hold the next matching cassette response behind an explicit barrier.
    ///
    /// The request still counts as written and consumes its cassette exchange.
    /// Only response delivery waits for [`ReplayResponseBarrier::release`].
    pub fn hold_next_response(
        &self,
        channel: &str,
        request_match: RequestMatch,
        request: &[u8],
    ) -> Result<ReplayResponseBarrier, FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .channels
            .get(channel)
            .ok_or_else(|| FixtureError::UnknownChannel(channel.to_string()))?;
        validate_report(request, runtime.report_support)
            .map_err(|message| FixtureError::invalid("replay response barrier", message))?;
        Ok(runtime.response_gates.hold(request_match, request))
    }

    /// Deliver an unsolicited report to every connected lifetime of `channel`.
    ///
    /// Returns the number of live channel lifetimes that received the report.
    pub fn emit_channel_report(&self, channel: &str, report: &[u8]) -> Result<usize, FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .channels
            .get_mut(channel)
            .ok_or_else(|| FixtureError::UnknownChannel(channel.to_string()))?;
        validate_report(report, runtime.report_support)
            .map_err(|message| FixtureError::invalid("replay channel report", message))?;
        let mut delivered = 0;
        runtime.lifetimes.retain(|lifetime| {
            let Some(connected) = lifetime.connected.upgrade() else {
                return false;
            };
            if connected.load(Ordering::SeqCst) && lifetime.incoming.send(report.to_vec()).is_ok() {
                delivered += 1;
            }
            true
        });
        Ok(delivered)
    }

    /// Number of current raw-channel lifetimes for one logical channel.
    pub fn channel_lifetime_count(&self, channel: &str) -> Result<usize, FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .channels
            .get_mut(channel)
            .ok_or_else(|| FixtureError::UnknownChannel(channel.to_string()))?;
        runtime
            .lifetimes
            .retain(|lifetime| lifetime.connected.upgrade().is_some());
        Ok(runtime.lifetimes.len())
    }

    /// Change pairing/link state for one declared receiver slot.
    pub fn set_receiver_slot_state(
        &self,
        node: &NodeId,
        slot: u8,
        slot_state: ReceiverSlotState,
    ) -> Result<(), FixtureError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = find_node_mut(&mut state.nodes, node)?;
        let receiver_slot = runtime
            .node
            .receiver_slots
            .iter_mut()
            .find(|candidate| candidate.slot == slot)
            .ok_or_else(|| {
                FixtureError::invalid(
                    "replay topology",
                    format!("node {node} has no receiver slot {slot}"),
                )
            })?;
        receiver_slot.state = slot_state;
        Ok(())
    }

    /// Read the pairing/link state of one declared receiver slot.
    pub fn receiver_slot_state(
        &self,
        node: &NodeId,
        slot: u8,
    ) -> Result<ReceiverSlotState, FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .nodes
            .iter()
            .find(|runtime| runtime.node.info.id == *node)
            .ok_or_else(|| FixtureError::UnknownNode(node.to_string()))?;
        runtime
            .node
            .receiver_slots
            .iter()
            .find(|candidate| candidate.slot == slot)
            .map(|receiver_slot| receiver_slot.state)
            .ok_or_else(|| {
                FixtureError::invalid(
                    "replay topology",
                    format!("node {node} has no receiver slot {slot}"),
                )
            })
    }

    /// Deliver a hotplug event without implicitly changing topology.
    pub fn emit_hotplug(&self, event: HotplugEvent) {
        self.watchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|watcher| watcher.send(event).is_ok());
    }

    /// Number of HID++ open attempts made for `node`.
    pub fn open_count(&self, node: &NodeId) -> Result<usize, FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .nodes
            .iter()
            .find(|runtime| runtime.node.info.id == *node)
            .map(|runtime| runtime.open_count)
            .ok_or_else(|| FixtureError::UnknownNode(node.to_string()))
    }

    /// Current completion report for one logical channel.
    pub fn channel_completion(&self, channel: &str) -> Result<ReplayCompletion, FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .channels
            .get(channel)
            .ok_or_else(|| FixtureError::UnknownChannel(channel.to_string()))?;
        let written = runtime
            .written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut completion = runtime.cassette.completion(written);
        completion.channel_open_count = runtime.open_count;
        Ok(completion)
    }

    /// Fail unless every logical channel consumed all required exchanges.
    pub fn require_complete(&self) -> Result<(), FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for runtime in state.channels.values() {
            let written = runtime
                .written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            runtime.cassette.completion(written).require_complete()?;
        }
        Ok(())
    }

    /// Inspection and connection handle for a node's raw writer.
    pub fn raw_writer_handle(&self, node: &NodeId) -> Result<ReplayRawWriterHandle, FixtureError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .nodes
            .iter()
            .find(|runtime| runtime.node.info.id == *node)
            .ok_or_else(|| FixtureError::UnknownNode(node.to_string()))?;
        Ok(ReplayRawWriterHandle::from_parts(
            Arc::clone(&runtime.raw_written),
            Arc::clone(&runtime.raw_connected),
        ))
    }
}

fn build_channels(
    channels: Vec<ReplayChannel>,
    cassettes: Vec<HidCassette>,
) -> Result<HashMap<String, ChannelRuntime>, FixtureError> {
    let mut cassette_by_channel = HashMap::new();
    for cassette in cassettes {
        cassette.validate()?;
        let channel = cassette.channel.clone();
        if cassette_by_channel
            .insert(channel.clone(), cassette)
            .is_some()
        {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("multiple cassettes name channel {channel}"),
            ));
        }
    }

    let mut runtimes = HashMap::new();
    for channel in channels {
        if channel.id.trim().is_empty() {
            return Err(FixtureError::invalid(
                "replay topology",
                "channel id must not be empty",
            ));
        }
        let Some(cassette) = cassette_by_channel.remove(&channel.id) else {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("channel {} has no cassette", channel.id),
            ));
        };
        if cassette.report_support != channel.report_support {
            return Err(FixtureError::invalid(
                "replay topology",
                format!(
                    "channel {} and its cassette disagree on report support",
                    channel.id
                ),
            ));
        }
        if runtimes
            .insert(
                channel.id.clone(),
                ChannelRuntime {
                    report_support: channel.report_support,
                    connection: channel.connection,
                    cassette: CassetteState::new(cassette)?,
                    response_gates: Arc::new(ResponseGates::default()),
                    written: Arc::new(Mutex::new(Vec::new())),
                    lifetimes: Vec::new(),
                    open_count: 0,
                },
            )
            .is_some()
        {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("duplicate channel {}", channel.id),
            ));
        }
    }
    if let Some(extra) = cassette_by_channel.keys().next() {
        return Err(FixtureError::invalid(
            "replay topology",
            format!("cassette references unknown channel {extra}"),
        ));
    }
    Ok(runtimes)
}

fn build_nodes(
    nodes: Vec<ReplayNode>,
    channels: &HashMap<String, ChannelRuntime>,
) -> Result<Vec<NodeRuntime>, FixtureError> {
    let mut node_ids = HashSet::new();
    let mut runtimes = Vec::new();
    for node in nodes {
        if !node_ids.insert(node.info.id.clone()) {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("duplicate node {}", node.info.id),
            ));
        }
        if node.open_outcome == OpenOutcome::Hidpp && node.channel.is_none() {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("HID++ node {} has no logical channel", node.info.id),
            ));
        }
        if let Some(channel) = &node.channel
            && !channels.contains_key(channel)
        {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("node {} references unknown channel {channel}", node.info.id),
            ));
        }
        validate_receiver_slots(&node)?;
        runtimes.push(NodeRuntime {
            node,
            open_count: 0,
            raw_written: Arc::new(Mutex::new(Vec::new())),
            raw_connected: Arc::new(AtomicBool::new(true)),
        });
    }
    Ok(runtimes)
}

fn validate_receiver_slots(node: &ReplayNode) -> Result<(), FixtureError> {
    let mut slots = HashSet::new();
    for slot in &node.receiver_slots {
        if !(1..=6).contains(&slot.slot) {
            return Err(FixtureError::invalid(
                "replay topology",
                format!(
                    "node {} has invalid receiver slot {}",
                    node.info.id, slot.slot
                ),
            ));
        }
        if !slots.insert(slot.slot) {
            return Err(FixtureError::invalid(
                "replay topology",
                format!("node {} repeats receiver slot {}", node.info.id, slot.slot),
            ));
        }
    }
    Ok(())
}

fn find_node_mut<'a>(
    nodes: &'a mut [NodeRuntime],
    node: &NodeId,
) -> Result<&'a mut NodeRuntime, FixtureError> {
    nodes
        .iter_mut()
        .find(|runtime| runtime.node.info.id == *node)
        .ok_or_else(|| FixtureError::UnknownNode(node.to_string()))
}

#[hidpp::async_trait]
impl HidBackend for ReplayBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(state
            .nodes
            .iter()
            .filter(|runtime| runtime.node.presence == NodePresence::Present)
            .map(|runtime| runtime.node.info.clone())
            .collect())
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        self.enumerate().await
    }

    async fn open_hidpp(&self, node: &NodeInfo) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        let raw = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(index) = state
                .nodes
                .iter()
                .position(|runtime| runtime.node.info.id == node.id)
            else {
                return Err(BackendError::Disconnected);
            };
            if state.nodes[index].node.presence == NodePresence::Absent {
                return Err(BackendError::Disconnected);
            }
            state.nodes[index].open_count += 1;
            match state.nodes[index].node.open_outcome {
                OpenOutcome::NotHidpp => return Ok(None),
                OpenOutcome::Denied => {
                    return Err(BackendError::Backend("replay HID open denied".to_string()));
                }
                OpenOutcome::Disconnected => return Err(BackendError::Disconnected),
                OpenOutcome::Hidpp => {}
            }
            let channel_id =
                state.nodes[index].node.channel.clone().ok_or_else(|| {
                    BackendError::Backend("replay node has no channel".to_string())
                })?;
            let vendor_id = state.nodes[index].node.info.vendor_id;
            let product_id = state.nodes[index].node.info.product_id;
            let channel = state.channels.get_mut(&channel_id).ok_or_else(|| {
                BackendError::Backend(format!("replay channel {channel_id} is missing"))
            })?;
            channel.open_count += 1;
            let connected = Arc::new(AtomicBool::new(
                channel.connection == ChannelConnection::Connected,
            ));
            let raw = ReplayRawHidChannel::from_parts(
                vendor_id,
                product_id,
                channel.report_support,
                Arc::clone(&channel.cassette),
                Arc::clone(&channel.written),
                Arc::clone(&connected),
                Arc::clone(&channel.response_gates),
            );
            channel.lifetimes.push(ChannelLifetime {
                connected: Arc::downgrade(&connected),
                incoming: raw.incoming_sender(),
            });
            raw
        };
        HidppChannel::from_raw_channel(raw)
            .await
            .map(Arc::new)
            .map(Some)
            .map_err(|error| BackendError::Backend(error.to_string()))
    }

    async fn open_raw_writer(
        &self,
        node: &NodeInfo,
    ) -> Result<Box<dyn crate::backend::RawWriter>, BackendError> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let runtime = state
            .nodes
            .iter()
            .find(|runtime| runtime.node.info.id == node.id)
            .ok_or(BackendError::Disconnected)?;
        if runtime.node.presence == NodePresence::Absent {
            return Err(BackendError::Disconnected);
        }
        if runtime.node.raw_writer == RawWriterAvailability::Unavailable {
            return Err(BackendError::Backend(
                "replay node has no raw writer".to_string(),
            ));
        }
        Ok(Box::new(ReplayRawWriter::from_parts(
            Arc::clone(&runtime.raw_written),
            Arc::clone(&runtime.raw_connected),
        )))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        self.watchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        Ok(Box::new(futures_lite::stream::poll_fn(move |context| {
            Pin::new(&mut receiver).poll_recv(context)
        })))
    }
}
