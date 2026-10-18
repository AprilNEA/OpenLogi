//! The Flow actor: the one owner of a generation's mutable state.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::config::{FlowConfig, FlowPeer, FlowPeerName, FlowPublicKey};
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::session::{LinkState, TrustedInitialState};
use openlogi_flow::transport::{IncomingRpc, TransportError, error_envelope, message_envelope};
use openlogi_ipc::{FlowLinkState, FlowPeerStatus, FlowStatus};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::Event;
use super::link::NOTIFY_TIMEOUT;
use crate::flow::devices::{InventoryDevice, LocalDevices};
use crate::flow::edge::EdgeCrossing;
use crate::flow::handoff::{ArmDecision, CancelOutcome, CompletedTransfer, IncomingLedger};
use crate::flow::pointer::PointerThread;
use crate::observable::ObservableState;

/// One configured peer as the actor sees it.
pub(super) struct ActorPeer {
    key: PublicKey,
    name: FlowPeerName,
    public_key: FlowPublicKey,
    link: LinkState,
    frames: mpsc::Sender<Envelope>,
}

impl ActorPeer {
    pub(super) fn new(key: PublicKey, peer: &FlowPeer, frames: mpsc::Sender<Envelope>) -> Self {
        Self {
            key,
            name: peer.name.clone(),
            public_key: peer.public_key,
            link: LinkState::Lost,
            frames,
        }
    }
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
    events: mpsc::Receiver<Event>,
    crossings: mpsc::Receiver<EdgeCrossing>,
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
        events: mpsc::Receiver<Event>,
        crossings: mpsc::Receiver<EdgeCrossing>,
        stop: watch::Receiver<bool>,
    ) -> Self {
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
            events,
            crossings,
            stop,
        };
        actor.publish_state();
        actor.publish_status();
        actor
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
                crossing = self.crossings.recv() => match crossing {
                    Some(crossing) => debug!(?crossing.side, "Flow edge crossing ignored"),
                    None => break,
                },
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
        }
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
