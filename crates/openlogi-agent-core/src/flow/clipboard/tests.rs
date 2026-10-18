use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use openlogi_flow::transport::{
    FlowEndpoint, MachineIdentity, NotificationEvent, PeerTrust, RpcEvent,
};

use super::*;

/// How long an expected frame may take to arrive.
const ARRIVAL: Duration = Duration::from_secs(5);
/// How long to wait for a frame that must not come.
const QUIET: Duration = Duration::from_millis(300);

/// An in-memory clipboard, holding text or file paths, that counts writes.
#[derive(Default)]
struct FakeClipboard {
    text: Mutex<Option<String>>,
    files: Mutex<Option<Vec<PathBuf>>>,
    writes: AtomicUsize,
}

impl FakeClipboard {
    fn holding(text: &str) -> Arc<Self> {
        let clipboard = Self::default();
        *clipboard.text.lock().unwrap() = Some(text.to_owned());
        Arc::new(clipboard)
    }

    fn holding_files(paths: Vec<PathBuf>) -> Arc<Self> {
        let clipboard = Self::default();
        *clipboard.files.lock().unwrap() = Some(paths);
        Arc::new(clipboard)
    }

    fn text(&self) -> Option<String> {
        self.text.lock().unwrap().clone()
    }

    fn files(&self) -> Option<Vec<PathBuf>> {
        self.files.lock().unwrap().clone()
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }
}

impl ClipboardBackend for FakeClipboard {
    fn read_text(&self, max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
        Ok(match self.text() {
            None => ClipboardText::Absent,
            Some(text) if text.is_empty() => ClipboardText::Absent,
            Some(text) if text.len() > max_bytes => ClipboardText::TooLarge,
            Some(text) => ClipboardText::Text(text),
        })
    }

    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        *self.text.lock().unwrap() = Some(text.to_owned());
        *self.files.lock().unwrap() = None;
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn files(&self) -> bool {
        true
    }

    fn read_files(&self) -> Result<Option<Vec<PathBuf>>, ClipboardError> {
        Ok(self.files())
    }

    fn write_files(&self, paths: &[PathBuf]) -> Result<(), ClipboardError> {
        *self.files.lock().unwrap() = Some(paths.to_vec());
        *self.text.lock().unwrap() = None;
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn change_count(&self) -> Option<u64> {
        u64::try_from(self.writes()).ok()
    }
}

/// Sync through `clipboard`, putting received files under `received`.
fn sync(clipboard: Arc<FakeClipboard>, received: &Path) -> ClipboardSync {
    ClipboardSync::with_backend(clipboard, received.to_owned())
}

/// Sync through `clipboard` for a test that receives no files.
fn text_sync(clipboard: Arc<FakeClipboard>) -> ClipboardSync {
    sync(clipboard, Path::new("no-files-expected"))
}

fn peer(byte: u8) -> PublicKey {
    PublicKey::new([byte; 32])
}

#[test]
fn every_offer_is_a_new_generation() {
    let mut ledger = Ledger::default();
    let first = ledger
        .offer(peer(1), Local::Text("one".to_owned()))
        .unwrap();
    let second = ledger
        .offer(peer(1), Local::Text("two".to_owned()))
        .unwrap();

    assert_eq!(first.sequence(), 1);
    assert_eq!(second.sequence(), 2);
    assert_eq!(ledger.current_offer(peer(1)).unwrap().sequence(), 2);
}

#[test]
fn an_offer_is_fetched_only_by_the_peer_it_was_announced_to() {
    let mut ledger = Ledger::default();
    let offer = ledger
        .offer(peer(1), Local::Text("for one".to_owned()))
        .unwrap();

    assert!(ledger.current_offer(peer(2)).is_none());
    // Nor does a fetch by another peer count as sharing it.
    ledger.served(peer(2), offer.sequence());
    assert!(
        ledger
            .offer(peer(2), Local::Text("for one".to_owned()))
            .is_some()
    );
}

#[test]
fn text_a_peer_already_holds_is_not_offered_again() {
    let mut ledger = Ledger::default();
    let offer = ledger
        .offer(peer(1), Local::Text("copied".to_owned()))
        .unwrap();
    ledger.served(peer(1), offer.sequence());

    assert!(
        ledger
            .offer(peer(1), Local::Text("copied".to_owned()))
            .is_none()
    );
    // A third machine has not seen it: the text goes on with the pointer.
    assert!(
        ledger
            .offer(peer(2), Local::Text("copied".to_owned()))
            .is_some()
    );
}

#[test]
fn text_taken_from_a_peer_is_not_sent_back() {
    let mut ledger = Ledger::default();
    ledger.adopted(peer(1), ContentDigest::of("from the peer"));

    assert!(
        ledger
            .offer(peer(1), Local::Text("from the peer".to_owned()))
            .is_none()
    );
}

#[test]
fn text_copied_here_after_a_sync_is_offered_even_to_a_peer_that_held_it() {
    let mut ledger = Ledger::default();
    let offer = ledger
        .offer(peer(1), Local::Text("first".to_owned()))
        .unwrap();
    ledger.served(peer(1), offer.sequence());
    // Another peer's text arrived, then the user copied "first" again.
    ledger.adopted(peer(2), ContentDigest::of("from elsewhere"));

    assert!(
        ledger
            .offer(peer(1), Local::Text("first".to_owned()))
            .is_some()
    );
}

#[test]
fn only_a_complete_fetch_of_the_current_offer_counts_as_shared() {
    let mut ledger = Ledger::default();
    let stale = ledger
        .offer(peer(1), Local::Text("older".to_owned()))
        .unwrap();
    ledger
        .offer(peer(1), Local::Text("newer".to_owned()))
        .unwrap();
    ledger.served(peer(1), stale.sequence());

    assert!(
        ledger
            .offer(peer(1), Local::Text("older".to_owned()))
            .is_some()
    );
}

#[test]
fn empty_text_is_never_offered() {
    let mut ledger = Ledger::default();
    assert!(ledger.offer(peer(1), Local::Text(String::new())).is_none());
    assert!(ledger.current_offer(peer(1)).is_none());
}

#[test]
fn a_newer_announce_supersedes_every_older_fetch() {
    let mut ledger = Ledger::default();
    let first = ledger.announced();
    let second = ledger.announced();

    assert!(!ledger.is_newest(first));
    assert!(ledger.is_newest(second));
}

#[tokio::test]
async fn clipboard_sync_is_advertised_only_when_it_runs() {
    assert!(ClipboardSync::system(false).capabilities().is_empty());
    assert_eq!(
        ClipboardSync::system(true).capabilities().is_empty(),
        !openlogi_clipboard::available()
    );
    assert_eq!(
        text_sync(Arc::new(FakeClipboard::default())).capabilities(),
        [
            proto::Capability::ClipboardText,
            proto::Capability::ClipboardFiles
        ]
    );
}

/// Two machines on one loopback QUIC link, each with its own clipboard.
struct Machines {
    _endpoints: [Arc<FlowEndpoint>; 2],
    sender_key: PublicKey,
    receiver_key: PublicKey,
    sender_connection: Arc<FlowConnection>,
    receiver_connection: Arc<FlowConnection>,
    sender: ClipboardSync,
    receiver: ClipboardSync,
}

impl Machines {
    /// Connects a sender and a receiver whose `Hello`s list what their
    /// clipboard syncs advertise.
    async fn connect(sender: ClipboardSync, receiver: ClipboardSync) -> Self {
        let receiver_capabilities = receiver.capabilities();
        Self::connect_listing(sender, receiver, receiver_capabilities).await
    }

    /// Connects them with the receiver's `Hello` listing
    /// `receiver_capabilities`, as a peer predating clipboard sync lists none.
    async fn connect_listing(
        sender: ClipboardSync,
        receiver: ClipboardSync,
        receiver_capabilities: Vec<proto::Capability>,
    ) -> Self {
        let sender_identity = MachineIdentity::generate().unwrap();
        let receiver_identity = MachineIdentity::generate().unwrap();
        let sender_key = sender_identity.public_key();
        let receiver_key = receiver_identity.public_key();
        let sender_endpoint = Arc::new(
            FlowEndpoint::bind(
                localhost(),
                sender_identity.clone(),
                PeerTrust::pinned([receiver_key]),
                hello(&sender_identity, sender.capabilities()),
            )
            .unwrap(),
        );
        let receiver_endpoint = Arc::new(
            FlowEndpoint::bind(
                localhost(),
                receiver_identity.clone(),
                PeerTrust::pinned([sender_key]),
                hello(&receiver_identity, receiver_capabilities),
            )
            .unwrap(),
        );
        let acceptor = Arc::clone(&receiver_endpoint);
        let accepted = tokio::spawn(async move { acceptor.accept().await });
        let sender_connection = sender_endpoint
            .connect(receiver_endpoint.local_addr().unwrap())
            .await
            .unwrap();
        let receiver_connection = accepted.await.unwrap().unwrap();
        Self {
            _endpoints: [sender_endpoint, receiver_endpoint],
            sender_key,
            receiver_key,
            sender_connection: Arc::new(sender_connection),
            receiver_connection: Arc::new(receiver_connection),
            sender,
            receiver,
        }
    }

    /// The sender offers its clipboard, as after an accepted handoff, and the
    /// receiver's connection loop takes the announce, as the runtime's does.
    /// Returns whether an announce arrived within `wait`.
    async fn offer(&self, wait: Duration) -> bool {
        self.sender
            .offer(self.receiver_key, &self.sender_connection);
        let Ok(announce) =
            tokio::time::timeout(wait, self.receiver_connection.accept_notification()).await
        else {
            return false;
        };
        let NotificationEvent::Notification(announce) = announce.unwrap() else {
            panic!("the announce was dropped");
        };
        self.receiver
            .connection(self.sender_key, Arc::clone(&self.receiver_connection))
            .announced(&announce);
        true
    }

    /// The sender's connection loop answers every fetch from now on, as the
    /// runtime's does: a file transfer takes one per file and one more for
    /// the list.
    fn serve_every_fetch(&self) {
        let connection = Arc::clone(&self.sender_connection);
        let clipboard = self
            .sender
            .connection(self.receiver_key, Arc::clone(&connection));
        tokio::spawn(async move {
            while let Ok(RpcEvent::Request(fetch)) = connection.accept_rpc().await {
                clipboard.serve(fetch);
            }
        });
    }

    /// The sender's connection loop answers one fetch. Returns whether the
    /// receiver fetched within `wait`.
    async fn serve_fetch(&self, wait: Duration) -> bool {
        let Ok(fetch) = tokio::time::timeout(wait, self.sender_connection.accept_rpc()).await
        else {
            return false;
        };
        let RpcEvent::Request(fetch) = fetch.unwrap() else {
            panic!("the fetch was rejected");
        };
        self.sender
            .connection(self.receiver_key, Arc::clone(&self.sender_connection))
            .serve(fetch);
        true
    }
}

fn localhost() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

fn hello(identity: &MachineIdentity, capabilities: Vec<proto::Capability>) -> proto::Hello {
    proto::Hello {
        proto_min: 1,
        proto_max: 1,
        public_key: identity.public_key().as_bytes().to_vec(),
        session_nonce: vec![7; 16],
        capabilities: capabilities.into_iter().map(Into::into).collect(),
        ..Default::default()
    }
}

/// Waits for `clipboard` to hold `expected`.
async fn wait_for_text(clipboard: &FakeClipboard, expected: &str) {
    tokio::time::timeout(ARRIVAL, async {
        while clipboard.text().as_deref() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the receiver's clipboard takes the sender's text");
}

#[tokio::test]
async fn text_copied_on_the_sender_lands_on_the_receivers_clipboard() {
    let sender_clipboard = FakeClipboard::holding("copied on machine A");
    let receiver_clipboard = FakeClipboard::holding("older text on machine B");
    let machines = Machines::connect(
        text_sync(sender_clipboard.clone()),
        text_sync(receiver_clipboard.clone()),
    )
    .await;

    assert!(
        machines.offer(ARRIVAL).await,
        "the sender announces its text"
    );
    assert!(
        machines.serve_fetch(ARRIVAL).await,
        "the receiver fetches it"
    );

    wait_for_text(&receiver_clipboard, "copied on machine A").await;
    assert_eq!(receiver_clipboard.writes(), 1);
    assert_eq!(sender_clipboard.writes(), 0);
}

#[tokio::test]
async fn text_copied_on_the_receiver_during_the_fetch_is_kept() {
    let receiver_clipboard = FakeClipboard::holding("older text on machine B");
    let machines = Machines::connect(
        text_sync(FakeClipboard::holding("copied on machine A")),
        text_sync(receiver_clipboard.clone()),
    )
    .await;

    assert!(
        machines.offer(ARRIVAL).await,
        "the sender announces its text"
    );
    let RpcEvent::Request(fetch) =
        tokio::time::timeout(ARRIVAL, machines.sender_connection.accept_rpc())
            .await
            .expect("the receiver fetches the text")
            .unwrap()
    else {
        panic!("the fetch was rejected");
    };
    // The user copies on machine B while the fetch is on the wire.
    receiver_clipboard
        .write_text("copied on machine B")
        .unwrap();
    machines
        .sender
        .connection(
            machines.receiver_key,
            Arc::clone(&machines.sender_connection),
        )
        .serve(fetch);

    tokio::time::sleep(QUIET).await;
    assert_eq!(
        receiver_clipboard.text().as_deref(),
        Some("copied on machine B")
    );
    assert_eq!(receiver_clipboard.writes(), 1, "only the user's copy");
}

#[tokio::test]
async fn text_the_receiver_already_holds_is_neither_fetched_nor_written() {
    let receiver_clipboard = FakeClipboard::holding("same on both");
    let machines = Machines::connect(
        text_sync(FakeClipboard::holding("same on both")),
        text_sync(receiver_clipboard.clone()),
    )
    .await;

    assert!(
        machines.offer(ARRIVAL).await,
        "the sender announces its text"
    );
    assert!(
        !machines.serve_fetch(QUIET).await,
        "a receiver holding the announced text does not fetch it"
    );
    assert_eq!(receiver_clipboard.writes(), 0);
}

#[tokio::test]
async fn nothing_crosses_to_a_peer_that_does_not_list_clipboard_sync() {
    let receiver_clipboard = FakeClipboard::holding("kept");
    let machines = Machines::connect_listing(
        text_sync(FakeClipboard::holding("copied")),
        text_sync(receiver_clipboard.clone()),
        Vec::new(),
    )
    .await;

    assert!(
        !machines.offer(QUIET).await,
        "no announce may reach a peer that did not list clipboard sync"
    );
    assert_eq!(receiver_clipboard.text().as_deref(), Some("kept"));
    assert_eq!(receiver_clipboard.writes(), 0);
}

#[tokio::test]
async fn text_over_the_limit_is_skipped_without_an_announce() {
    let receiver_clipboard = FakeClipboard::holding("kept");
    let machines = Machines::connect(
        text_sync(FakeClipboard::holding(&"x".repeat(MAX_TEXT_BYTES + 1))),
        text_sync(receiver_clipboard.clone()),
    )
    .await;

    assert!(
        !machines.offer(QUIET).await,
        "text over the limit is not announced"
    );
    assert_eq!(receiver_clipboard.writes(), 0);
}

/// Writes `bytes` to `path`, making its folders, and stamps it with
/// `modified` so copies of it can match.
fn write_file(path: &Path, bytes: &[u8], modified: SystemTime) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// Copied files: a file and a folder holding a nested folder.
fn copy_some_files(root: &Path) -> Vec<PathBuf> {
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    write_file(&root.join("report.txt"), b"quarterly numbers", modified);
    write_file(
        &root.join("photos/2024/beach.jpg"),
        &vec![7; 150_000],
        modified,
    );
    write_file(&root.join("photos/cover.png"), b"png", modified);
    vec![root.join("report.txt"), root.join("photos")]
}

/// Waits for `clipboard` to hold files.
async fn wait_for_files(clipboard: &FakeClipboard) -> Vec<PathBuf> {
    tokio::time::timeout(ARRIVAL, async {
        loop {
            if let Some(files) = clipboard.files() {
                return files;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the receiver's clipboard takes the sender's files")
}

#[tokio::test]
async fn files_copied_on_the_sender_land_on_the_receivers_clipboard() {
    let copied = tempfile::tempdir().unwrap();
    let received = tempfile::tempdir().unwrap();
    let sender_clipboard = FakeClipboard::holding_files(copy_some_files(copied.path()));
    let receiver_clipboard = FakeClipboard::holding("older text on machine B");
    let machines = Machines::connect(
        text_sync(sender_clipboard),
        sync(receiver_clipboard.clone(), received.path()),
    )
    .await;
    machines.serve_every_fetch();

    assert!(
        machines.offer(ARRIVAL).await,
        "the sender announces its files"
    );
    let pasted = wait_for_files(&receiver_clipboard).await;

    assert_eq!(pasted.len(), 2, "the two copied items, in their order");
    assert_eq!(pasted[0].file_name().unwrap(), "report.txt");
    assert_eq!(pasted[1].file_name().unwrap(), "photos");
    assert!(pasted.iter().all(|path| path.starts_with(received.path())));
    assert_eq!(fs::read(&pasted[0]).unwrap(), b"quarterly numbers");
    assert_eq!(
        fs::read(pasted[1].join("2024").join("beach.jpg")).unwrap(),
        vec![7; 150_000]
    );
    assert_eq!(fs::read(pasted[1].join("cover.png")).unwrap(), b"png");
    assert_eq!(
        fs::metadata(&pasted[0]).unwrap().modified().unwrap(),
        fs::metadata(copied.path().join("report.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        "a received file keeps its modification time"
    );
    assert_eq!(receiver_clipboard.writes(), 1);
}

#[tokio::test]
async fn files_that_only_match_by_name_size_and_time_are_fetched() {
    let here = tempfile::tempdir().unwrap();
    let there = tempfile::tempdir().unwrap();
    let received = tempfile::tempdir().unwrap();
    let held = copy_some_files(there.path());
    // Same name, size and modification time, other bytes.
    write_file(
        &there.path().join("report.txt"),
        b"QUARTERLY NUMBERS",
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    );
    let receiver_clipboard = FakeClipboard::holding_files(held);
    let machines = Machines::connect(
        text_sync(FakeClipboard::holding_files(copy_some_files(here.path()))),
        sync(receiver_clipboard.clone(), received.path()),
    )
    .await;
    machines.serve_every_fetch();

    assert!(
        machines.offer(ARRIVAL).await,
        "the sender announces its files"
    );
    tokio::time::timeout(ARRIVAL, async {
        while receiver_clipboard.writes() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the receiver fetches files it cannot prove it holds");
    let pasted = receiver_clipboard.files().unwrap();
    assert_eq!(fs::read(&pasted[0]).unwrap(), b"quarterly numbers");
}

#[tokio::test]
async fn files_go_only_to_a_peer_that_lists_file_sync() {
    let copied = tempfile::tempdir().unwrap();
    let receiver_clipboard = FakeClipboard::holding("kept");
    let machines = Machines::connect_listing(
        text_sync(FakeClipboard::holding_files(copy_some_files(copied.path()))),
        text_sync(receiver_clipboard.clone()),
        vec![proto::Capability::ClipboardText],
    )
    .await;

    assert!(
        !machines.offer(QUIET).await,
        "files are not announced to a peer that syncs text only"
    );
    assert_eq!(receiver_clipboard.text().as_deref(), Some("kept"));
}
