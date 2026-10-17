//! Clipboard sync: the text copied here goes with the pointer to the peer it
//! crosses to.
//!
//! Once a peer accepts a handoff, this machine announces its clipboard text
//! (FLOW-PROTOCOL.md §Clipboard & files). The peer fetches it at once and
//! puts it on its own clipboard, so the text is there before the keyboard
//! that pastes it. Sync needs `CAPABILITY_CLIPBOARD_TEXT` on both ends, which
//! a machine lists only with `[flow] clipboard` on and a clipboard backend for
//! its OS. Text over [`MAX_TEXT_BYTES`] is skipped, never truncated, and no
//! text is ever logged.
//!
//! [`Ledger`] decides what to send and what to keep. One worker runs every
//! clipboard read, fetch and write in the order they were asked for, so a
//! write from an earlier crossing cannot land after a later one; answering a
//! peer's fetch never waits behind it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_clipboard::{ClipboardError, ClipboardText};
use openlogi_flow::clipboard::{
    Announced, AnnouncedText, MAX_TEXT_BYTES, Served, TextDigest, TextOffer, fetch_text,
    serve_fetch, text_sync_enabled,
};
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::transport::{FlowConnection, IncomingRpc, message_envelope};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How long fetching a peer's text may take before it is given up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The OS clipboard as clipboard sync uses it; the seam the tests fake.
pub(super) trait ClipboardBackend: Send + Sync {
    /// Reads the clipboard's text, copying it out only within `max_bytes`.
    fn read_text(&self, max_bytes: usize) -> Result<ClipboardText, ClipboardError>;
    /// Replaces the clipboard's contents with `text`.
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
}

/// This machine's clipboard.
struct SystemClipboard;

impl ClipboardBackend for SystemClipboard {
    fn read_text(&self, max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
        openlogi_clipboard::read_text(max_bytes)
    }

    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        openlogi_clipboard::write_text(text)
    }
}

/// Clipboard sync for one Flow generation.
pub(super) struct ClipboardSync {
    /// `None` while sync is off here.
    worker: Option<Worker>,
}

#[derive(Clone)]
struct Worker {
    shared: Arc<Shared>,
    jobs: mpsc::UnboundedSender<Job>,
}

struct Shared {
    backend: Arc<dyn ClipboardBackend>,
    ledger: Mutex<Ledger>,
}

impl Shared {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

enum Job {
    /// Announce this clipboard's text to a peer that accepted a handoff.
    Offer {
        peer: PublicKey,
        connection: Arc<FlowConnection>,
    },
    /// Fetch the text a peer announced and put it on this clipboard.
    Fetch {
        peer: PublicKey,
        connection: Arc<FlowConnection>,
        announced: AnnouncedText,
        ticket: u64,
    },
}

impl ClipboardSync {
    /// Sync through this machine's clipboard: on when `enabled` (the
    /// `[flow] clipboard` setting) and the OS has a clipboard backend.
    pub(super) fn system(enabled: bool) -> Self {
        if enabled && openlogi_clipboard::available() {
            Self::with_backend(Arc::new(SystemClipboard))
        } else {
            Self { worker: None }
        }
    }

    /// Sync through `backend`. Spawns the worker, so it needs a runtime.
    fn with_backend(backend: Arc<dyn ClipboardBackend>) -> Self {
        let shared = Arc::new(Shared {
            backend,
            ledger: Mutex::new(Ledger::default()),
        });
        let (jobs, queue) = mpsc::unbounded_channel();
        tokio::spawn(run_jobs(Arc::clone(&shared), queue));
        Self {
            worker: Some(Worker { shared, jobs }),
        }
    }

    /// The capabilities this machine lists in its `Hello` for clipboard sync.
    pub(super) fn capabilities(&self) -> Vec<proto::Capability> {
        self.worker
            .iter()
            .map(|_| proto::Capability::ClipboardText)
            .collect()
    }

    /// Offers this clipboard's text to `peer`, which just accepted a handoff.
    /// Does nothing unless both ends negotiated clipboard sync.
    pub(super) fn offer(&self, peer: PublicKey, connection: &Arc<FlowConnection>) {
        let Some(worker) = &self.worker else {
            return;
        };
        if !text_sync_enabled(connection) {
            return;
        }
        let _ = worker.jobs.send(Job::Offer {
            peer,
            connection: Arc::clone(connection),
        });
    }

    /// Clipboard handling for one live connection to `peer`.
    pub(super) fn connection(
        &self,
        peer: PublicKey,
        connection: Arc<FlowConnection>,
    ) -> ConnectionClipboard {
        ConnectionClipboard {
            worker: self.worker.clone(),
            peer,
            connection,
            highest_sequence: 0,
        }
    }
}

/// Clipboard handling for one live peer connection.
pub(super) struct ConnectionClipboard {
    worker: Option<Worker>,
    peer: PublicKey,
    connection: Arc<FlowConnection>,
    /// The newest generation this connection announced, so a reordered older
    /// announce is ignored.
    highest_sequence: u64,
}

impl ConnectionClipboard {
    /// Answers the peer's `ClipboardFetch` from the text offered now, on a
    /// task of its own.
    pub(super) fn serve(&self, rpc: IncomingRpc) {
        let shared = self
            .worker
            .as_ref()
            .map(|worker| Arc::clone(&worker.shared));
        let peer = self.peer;
        tokio::spawn(async move {
            let offer = shared
                .as_ref()
                .and_then(|shared| shared.ledger().current_offer());
            match serve_fetch(rpc, offer.as_deref()).await {
                Ok(Served::Sent { sequence }) => {
                    if let Some(shared) = &shared {
                        shared.ledger().served(peer, sequence);
                    }
                    debug!(sequence, "Flow clipboard text sent");
                }
                Ok(Served::Refused) => {
                    debug!("Flow clipboard fetch for a generation no longer offered refused");
                }
                Err(error) => debug!(%error, "Flow clipboard fetch could not be answered"),
            }
        });
    }

    /// Takes the peer's `ClipboardAnnounce`: the newest one supersedes every
    /// fetch still pending, and text within the limit is fetched.
    pub(super) fn announced(&mut self, envelope: &Envelope) {
        let Some(worker) = &self.worker else {
            return;
        };
        let Ok(announce) = envelope.decode::<proto::ClipboardAnnounce>(InboundRole::Notification)
        else {
            return;
        };
        if announce.sequence <= self.highest_sequence {
            debug!(
                sequence = announce.sequence,
                "stale Flow clipboard announce ignored"
            );
            return;
        }
        self.highest_sequence = announce.sequence;
        let ticket = worker.shared.ledger().announced();
        match Announced::from_announce(&announce) {
            Announced::Text(announced) => {
                let _ = worker.jobs.send(Job::Fetch {
                    peer: self.peer,
                    connection: Arc::clone(&self.connection),
                    announced,
                    ticket,
                });
            }
            Announced::TooLarge => {
                debug!("Flow clipboard text over the sync limit skipped");
            }
            Announced::NoText => {}
        }
    }
}

async fn run_jobs(shared: Arc<Shared>, mut queue: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = queue.recv().await {
        match job {
            Job::Offer { peer, connection } => offer_text(&shared, peer, &connection).await,
            Job::Fetch {
                peer,
                connection,
                announced,
                ticket,
            } => receive_text(&shared, peer, &connection, &announced, ticket).await,
        }
    }
}

async fn offer_text(shared: &Arc<Shared>, peer: PublicKey, connection: &FlowConnection) {
    let text = match read_local(shared).await {
        Some(ClipboardText::Text(text)) => text,
        Some(ClipboardText::TooLarge) => {
            debug!("Flow clipboard text over the sync limit not offered");
            return;
        }
        Some(ClipboardText::Absent) | None => return,
    };
    let Some(offer) = shared.ledger().offer(peer, text) else {
        debug!("Flow clipboard text already shared with this peer");
        return;
    };
    let announce = match message_envelope(FrameKind::ClipboardAnnounce, &offer.announce()) {
        Ok(announce) => announce,
        Err(error) => {
            warn!(%error, "Flow clipboard announce could not be encoded");
            return;
        }
    };
    match connection.notify(announce).await {
        Ok(()) => debug!(
            sequence = offer.sequence(),
            bytes = offer.size_bytes(),
            "Flow clipboard text offered"
        ),
        Err(error) => debug!(%error, "Flow clipboard announce failed"),
    }
}

async fn receive_text(
    shared: &Arc<Shared>,
    peer: PublicKey,
    connection: &FlowConnection,
    announced: &AnnouncedText,
    ticket: u64,
) {
    if !shared.ledger().is_newest(ticket) {
        return;
    }
    let held = match read_local(shared).await {
        Some(ClipboardText::Text(text)) => Some(TextDigest::of(&text)),
        _ => None,
    };
    if let Some(held) = held.filter(|held| announced.digest() == Some(*held)) {
        // Already here: nothing to fetch or write, but both ends share it now.
        shared.ledger().adopted(peer, held);
        return;
    }
    let text = match tokio::time::timeout(FETCH_TIMEOUT, fetch_text(connection, announced)).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            debug!(%error, "Flow clipboard fetch failed");
            return;
        }
        Err(_) => {
            debug!("Flow clipboard fetch timed out");
            return;
        }
    };
    if !shared.ledger().is_newest(ticket) {
        debug!("Flow clipboard text superseded while it was fetched");
        return;
    }
    let digest = TextDigest::of(&text);
    let backend = Arc::clone(&shared.backend);
    match tokio::task::spawn_blocking(move || backend.write_text(&text)).await {
        Ok(Ok(())) => {
            shared.ledger().adopted(peer, digest);
            info!("Flow clipboard text received");
        }
        Ok(Err(error)) => warn!(%error, "Flow could not place the received clipboard text"),
        Err(error) => warn!(%error, "Flow clipboard write task failed"),
    }
}

/// This clipboard's text, or `None` when it cannot be read.
async fn read_local(shared: &Arc<Shared>) -> Option<ClipboardText> {
    let backend = Arc::clone(&shared.backend);
    match tokio::task::spawn_blocking(move || backend.read_text(MAX_TEXT_BYTES)).await {
        Ok(Ok(text)) => Some(text),
        Ok(Err(error)) => {
            debug!(%error, "Flow could not read the clipboard");
            None
        }
        Err(error) => {
            debug!(%error, "Flow clipboard read task failed");
            None
        }
    }
}

/// What clipboard sync knows and decides by; no I/O.
#[derive(Default)]
struct Ledger {
    /// The last generation this machine announced; the first is 1.
    sequence: u64,
    /// The generation peers may fetch: the last one announced.
    offer: Option<Arc<TextOffer>>,
    /// The text of the last sync either way: the last offer a peer fetched,
    /// or the last text taken from a peer. Clipboard text that differs was
    /// copied here since.
    last_synced: Option<TextDigest>,
    /// The text each peer is known to hold, from the same two events.
    peer_holds: HashMap<PublicKey, TextDigest>,
    /// Arrival order of the newest peer announce.
    newest: u64,
}

impl Ledger {
    /// The generation to announce to `peer` for `text`, just read off this
    /// clipboard; `None` when the peer gains nothing from it.
    ///
    /// That is only when nothing was copied here since the last sync and the
    /// peer is known to hold the text. Resending it otherwise would put stale
    /// text back over whatever the peer's user copied since — a screenshot,
    /// say, which this sync cannot carry.
    fn offer(&mut self, peer: PublicKey, text: String) -> Option<Arc<TextOffer>> {
        let digest = TextDigest::of(&text);
        if self.last_synced == Some(digest) && self.peer_holds.get(&peer) == Some(&digest) {
            return None;
        }
        let offer = Arc::new(TextOffer::new(self.sequence + 1, text).ok()?);
        self.sequence = offer.sequence();
        self.offer = Some(Arc::clone(&offer));
        Some(offer)
    }

    fn current_offer(&self) -> Option<Arc<TextOffer>> {
        self.offer.clone()
    }

    /// `peer` fetched generation `sequence` in full.
    fn served(&mut self, peer: PublicKey, sequence: u64) {
        let Some(digest) = self
            .offer
            .as_ref()
            .filter(|offer| offer.sequence() == sequence)
            .map(|offer| offer.digest())
        else {
            return;
        };
        self.synced(peer, digest);
    }

    /// Text `peer` announced is on this clipboard now, written or already
    /// there.
    fn adopted(&mut self, peer: PublicKey, digest: TextDigest) {
        self.synced(peer, digest);
    }

    fn synced(&mut self, peer: PublicKey, digest: TextDigest) {
        self.last_synced = Some(digest);
        self.peer_holds.insert(peer, digest);
    }

    /// A peer announced a new generation. Returns its ticket; a fetch for an
    /// older ticket must not be written.
    fn announced(&mut self) -> u64 {
        self.newest += 1;
        self.newest
    }

    fn is_newest(&self, ticket: u64) -> bool {
        ticket == self.newest
    }
}

#[cfg(test)]
mod tests;
