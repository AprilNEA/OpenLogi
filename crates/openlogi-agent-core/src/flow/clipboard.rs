//! Clipboard sync: the text or files copied here go with the pointer to the
//! peer it crosses to.
//!
//! Once a peer accepts a handoff, this machine announces its clipboard
//! (FLOW-PROTOCOL.md §Clipboard & files). The peer fetches it at once and puts
//! it on its own clipboard, so it is there before the keyboard that pastes it:
//! text as text, and files downloaded into a folder of OpenLogi's, then placed
//! as a file copy. Text needs `CAPABILITY_CLIPBOARD_TEXT` on both ends, and
//! files `CAPABILITY_CLIPBOARD_FILES` as well; a machine lists them only with
//! `[flow] clipboard` on and a clipboard backend for its OS. Content over the
//! limits ([`MAX_TEXT_BYTES`], [`MAX_FILES`], [`MAX_FILES_BYTES`]) is skipped,
//! never truncated, and neither text nor a file name is ever logged.
//!
//! [`Ledger`] decides what to send and what to keep. One worker runs every
//! clipboard read, fetch and write in the order they were asked for, so a
//! write from an earlier crossing cannot land after a later one; answering a
//! peer's fetch never waits behind it.
//!
//! [`MAX_FILES`]: openlogi_flow::clipboard::MAX_FILES
//! [`MAX_FILES_BYTES`]: openlogi_flow::clipboard::MAX_FILES_BYTES

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_clipboard::{ClipboardError, ClipboardText};
use openlogi_flow::clipboard::{
    Announced, AnnouncedFiles, AnnouncedText, ClipboardOffer, ContentDigest, FilesOffer,
    ListedFile, MAX_TEXT_BYTES, OfferedFile, Served, TextOffer, fetch_file_list, fetch_text,
    files_sync_enabled, serve_fetch, text_sync_enabled,
};
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::sas::PublicKey;
use openlogi_flow::transport::{FlowConnection, IncomingRpc, message_envelope};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

mod files;

/// How long fetching a peer's text, or its file list, may take before it is
/// given up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The OS clipboard as clipboard sync uses it; the seam the tests fake.
pub(super) trait ClipboardBackend: Send + Sync {
    /// Reads the clipboard's text, copying it out only within `max_bytes`.
    fn read_text(&self, max_bytes: usize) -> Result<ClipboardText, ClipboardError>;
    /// Replaces the clipboard's contents with `text`.
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
    /// Whether this backend reads and writes copied files; without, only
    /// text is synced.
    fn files(&self) -> bool;
    /// The files on the clipboard, or `None` when it holds none.
    fn read_files(&self) -> Result<Option<Vec<PathBuf>>, ClipboardError>;
    /// Replaces the clipboard's contents with a copy of `paths`.
    fn write_files(&self, paths: &[PathBuf]) -> Result<(), ClipboardError>;
    /// A number that changes whenever the clipboard does, where the OS keeps
    /// one: a fetch that took long must not replace what was copied since.
    fn change_count(&self) -> Option<u64>;
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

    fn files(&self) -> bool {
        openlogi_clipboard::available()
    }

    fn read_files(&self) -> Result<Option<Vec<PathBuf>>, ClipboardError> {
        openlogi_clipboard::read_files()
    }

    fn write_files(&self, paths: &[PathBuf]) -> Result<(), ClipboardError> {
        openlogi_clipboard::write_files(paths)
    }

    fn change_count(&self) -> Option<u64> {
        openlogi_clipboard::change_count()
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
    /// Where files fetched from peers go, one folder per transfer.
    received: PathBuf,
    ledger: Mutex<Ledger>,
}

impl Shared {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

enum Job {
    /// Announce this clipboard to a peer that accepted a handoff.
    Offer {
        peer: PublicKey,
        connection: Arc<FlowConnection>,
    },
    /// Fetch the text a peer announced and put it on this clipboard.
    FetchText {
        peer: PublicKey,
        connection: Arc<FlowConnection>,
        announced: AnnouncedText,
        ticket: u64,
    },
    /// Fetch the files a peer announced and put a copy of them on this
    /// clipboard.
    FetchFiles {
        peer: PublicKey,
        connection: Arc<FlowConnection>,
        announced: AnnouncedFiles,
        ticket: u64,
    },
}

impl ClipboardSync {
    /// Sync through this machine's clipboard: on when `enabled` (the
    /// `[flow] clipboard` setting) and the OS has a clipboard backend.
    pub(super) fn system(enabled: bool) -> Self {
        if !enabled || !openlogi_clipboard::available() {
            return Self { worker: None };
        }
        let received = openlogi_core::paths::data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("openlogi"))
            .join(files::RECEIVED_DIR);
        Self::with_backend(Arc::new(SystemClipboard), received)
    }

    /// Sync through `backend`, putting received files under `received`.
    /// Spawns the worker, so it needs a runtime.
    fn with_backend(backend: Arc<dyn ClipboardBackend>, received: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            backend,
            received,
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
        let Some(worker) = &self.worker else {
            return Vec::new();
        };
        let mut capabilities = vec![proto::Capability::ClipboardText];
        if worker.shared.backend.files() {
            capabilities.push(proto::Capability::ClipboardFiles);
        }
        capabilities
    }

    /// Offers this clipboard to `peer`, which just accepted a handoff. Does
    /// nothing unless both ends negotiated clipboard sync.
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
    /// Answers the peer's `ClipboardFetch` or `FileFetch` from what is
    /// offered now, on a task of its own.
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
                Ok(Served::Text { sequence }) => {
                    if let Some(shared) = &shared {
                        shared.ledger().served(peer, sequence);
                    }
                    debug!(sequence, "Flow clipboard text sent");
                }
                Ok(Served::Files { sequence }) => debug!(sequence, "Flow clipboard file sent"),
                Ok(Served::Refused) => {
                    debug!("Flow clipboard fetch for something no longer offered refused");
                }
                Err(error) => debug!(%error, "Flow clipboard fetch could not be answered"),
            }
        });
    }

    /// Takes the peer's `ClipboardAnnounce`: the newest one supersedes every
    /// fetch still pending, and text or files within the limits are fetched.
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
        let peer = self.peer;
        let connection = Arc::clone(&self.connection);
        let job = match Announced::from_announce(&announce) {
            Announced::Text(announced) => Job::FetchText {
                peer,
                connection,
                announced,
                ticket,
            },
            Announced::Files(announced) => Job::FetchFiles {
                peer,
                connection,
                announced,
                ticket,
            },
            Announced::TooLarge => {
                debug!("Flow clipboard content over the sync limit skipped");
                return;
            }
            Announced::Nothing => return,
        };
        let _ = worker.jobs.send(job);
    }
}

async fn run_jobs(shared: Arc<Shared>, mut queue: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = queue.recv().await {
        match job {
            Job::Offer { peer, connection } => offer_clipboard(&shared, peer, &connection).await,
            Job::FetchText {
                peer,
                connection,
                announced,
                ticket,
            } => receive_text(&shared, peer, &connection, &announced, ticket).await,
            Job::FetchFiles {
                peer,
                connection,
                announced,
                ticket,
            } => receive_files(&shared, peer, &connection, &announced, ticket).await,
        }
    }
}

/// What this clipboard holds, as clipboard sync can offer it.
enum Local {
    Text(String),
    Files(Vec<OfferedFile>),
}

async fn offer_clipboard(shared: &Arc<Shared>, peer: PublicKey, connection: &FlowConnection) {
    let Some(local) = read_local(shared, true).await else {
        return;
    };
    if matches!(local, Local::Files(_)) && !files_sync_enabled(connection) {
        info!(
            "Flow clipboard files not offered: the other computer's OpenLogi does not take files"
        );
        return;
    }
    let Some(offer) = shared.ledger().offer(peer, local) else {
        debug!("Flow clipboard already shared with this peer");
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
        Ok(()) => match &*offer {
            ClipboardOffer::Text(text) => debug!(
                sequence = text.sequence(),
                bytes = text.size_bytes(),
                "Flow clipboard text offered"
            ),
            ClipboardOffer::Files(files) => info!(
                sequence = files.sequence(),
                files = files.file_count(),
                bytes = files.total_bytes(),
                "Flow clipboard files offered"
            ),
        },
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
    let held = match read_local(shared, false).await {
        Some(Local::Text(text)) => Some(ContentDigest::of(&text)),
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
    let digest = ContentDigest::of(&text);
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

async fn receive_files(
    shared: &Arc<Shared>,
    peer: PublicKey,
    connection: &FlowConnection,
    announced: &AnnouncedFiles,
    ticket: u64,
) {
    if !shared.ledger().is_newest(ticket)
        || !shared.backend.files()
        || !files_sync_enabled(connection)
    {
        return;
    }
    let copied_before = shared.backend.change_count();
    if let Some(Local::Files(held)) = read_local(shared, true).await
        && let Ok(held) = FilesOffer::new(0, held)
        && announced.digest() == Some(held.digest())
    {
        // The same files are on this clipboard already.
        shared.ledger().adopted(peer, held.digest());
        return;
    }
    let listed =
        match tokio::time::timeout(FETCH_TIMEOUT, fetch_file_list(connection, announced)).await {
            Ok(Ok(listed)) => listed,
            Ok(Err(error)) => {
                warn!(%error, "Flow clipboard file list fetch failed");
                return;
            }
            Err(_) => {
                warn!("Flow clipboard file list fetch timed out");
                return;
            }
        };
    info!(
        files = listed.len(),
        bytes = listed.iter().map(ListedFile::size_bytes).sum::<u64>(),
        "Flow clipboard files incoming"
    );
    let still_wanted = || shared.ledger().is_newest(ticket);
    let received = match files::receive(
        connection,
        announced.sequence(),
        &listed,
        &shared.received,
        &still_wanted,
    )
    .await
    {
        Ok(received) => received,
        Err(error) => {
            warn!(%error, "Flow clipboard files fetch failed");
            return;
        }
    };
    if !still_wanted() || shared.backend.change_count() != copied_before {
        info!("Flow clipboard files superseded while they were fetched");
        files::discard(received.folder());
        return;
    }
    let digest = announced.digest().or_else(|| received.digest());
    let paths = received.top_level();
    let backend = Arc::clone(&shared.backend);
    match tokio::task::spawn_blocking(move || backend.write_files(&paths)).await {
        Ok(Ok(())) => {
            if let Some(digest) = digest {
                shared.ledger().adopted(peer, digest);
            }
            info!(
                files = listed.len(),
                bytes = received.total_bytes(),
                "Flow clipboard files received"
            );
            let root = shared.received.clone();
            tokio::task::spawn_blocking(move || files::prune(&root));
        }
        Ok(Err(error)) => {
            warn!(%error, "Flow could not place the received clipboard files");
            files::discard(received.folder());
        }
        Err(error) => warn!(%error, "Flow clipboard write task failed"),
    }
}

/// This clipboard's content, files first when `files` sync is on, or `None`
/// when there is nothing to offer or it cannot be read.
async fn read_local(shared: &Arc<Shared>, files: bool) -> Option<Local> {
    let backend = Arc::clone(&shared.backend);
    let files = files && backend.files();
    let read = tokio::task::spawn_blocking(move || -> Result<Option<Local>, ClipboardError> {
        if files && let Some(paths) = backend.read_files()? {
            return Ok(match files::collect(&paths) {
                Ok(files) => Some(Local::Files(files)),
                Err(error) => {
                    info!(%error, "Flow clipboard files not offered");
                    None
                }
            });
        }
        Ok(match backend.read_text(MAX_TEXT_BYTES)? {
            ClipboardText::Text(text) => Some(Local::Text(text)),
            ClipboardText::TooLarge => {
                debug!("Flow clipboard text over the sync limit not offered");
                None
            }
            ClipboardText::Absent => None,
        })
    })
    .await;
    match read {
        Ok(Ok(local)) => local,
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
    offer: Option<Arc<ClipboardOffer>>,
    /// The content of the last sync either way: the last offer a peer
    /// fetched, or the last content taken from a peer. Clipboard content that
    /// differs was copied here since.
    last_synced: Option<ContentDigest>,
    /// The content each peer is known to hold, from the same two events.
    peer_holds: HashMap<PublicKey, ContentDigest>,
    /// Arrival order of the newest peer announce.
    newest: u64,
}

impl Ledger {
    /// The generation to announce to `peer` for `local`, just read off this
    /// clipboard; `None` when there is nothing to send or the peer gains
    /// nothing from it.
    ///
    /// That is only when nothing was copied here since the last sync and the
    /// peer is known to hold the content. Resending it otherwise would put
    /// stale content back over whatever the peer's user copied since — a
    /// screenshot, say, which this sync cannot carry.
    fn offer(&mut self, peer: PublicKey, local: Local) -> Option<Arc<ClipboardOffer>> {
        let sequence = self.sequence + 1;
        let offer = match local {
            Local::Text(text) => ClipboardOffer::Text(TextOffer::new(sequence, text).ok()?),
            Local::Files(files) => ClipboardOffer::Files(FilesOffer::new(sequence, files).ok()?),
        };
        let digest = offer.digest();
        if self.last_synced == Some(digest) && self.peer_holds.get(&peer) == Some(&digest) {
            return None;
        }
        let offer = Arc::new(offer);
        self.sequence = sequence;
        self.offer = Some(Arc::clone(&offer));
        Some(offer)
    }

    fn current_offer(&self) -> Option<Arc<ClipboardOffer>> {
        self.offer.clone()
    }

    /// `peer` fetched generation `sequence`'s text in full.
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

    /// Content `peer` announced is on this clipboard now, written or already
    /// there.
    fn adopted(&mut self, peer: PublicKey, digest: ContentDigest) {
        self.synced(peer, digest);
    }

    fn synced(&mut self, peer: PublicKey, digest: ContentDigest) {
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

/// The folder files from one transfer were put in, and what is in it.
pub(super) struct ReceivedFiles {
    folder: PathBuf,
    top_level: Vec<PathBuf>,
    total_bytes: u64,
    offered: Vec<OfferedFile>,
}

impl ReceivedFiles {
    fn folder(&self) -> &Path {
        &self.folder
    }

    /// The paths a file copy of the transfer names: its top-level files and
    /// folders, in the order the peer listed them.
    fn top_level(&self) -> Vec<PathBuf> {
        self.top_level.clone()
    }

    const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The digest these files have when this machine offers them on.
    fn digest(&self) -> Option<ContentDigest> {
        FilesOffer::new(0, self.offered.clone())
            .ok()
            .map(|offer| offer.digest())
    }
}

#[cfg(test)]
mod tests;
