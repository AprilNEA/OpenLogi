//! The tasks that carry one peer's frames between its connection and the
//! Flow actor.
//!
//! Each peer has three tasks. One reports link-state changes. One accepts the
//! peer's streams and reads each in its own task, under the transport's read
//! deadline, so a slow stream never blocks the next one. One writes: it drains
//! a bounded queue of transfer frames and resends the whole device state when
//! it changes, each send under [`NOTIFY_TIMEOUT`]. The actor never waits on
//! the network; it only queues.

use std::time::Duration;

use openlogi_flow::frame::{Envelope, FrameKind};
use openlogi_flow::session::{PeerSessionHandle, TrustedInitialState};
use openlogi_flow::transport::{
    FlowConnection, NotificationEvent, RpcEvent, TransportError, message_envelope,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tracing::debug;

use super::Event;

/// Longest wait for one outbound notification.
pub(super) const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);
/// Inbound streams one connection may have in flight before the next accept
/// waits.
const MAX_STREAM_READS: usize = 8;

/// Forward the peer's link state to the actor until the runtime stops.
pub(super) async fn watch_link(
    handle: PeerSessionHandle,
    events: mpsc::Sender<Event>,
    mut stop: watch::Receiver<bool>,
) {
    let peer = handle.public_key();
    let mut state = handle.subscribe_state();
    loop {
        let link = *state.borrow_and_update();
        if events
            .send(Event::Link { peer, state: link })
            .await
            .is_err()
        {
            return;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return,
            changed = state.changed() => if changed.is_err() {
                return;
            },
        }
    }
}

/// Read every stream the peer opens, following connection replacements,
/// until the runtime stops.
pub(super) async fn read_peer(
    handle: PeerSessionHandle,
    events: mpsc::Sender<Event>,
    mut stop: watch::Receiver<bool>,
) {
    let mut connection = handle.subscribe_connection();
    loop {
        let current = connection.borrow_and_update().clone();
        let serve = async {
            if let Some(current) = current {
                serve(&current, &events).await;
            }
            // Closed or absent: wait for the session to replace it.
            std::future::pending::<()>().await;
        };
        tokio::select! {
            biased;
            _ = stop.changed() => return,
            changed = connection.changed() => if changed.is_err() {
                return;
            },
            () = serve => {}
        }
    }
}

/// Accept streams on one connection until it closes. Dropping the future
/// aborts the reads still in flight.
async fn serve(connection: &FlowConnection, events: &mpsc::Sender<Event>) {
    let peer = connection.peer_key();
    let mut reads = JoinSet::new();
    loop {
        let room = reads.len() < MAX_STREAM_READS;
        tokio::select! {
            rpc = connection.accept_rpc_stream(), if room => {
                let Ok(inbound) = rpc else { return };
                let events = events.clone();
                reads.spawn(async move {
                    match inbound.read().await {
                        Ok(RpcEvent::Request(rpc)) => {
                            let _ = events.send(Event::Request { peer, rpc }).await;
                        }
                        Ok(RpcEvent::Rejected(code)) => {
                            debug!(?code, "refused a Flow request from the peer");
                        }
                        Err(error) => debug!(%error, "Flow request stream failed"),
                    }
                });
            }
            notification = connection.accept_notification_stream(), if room => {
                let Ok(inbound) = notification else { return };
                let events = events.clone();
                reads.spawn(async move {
                    match inbound.read().await {
                        Ok(NotificationEvent::Notification(envelope)) => {
                            let _ = events.send(Event::Notification { peer, envelope }).await;
                        }
                        Ok(NotificationEvent::Dropped(code)) => {
                            debug!(?code, "dropped a Flow notification from the peer");
                        }
                        Err(error) => debug!(%error, "Flow notification stream failed"),
                    }
                });
            }
            Some(_) = reads.join_next(), if !reads.is_empty() => {}
        }
    }
}

/// Send the actor's frames and state changes to the peer until the runtime
/// stops.
pub(super) async fn write_peer(
    handle: PeerSessionHandle,
    mut frames: mpsc::Receiver<Envelope>,
    mut state: watch::Receiver<TrustedInitialState>,
    mut stop: watch::Receiver<bool>,
) {
    // The session sends the state current at trust time; send only changes.
    state.mark_unchanged();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => return,
            frame = frames.recv() => {
                let Some(frame) = frame else { return };
                deliver(&handle, frame).await;
            }
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                let current = state.borrow_and_update().clone();
                for envelope in [
                    message_envelope(FrameKind::AnnounceDevices, &current.announce_devices),
                    message_envelope(FrameKind::PeerState, &current.peer_state),
                ] {
                    match envelope {
                        Ok(envelope) => deliver(&handle, envelope).await,
                        Err(error) => debug!(%error, "could not encode the Flow state"),
                    }
                }
            }
        }
    }
}

/// Send one notification on the peer's current connection. A peer without a
/// connection misses it; whole-state frames heal on reconnect, and a lost
/// transfer frame ends in the other side's deadline.
async fn deliver(handle: &PeerSessionHandle, envelope: Envelope) {
    let kind = envelope.kind;
    let Some(connection) = handle.connection() else {
        debug!(?kind, "no Flow connection; dropped a notification");
        return;
    };
    let outcome = tokio::time::timeout(NOTIFY_TIMEOUT, connection.notify(envelope))
        .await
        .unwrap_or(Err(TransportError::StreamTimeout));
    if let Err(error) = outcome {
        debug!(?kind, %error, "Flow notification not delivered");
    }
}
