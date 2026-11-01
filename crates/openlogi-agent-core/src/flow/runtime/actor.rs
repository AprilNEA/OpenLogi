//! The Flow actor: the one owner of a generation's mutable state.

use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::{FlowConfig, FlowEdge, FlowPeer, FlowPeerName, FlowPublicKey};
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::session::{LinkState, PeerSessionHandle, SessionManager, TrustedInitialState};
use openlogi_flow::transport::{IncomingRpc, TransportError, error_envelope, message_envelope};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::link::NOTIFY_TIMEOUT;
use super::sender::{self, FlowHardware, Negotiation, SendReport};
use super::{Event, unix_millis};
use crate::flow::devices::{InventoryDevice, LocalDevices};
use crate::flow::edge::{ArmedSides, EdgeCrossing, EdgeSide};
use crate::flow::handoff::{
    ArmDecision, CancelOutcome, CompletedTransfer, IncomingLedger, entry_point,
};
use crate::flow::pointer::{PointerSettings, PointerThread};
use crate::observable::ObservableState;

/// One configured peer as the actor sees it.
pub(super) struct ActorPeer {
    key: PublicKey,
    name: FlowPeerName,
    public_key: FlowPublicKey,
    link: LinkState,
    frames: mpsc::Sender<Envelope>,
    /// Set by [`Actor::attach`] once the sessions run.
    handle: Option<PeerSessionHandle>,
}

impl ActorPeer {
    pub(super) fn new(key: PublicKey, peer: &FlowPeer, frames: mpsc::Sender<Envelope>) -> Self {
        Self {
            key,
            name: peer.name.clone(),
            public_key: peer.public_key,
            link: LinkState::Lost,
            frames,
            handle: None,
        }
    }
}

/// How long a stopping actor waits for its outgoing transfer.
const SENDER_GRACE: Duration = Duration::from_secs(3);

/// The channels the actor reads, and the sender its transfer tasks report on.
pub(super) struct ActorInputs {
    pub(super) events: mpsc::Receiver<Event>,
    pub(super) events_tx: mpsc::Sender<Event>,
    pub(super) crossings: mpsc::Receiver<EdgeCrossing>,
    pub(super) stop: watch::Receiver<bool>,
}

/// The one transfer this host is sending.
struct Outgoing {
    peer: PublicKey,
    transfer_id: u64,
    /// Taken when the peer's result arrives.
    result: Option<oneshot::Sender<proto::HandoffResult>>,
    task: JoinHandle<()>,
}

pub(super) struct Actor {
    config: Arc<FlowConfig>,
    peers: Vec<ActorPeer>,
    inventory: watch::Receiver<Arc<[InventoryDevice]>>,
    devices: LocalDevices,
    ledger: IncomingLedger,
    state: watch::Sender<TrustedInitialState>,
    revision: u64,
    pointer: PointerThread,
    observable: Arc<ObservableState>,
    hardware: FlowHardware,
    outgoing: Option<Outgoing>,
    events: mpsc::Receiver<Event>,
    events_tx: mpsc::Sender<Event>,
    crossings: mpsc::Receiver<EdgeCrossing>,
    /// False once the pointer thread has gone, which it does at once when
    /// this session has no pointer probe (Wayland, or no display). Incoming
    /// handoffs still work; only outgoing ones need a crossing.
    crossings_open: bool,
    stop: watch::Receiver<bool>,
}

impl Actor {
    #[expect(
        clippy::too_many_arguments,
        reason = "built once, from the generation's parts"
    )]
    pub(super) fn new(
        config: Arc<FlowConfig>,
        peers: Vec<ActorPeer>,
        mut inventory: watch::Receiver<Arc<[InventoryDevice]>>,
        state: watch::Sender<TrustedInitialState>,
        pointer: PointerThread,
        observable: Arc<ObservableState>,
        hardware: FlowHardware,
        inputs: ActorInputs,
    ) -> Self {
        let ActorInputs {
            events,
            events_tx,
            crossings,
            stop,
        } = inputs;
        let devices = LocalDevices::join(&config, &inventory.borrow_and_update());
        let mut actor = Self {
            config,
            peers,
            inventory,
            devices,
            ledger: IncomingLedger::default(),
            state,
            revision: initial_revision(),
            pointer,
            observable,
            hardware,
            outgoing: None,
            events,
            events_tx,
            crossings,
            crossings_open: true,
            stop,
        };
        actor.publish_state();
        actor.publish_status();
        actor
    }

    /// Give each peer its session handle, then arm the edges of the
    /// connected peers.
    pub(super) fn attach(&mut self, sessions: &SessionManager) {
        for peer in &mut self.peers {
            peer.handle = sessions.peer(peer.key).cloned();
        }
        self.arm_pointer();
    }

    pub(super) async fn run(mut self) {
        loop {
            let deadline = self.ledger.deadline();
            tokio::select! {
                biased;
                _ = self.stop.changed() => break,
                event = self.events.recv() => match event {
                    Some(event) => self.handle(event),
                    None => break,
                },
                changed = self.inventory.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    self.inventory_changed();
                }
                () = sleep_until(deadline) => self.expire(),
                crossing = self.crossings.recv(), if self.crossings_open => {
                    if let Some(crossing) = crossing {
                        self.crossed(&crossing);
                    } else {
                        info!("Flow has no pointer in this session; outgoing handoffs are off");
                        self.crossings_open = false;
                    }
                }
            }
        }
        if let Some(outgoing) = self.outgoing.take() {
            // The sender sees the stop signal and returns without waiting for
            // a result.
            let mut task = outgoing.task;
            if tokio::time::timeout(SENDER_GRACE, &mut task).await.is_err() {
                warn!("the Flow sender did not stop in time");
                task.abort();
            }
        }
        let pointer = self.pointer;
        if tokio::task::spawn_blocking(move || pointer.stop())
            .await
            .is_err()
        {
            warn!("the Flow pointer thread could not be stopped");
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Link { peer, state } => self.link_changed(peer, state),
            Event::Request { peer, rpc } => self.request(peer, rpc),
            Event::Notification { peer, envelope } => self.notification(peer, &envelope),
            Event::Sent {
                transfer_id,
                report,
            } => self.sent(transfer_id, &report),
        }
    }

    fn link_changed(&mut self, key: PublicKey, state: LinkState) {
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.key == key) else {
            return;
        };
        if peer.link != state {
            info!(peer = %peer.name, ?state, "Flow peer link changed");
            peer.link = state;
            self.publish_status();
            self.arm_pointer();
        }
    }

    /// Arm the edges whose peer is connected.
    fn arm_pointer(&self) {
        let sides = self
            .config
            .layout()
            .iter()
            .filter(|layout| {
                self.peers
                    .iter()
                    .any(|peer| peer.name == layout.peer && peer.link == LinkState::Connected)
            })
            .map(|layout| edge_side(layout.edge));
        self.pointer.configure(PointerSettings {
            sides: ArmedSides::from_sides(sides),
            require_modifier: self.config.require_modifier,
        });
    }

    /// Start handing the devices to the peer beyond the crossed edge. One
    /// transfer runs at a time, in either direction.
    fn crossed(&mut self, crossing: &EdgeCrossing) {
        if self.outgoing.is_some() || self.ledger.deadline().is_some() {
            debug!(?crossing.side, "Flow transfer in progress; crossing ignored");
            return;
        }
        let Some(peer) = self
            .config
            .layout()
            .iter()
            .find(|layout| edge_side(layout.edge) == crossing.side)
            .and_then(|layout| self.peers.iter().find(|peer| peer.name == layout.peer))
        else {
            return;
        };
        let connection = (peer.link == LinkState::Connected)
            .then(|| peer.handle.as_ref().and_then(PeerSessionHandle::connection))
            .flatten();
        let Some(connection) = connection else {
            debug!(peer = %peer.name, "Flow peer not connected; crossing ignored");
            return;
        };
        let devices = self.devices.outgoing(&peer.name);
        if devices.is_empty() {
            debug!(peer = %peer.name, "no device to hand to the Flow peer");
            return;
        }
        let transfer_id = sender::new_transfer_id();
        info!(
            peer = %peer.name,
            transfer = transfer_id,
            devices = devices.len(),
            "Flow handoff starting"
        );
        let (result_tx, result_rx) = oneshot::channel();
        let hardware = self.hardware.clone();
        let entry = entry_point(crossing);
        let stop = self.stop.clone();
        let events = self.events_tx.clone();
        let task = tokio::spawn(async move {
            let report = match sender::acquire_and_prepare(&hardware, &devices).await {
                Ok((lease, switches)) => {
                    let negotiation = Negotiation {
                        connection,
                        transfer_id,
                        entry,
                        switches,
                        result: result_rx,
                        stop,
                        device_io: hardware.device_io.clone(),
                    };
                    sender::negotiate(negotiation, lease).await
                }
                Err(report) => report,
            };
            // The actor may have stopped; nothing is left to tell then.
            let _ = events
                .send(Event::Sent {
                    transfer_id,
                    report,
                })
                .await;
        });
        self.outgoing = Some(Outgoing {
            peer: peer.key,
            transfer_id,
            result: Some(result_tx),
            task,
        });
    }

    fn sent(&mut self, transfer_id: u64, report: &SendReport) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|outgoing| outgoing.transfer_id == transfer_id)
        {
            self.outgoing = None;
        }
        info!(transfer = transfer_id, ?report, "Flow handoff sent");
    }

    fn request(&mut self, peer: PublicKey, rpc: IncomingRpc) {
        let request = rpc.request();
        let encoded = if request.kind == FrameKind::HandoffRequest {
            match request.decode::<proto::HandoffRequest>(InboundRole::Request) {
                Ok(request) => self.handoff_request(peer, &request),
                Err(_) => error_envelope(proto::ErrorCode::Invalid, "malformed handoff request"),
            }
        } else {
            error_envelope(proto::ErrorCode::UnsupportedKind, "unsupported request")
        };
        let response = match encoded {
            Ok(response) => response,
            Err(error) => {
                // Dropping the stream resets it; the sender's deadline ends
                // the transfer.
                warn!(%error, "could not encode a Flow response");
                return;
            }
        };
        // Responding runs on its own, under a deadline. A failed response does
        // not disarm the transfer: the accept may have reached the sender, so
        // the transfer runs to its deadline.
        tokio::spawn(async move {
            match tokio::time::timeout(NOTIFY_TIMEOUT, rpc.respond(response)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => debug!(%error, "Flow response not delivered"),
                Err(_) => debug!("Flow response timed out"),
            }
        });
    }

    fn handoff_request(
        &mut self,
        peer: PublicKey,
        request: &proto::HandoffRequest,
    ) -> Result<Envelope, TransportError> {
        let decision = self.ledger.arm(
            peer,
            request,
            &self.devices,
            self.config.enabled,
            Instant::now(),
        );
        match &decision {
            ArmDecision::Armed(accept) => {
                info!(transfer = accept.transfer_id, "Flow handoff armed");
                message_envelope(FrameKind::HandoffAccept, accept)
            }
            ArmDecision::Duplicate(accept) => message_envelope(FrameKind::HandoffAccept, accept),
            ArmDecision::Replay { accept, result } => {
                self.send(peer, message_envelope(FrameKind::HandoffResult, result));
                message_envelope(FrameKind::HandoffAccept, accept)
            }
            ArmDecision::Rejected(reject) => {
                debug!(transfer = reject.transfer_id, reason = ?reject.reason, "Flow handoff rejected");
                message_envelope(FrameKind::HandoffReject, reject)
            }
        }
    }

    fn notification(&mut self, peer: PublicKey, envelope: &Envelope) {
        match envelope.kind {
            FrameKind::HandoffCancel => {
                let Ok(cancel) = envelope.decode::<proto::HandoffCancel>(InboundRole::Notification)
                else {
                    debug!("malformed Flow handoff cancel");
                    return;
                };
                match self.ledger.cancel(peer, cancel.transfer_id) {
                    CancelOutcome::Disarmed => {
                        info!(transfer = cancel.transfer_id, "Flow handoff cancelled");
                    }
                    CancelOutcome::Ignored => {
                        info!(
                            transfer = cancel.transfer_id,
                            "Flow handoff cancel ignored: a device already arrived"
                        );
                    }
                    CancelOutcome::Unknown => {}
                }
            }
            FrameKind::HandoffResult => {
                let Ok(result) = envelope.decode::<proto::HandoffResult>(InboundRole::Notification)
                else {
                    debug!("malformed Flow handoff result");
                    return;
                };
                let waiting = self
                    .outgoing
                    .as_mut()
                    .filter(|outgoing| {
                        outgoing.peer == peer && outgoing.transfer_id == result.transfer_id
                    })
                    .and_then(|outgoing| outgoing.result.take());
                if let Some(waiting) = waiting {
                    // The sender may have given up already.
                    let _ = waiting.send(result);
                }
            }
            kind => debug!(?kind, "Flow notification ignored"),
        }
    }

    fn inventory_changed(&mut self) {
        let inventory = Arc::clone(&self.inventory.borrow_and_update());
        self.devices = LocalDevices::join(&self.config, &inventory);
        let observation = self.ledger.observe(&self.devices, Instant::now());
        if let Some(entry) = observation.entry {
            self.pointer.place(entry);
        }
        if let Some(done) = observation.completed {
            self.finish(&done);
        }
        self.publish_state();
    }

    fn expire(&mut self) {
        if let Some(done) = self.ledger.expire(Instant::now()) {
            self.finish(&done);
        }
    }

    fn finish(&self, done: &CompletedTransfer) {
        info!(
            transfer = done.result.transfer_id,
            outcome = ?done.result.outcome,
            "Flow handoff finished"
        );
        self.send(
            done.peer,
            message_envelope(FrameKind::HandoffResult, &done.result),
        );
    }

    /// Queue an encoded frame for one peer's writer. A full queue drops the
    /// frame.
    fn send(&self, key: PublicKey, encoded: Result<Envelope, TransportError>) {
        let Some(peer) = self.peers.iter().find(|peer| peer.key == key) else {
            return;
        };
        let envelope = match encoded {
            Ok(envelope) => envelope,
            Err(error) => {
                warn!(%error, "could not encode a Flow frame");
                return;
            }
        };
        let kind = envelope.kind;
        if peer.frames.try_send(envelope).is_err() {
            warn!(peer = %peer.name, ?kind, "Flow peer queue is full; dropped a frame");
        }
    }

    /// Publish the whole device state under a new revision.
    fn publish_state(&mut self) {
        self.revision += 1;
        let state = TrustedInitialState {
            announce_devices: self.devices.announce(self.revision),
            peer_state: proto::PeerState {
                flow_enabled: self.config.enabled,
                held: self.devices.held(),
                revision: self.revision,
                ..Default::default()
            },
        };
        self.state.send_replace(state);
    }

    fn publish_status(&self) {
        self.observable.set_flow(FlowStatus {
            enabled: self.config.enabled,
            peers: self
                .peers
                .iter()
                .map(|peer| FlowPeerStatus {
                    name: peer.name.clone(),
                    public_key: peer.public_key,
                    state: match peer.link {
                        LinkState::Connected => FlowLinkState::Connected,
                        LinkState::Degraded => FlowLinkState::Degraded,
                        LinkState::Lost => FlowLinkState::Lost,
                    },
                })
                .collect(),
        });
    }
}

/// A revision above any this machine sent before a restart: peers drop state
/// older than what they hold, so counting from zero again would be ignored.
fn initial_revision() -> u64 {
    unix_millis()
}

const fn edge_side(edge: FlowEdge) -> EdgeSide {
    match edge {
        FlowEdge::Left => EdgeSide::Left,
        FlowEdge::Right => EdgeSide::Right,
        FlowEdge::Top => EdgeSide::Top,
        FlowEdge::Bottom => EdgeSide::Bottom,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
