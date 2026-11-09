//! Pairing another computer for Flow, driven by the GUI over IPC.
//!
//! While the user has pairing open, the agent runs a pairing endpoint of its
//! own beside the Flow runtime's. It trusts no key, so it carries only the
//! pairing ceremony, and the runtime's endpoint, which trusts only pinned
//! keys, never sees an unknown peer. The pairing endpoint advertises itself
//! under the pairing mDNS service, lists the other computers that do, and
//! runs one [`PairingSession`] at a time, either side starting it.
//!
//! The session saves the peer through [`store::ConfigPeerStore`] only once
//! both users confirmed the code, and the agent then reloads its config:
//! the new pin reaches the Flow runtime the way a hand edit would. The
//! pairing connection itself closes; the runtime reaches the peer on its own
//! endpoint.
//!
//! One task owns everything while pairing is open. Network reads, calls, and
//! dials run in tasks of the current attempt and report back through one
//! queue, tagged with the attempt they belong to, so work of an attempt that
//! ended is ignored. The confirmation step that saves the peer runs on a
//! blocking thread, and the task waits for it.

mod stage;
mod store;
#[cfg(test)]
mod tests;

use std::io;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::{FlowPeerName, FlowPublicKey};
use openlogi_flow::discovery::{
    CandidateSource as _, ManualCandidateSource, MdnsAdvertiser, MdnsRecord, PAIRING_PORT,
    PairingAdvert, PairingBrowser, PairingCandidate,
};
use openlogi_flow::frame::{Envelope, FrameKind, InboundRole};
use openlogi_flow::generated as proto;
use openlogi_flow::pairing::{
    PairingAbortReason, PairingError, PairingSession, PairingState, PersistPeerKeyError,
};
use openlogi_flow::sas::{PairingNonce, PublicKey};
use openlogi_flow::transport::{
    FlowConnection, FlowEndpoint, IncomingConnection, IncomingRpc, MachineIdentity,
    NotificationEvent, PeerTrust, RpcEvent, TransportError, error_envelope, message_envelope,
};
use openlogi_ipc::{
    FlowIdentity, FlowPairTarget, FlowPairingCommandError, FlowPairingFailure, FlowPairingPeer,
    FlowPairingPhase, FlowPlatform,
};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};

use super::identity;
use super::runtime::{PROTOCOL_MAX, PROTOCOL_MIN, bind, hello, machine_name, platform};
use crate::observable::ObservableState;
use stage::{Deadline, Ending, Stage};
use store::ConfigPeerStore;
pub use store::{PairedPeer, SavePeerError, save_peer};

/// How long pairing stays open without an attempt before it closes itself.
const OPEN_FOR: Duration = Duration::from_mins(10);
/// How long a pairing connection may take from the dial until the code
/// shows.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
/// Deadline for the answer to one pairing request.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an ended attempt keeps its connection open so that its last
/// frames reach the peer, unless the peer closes it first.
const LINGER: Duration = Duration::from_secs(2);
/// Streams one pairing connection may have in flight before the next accept
/// waits.
const MAX_STREAM_READS: usize = 8;
const COMMAND_QUEUE: usize = 8;
const EVENT_QUEUE: usize = 32;

/// Flow pairing for the agent's lifetime. Cloning shares it.
#[derive(Clone)]
pub struct FlowPairing {
    inner: Arc<Inner>,
}

struct Inner {
    setup: Setup,
    observable: Arc<ObservableState>,
    /// Counts the peers saved, so the agent reloads its config after each.
    saved: watch::Sender<u64>,
    /// The session pairing runs while it is open. One that closed itself
    /// stays until pairing opens again or stops.
    open: Mutex<Option<Open>>,
}

/// A pairing session: where its commands go and the task running it. Kept
/// under one lock, so stopping waits for the session it stopped and opening
/// binds the pairing port only once the last session freed it.
struct Open {
    commands: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}

/// Where and as whom pairing runs. Production binds the pairing port and
/// uses mDNS; tests bind loopback without it.
#[derive(Clone)]
struct Setup {
    address: SocketAddr,
    mdns: bool,
    /// `None` loads, or creates, the machine identity file.
    identity: Option<MachineIdentity>,
    /// `None` saves peers to the user's config file.
    config: Option<PathBuf>,
    /// How long a code this side answers with stays valid; `None` for the
    /// protocol default.
    code_timeout: Option<Duration>,
}

type Reply = oneshot::Sender<Result<(), FlowPairingCommandError>>;

enum Command {
    Renew(Reply),
    Pair(FlowPairTarget, Reply),
    Confirm(Reply),
    Cancel(Reply),
    Stop,
}

impl FlowPairing {
    /// Pairing as the agent runs it, closed until [`Self::start`].
    pub(crate) fn new(observable: Arc<ObservableState>) -> Self {
        Self::with_setup(
            observable,
            Setup {
                address: SocketAddr::from((Ipv6Addr::UNSPECIFIED, PAIRING_PORT)),
                mdns: true,
                identity: None,
                config: None,
                code_timeout: None,
            },
        )
    }

    fn with_setup(observable: Arc<ObservableState>, setup: Setup) -> Self {
        Self {
            inner: Arc::new(Inner {
                setup,
                observable,
                saved: watch::channel(0).0,
                open: Mutex::new(None),
            }),
        }
    }

    /// Changes each time pairing saves a peer in `config.toml`.
    #[must_use]
    pub fn saved(&self) -> watch::Receiver<u64> {
        self.inner.saved.subscribe()
    }

    /// This computer's Flow identity, creating the machine key on first use.
    pub async fn identity(&self) -> Result<FlowIdentity, FlowPairingCommandError> {
        let identity = self.load_identity().await?;
        Ok(FlowIdentity {
            machine_name: machine_name(),
            public_key: flow_key(identity.public_key()),
        })
    }

    /// Open pairing, or keep it open when it already is.
    pub async fn start(&self) -> Result<(), FlowPairingCommandError> {
        let mut open = self.inner.open.lock().await;
        if let Some(running) = open.as_ref()
            && ask(&running.commands, Command::Renew).await.is_ok()
        {
            return Ok(());
        }
        if let Some(ended) = open.take() {
            // It closed itself; wait until it has freed the pairing port.
            let _ = ended.task.await;
        }
        let identity = self.load_identity().await?;
        let (session, commands) = Session::open(Arc::clone(&self.inner), identity)?;
        // Published before this returns, so the caller's next snapshot shows
        // pairing open.
        session.publish();
        *open = Some(Open {
            commands,
            task: tokio::spawn(session.run()),
        });
        info!("Flow pairing opened");
        Ok(())
    }

    /// Pair with `target`.
    pub async fn pair(&self, target: FlowPairTarget) -> Result<(), FlowPairingCommandError> {
        self.ask(|reply| Command::Pair(target, reply)).await
    }

    /// The user confirmed that both computers show the same code.
    pub async fn confirm(&self) -> Result<(), FlowPairingCommandError> {
        self.ask(Command::Confirm).await
    }

    /// End the current attempt, or dismiss its result.
    pub async fn cancel(&self) -> Result<(), FlowPairingCommandError> {
        self.ask(Command::Cancel).await
    }

    /// Close pairing, cancelling an attempt in progress, and wait until it
    /// has closed.
    pub async fn stop(&self) {
        // Held until the session closed: a `start` meanwhile waits, then
        // opens a new one.
        let mut open = self.inner.open.lock().await;
        let Some(Open { commands, task }) = open.take() else {
            return;
        };
        let _ = commands.send(Command::Stop).await;
        if task.await.is_err() {
            warn!("the Flow pairing task panicked");
        }
    }

    async fn ask(
        &self,
        command: impl FnOnce(Reply) -> Command,
    ) -> Result<(), FlowPairingCommandError> {
        let commands = self
            .inner
            .open
            .lock()
            .await
            .as_ref()
            .map(|open| open.commands.clone());
        match commands {
            Some(commands) => ask(&commands, command).await,
            None => Err(FlowPairingCommandError::NotStarted),
        }
    }

    async fn load_identity(&self) -> Result<MachineIdentity, FlowPairingCommandError> {
        if let Some(identity) = &self.inner.setup.identity {
            return Ok(identity.clone());
        }
        match tokio::task::spawn_blocking(identity::load).await {
            Ok(Ok(identity)) => Ok(identity),
            Ok(Err(error)) => Err(unavailable(&error)),
            Err(_) => Err(FlowPairingCommandError::Unavailable {
                message: "the Flow identity task failed".to_owned(),
            }),
        }
    }
}

/// Send `command` and wait for its answer. A session that ended answers
/// [`FlowPairingCommandError::NotStarted`].
async fn ask(
    commands: &mpsc::Sender<Command>,
    command: impl FnOnce(Reply) -> Command,
) -> Result<(), FlowPairingCommandError> {
    let (reply, answer) = oneshot::channel();
    if commands.send(command(reply)).await.is_err() {
        return Err(FlowPairingCommandError::NotStarted);
    }
    answer
        .await
        .unwrap_or(Err(FlowPairingCommandError::NotStarted))
}

fn unavailable(error: &dyn std::error::Error) -> FlowPairingCommandError {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    FlowPairingCommandError::Unavailable { message }
}

/// Pairing while it is open: the endpoint, mDNS, and at most one attempt.
struct Session {
    inner: Arc<Inner>,
    endpoint: Arc<FlowEndpoint>,
    local_key: PublicKey,
    _advertiser: Option<MdnsAdvertiser>,
    browser: Option<PairingBrowser>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Tagged>,
    event_queue: mpsc::Receiver<Tagged>,
    /// The computers found with pairing open, this one left out.
    nearby: Vec<PairingCandidate>,
    attempt: Option<Attempt>,
    next_attempt: u64,
    lifecycle: Lifecycle,
}

/// Whether pairing takes commands.
enum Lifecycle {
    /// Pairing closes itself at `expires` unless an attempt runs. `result`
    /// is the last attempt's result, shown until the user dismisses it or
    /// another attempt shows.
    Open {
        expires: Instant,
        result: Option<FlowPairingPhase>,
    },
    /// Pairing closes once its attempt, ended, has let its last frames go.
    /// It takes no commands, starts no attempt, and shows nothing.
    Stopping,
}

impl Lifecycle {
    /// Keep pairing open for [`OPEN_FOR`] from `now`.
    fn renew(&mut self, now: Instant) {
        if let Self::Open { expires, .. } = self {
            *expires = now + OPEN_FOR;
        }
    }

    /// Show `result` until the user dismisses it.
    fn show(&mut self, result: FlowPairingPhase) {
        if let Self::Open { result: shown, .. } = self {
            *shown = Some(result);
        }
    }

    fn dismiss(&mut self) {
        if let Self::Open { result, .. } = self {
            *result = None;
        }
    }
}

/// An event from a task of attempt `attempt`.
struct Tagged {
    attempt: u64,
    event: Event,
}

enum Event {
    /// The dial or the inbound handshake finished.
    Connected(Result<Arc<FlowConnection>, FlowPairingFailure>),
    /// The peer sent a request.
    Request(IncomingRpc),
    /// The peer sent a notification.
    Notification(Envelope),
    /// One of this side's requests was answered, or failed.
    Answered(Call, Result<Envelope, TransportError>),
    /// The connection closed.
    Closed,
}

/// This side's requests to the peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Start,
    Reveal,
    Confirm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    Initiator,
    Responder,
}

/// One pairing attempt: a dial or an inbound connection, then its ceremony.
struct Attempt {
    id: u64,
    /// The address the user typed, saved with the peer.
    address: Option<String>,
    stage: Stage<Arc<FlowConnection>>,
    /// Dials, reads, requests, and responses. Dropped with the attempt,
    /// which aborts what is still running.
    tasks: JoinSet<()>,
}

/// A connection and the ceremony on it. `C` is the connection,
/// `Arc<FlowConnection>` in the agent.
#[derive(Clone)]
struct Link<C> {
    connection: C,
    session: PairingSession,
}

impl<C> Link<C> {
    /// Run a confirmation `step` of the ceremony on a blocking thread and
    /// keep the session it leaves. The step that sees both users confirmed
    /// saves the peer through `store`, which reads and writes `config.toml`;
    /// the attempt waits for it, so pairing succeeds only once the peer is
    /// saved. Answers the step's result and the name the peer was saved
    /// under, if it was.
    async fn confirm<T: Send + 'static>(
        &mut self,
        mut store: ConfigPeerStore,
        step: impl FnOnce(&mut PairingSession, &mut ConfigPeerStore) -> Result<T, PairingError>
        + Send
        + 'static,
    ) -> (Result<T, PairingError>, Option<FlowPeerName>) {
        let mut session = self.session.clone();
        let stepped = tokio::task::spawn_blocking(move || {
            let result = step(&mut session, &mut store);
            (session, result, store.saved)
        })
        .await;
        match stepped {
            Ok((session, result, saved)) => {
                self.session = session;
                (result, saved)
            }
            Err(error) => {
                let error = PersistPeerKeyError::new(format!("the save task failed: {error}"));
                (Err(PairingError::Persistence(error)), None)
            }
        }
    }
}

/// What ending an attempt does with its connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Close {
    /// The peer ended the attempt or is gone: close at once.
    Now,
    /// Responses this side sent are on their way: let them arrive.
    Flush,
    /// Tell the peer why the ceremony ended, then let that arrive.
    Abort(PairingAbortReason),
}

impl Attempt {
    fn new(id: u64, address: Option<String>, stage: Stage<Arc<FlowConnection>>) -> Self {
        Self {
            id,
            address,
            stage,
            tasks: JoinSet::new(),
        }
    }

    /// Send `request` as `call` and report its answer.
    fn call(
        &mut self,
        events: &mpsc::Sender<Tagged>,
        call: Call,
        request: Result<Envelope, TransportError>,
    ) {
        let Some(live) = self.stage.live() else {
            return;
        };
        let connection = Arc::clone(&live.link.connection);
        if call == Call::Confirm && self.stage.confirm_sent().is_err() {
            return;
        }
        let (attempt, events) = (self.id, events.clone());
        self.tasks.spawn(async move {
            let answer = match request {
                Ok(request) => timeout(CALL_TIMEOUT, connection.call(request))
                    .await
                    .unwrap_or(Err(TransportError::StreamTimeout)),
                Err(error) => Err(error),
            };
            let event = Event::Answered(call, answer);
            let _ = events.send(Tagged { attempt, event }).await;
        });
    }

    /// Answer the peer's request `rpc` with `response`.
    fn respond(&mut self, rpc: IncomingRpc, response: Result<Envelope, TransportError>) {
        self.tasks.spawn(async move {
            let sent = match response {
                Ok(response) => timeout(CALL_TIMEOUT, rpc.respond(response))
                    .await
                    .unwrap_or(Err(TransportError::StreamTimeout)),
                Err(error) => Err(error),
            };
            if let Err(error) = sent {
                debug!(%error, "a Flow pairing response was not sent");
            }
        });
    }

    /// Tell the peer the ceremony ended for `reason`: when this side's
    /// session still ran it, or when the session ended itself for that
    /// reason, as on a commitment mismatch or an expired deadline.
    fn abort(&mut self, reason: PairingAbortReason) {
        let Some(live) = self.stage.live_mut() else {
            return;
        };
        let link = &mut live.link;
        let ended_for_reason = match link.session.state() {
            PairingState::Aborted(ended) => ended == reason,
            PairingState::TimedOut => reason == PairingAbortReason::Timeout,
            _ => false,
        };
        let abort = match link.session.abort(reason) {
            Ok(abort) => abort,
            Err(_) if ended_for_reason => proto::PairAbort {
                reason: proto::PairAbortReason::from(reason).into(),
                ..Default::default()
            },
            Err(_) => return,
        };
        let notification = message_envelope(FrameKind::PairAbort, &abort);
        let connection = Arc::clone(&link.connection);
        self.tasks.spawn(async move {
            let sent = match notification {
                Ok(notification) => timeout(CALL_TIMEOUT, connection.notify(notification))
                    .await
                    .unwrap_or(Err(TransportError::StreamTimeout)),
                Err(error) => Err(error),
            };
            if let Err(error) = sent {
                debug!(%error, "the Flow pairing abort was not sent");
            }
        });
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(connection) = self.stage.connection() {
            connection.close();
        }
    }
}

impl Session {
    /// Bind the pairing endpoint and start mDNS.
    fn open(
        inner: Arc<Inner>,
        identity: MachineIdentity,
    ) -> Result<(Self, mpsc::Sender<Command>), FlowPairingCommandError> {
        let local_key = identity.public_key();
        let endpoint = Arc::new(bind_pairing(inner.setup.address, identity)?);
        let port = endpoint
            .local_addr()
            .map_err(|error| unavailable(&error))?
            .port();
        let (advertiser, browser) = if inner.setup.mdns {
            (advertise(local_key, port), browse())
        } else {
            (None, None)
        };
        let (commands_tx, commands) = mpsc::channel(COMMAND_QUEUE);
        let (events, event_queue) = mpsc::channel(EVENT_QUEUE);
        let session = Self {
            inner,
            endpoint,
            local_key,
            _advertiser: advertiser,
            browser,
            commands,
            events,
            event_queue,
            nearby: Vec::new(),
            attempt: None,
            next_attempt: 0,
            lifecycle: Lifecycle::Open {
                expires: Instant::now() + OPEN_FOR,
                result: None,
            },
        };
        Ok((session, commands_tx))
    }

    async fn run(mut self) {
        let mut nearby = self.browser.as_ref().map(PairingBrowser::subscribe);
        let endpoint = Arc::clone(&self.endpoint);
        loop {
            let idle = self.attempt.is_none();
            // Open pairing closes itself once idle past `expires`; stopping
            // pairing closes once its last attempt is gone.
            let (open, expires) = match &self.lifecycle {
                Lifecycle::Open { expires, .. } => (true, idle.then_some(*expires)),
                Lifecycle::Stopping if idle => break,
                Lifecycle::Stopping => (false, None),
            };
            let deadline = self
                .attempt
                .as_ref()
                .map(|attempt| attempt.stage.deadline().at());
            tokio::select! {
                command = self.commands.recv(), if open => match command {
                    Some(command) => self.command(command).await,
                    None => self.stop(),
                },
                Some(tagged) = self.event_queue.recv() => self.event(tagged).await,
                incoming = endpoint.accept_incoming() => match incoming {
                    Ok(incoming) => self.incoming(incoming),
                    Err(_) => break,
                },
                changed = changed(nearby.as_mut()), if nearby.is_some() => {
                    if changed {
                        self.nearby_changed(nearby.as_ref());
                    } else {
                        nearby = None;
                    }
                }
                () = until(deadline) => self.deadline(),
                () = until(expires) => break,
            }
            self.publish();
        }
        if let Some(mut attempt) = self.attempt.take() {
            // Its dials hold the endpoint; wait until they let go.
            attempt.tasks.shutdown().await;
        }
        self.inner.observable.set_flow_pairing(None);
        let Self {
            endpoint: own,
            browser,
            _advertiser: advertiser,
            ..
        } = self;
        drop((own, browser, advertiser));
        // Closing frees the pairing port for the next time pairing opens.
        if let Some(endpoint) = Arc::into_inner(endpoint) {
            let _ = timeout(LINGER, endpoint.close()).await;
        }
        info!("Flow pairing closed");
    }

    fn publish(&self) {
        self.inner.observable.set_flow_pairing(self.phase());
    }

    /// What pairing shows, `None` once it is stopping.
    fn phase(&self) -> Option<FlowPairingPhase> {
        let Lifecycle::Open { result, .. } = &self.lifecycle else {
            return None;
        };
        if let Some(shown) = self
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.stage.phase())
        {
            return Some(shown);
        }
        if let Some(result) = result {
            return Some(result.clone());
        }
        let phase = if self.nearby.is_empty() {
            FlowPairingPhase::Searching
        } else {
            FlowPairingPhase::Found(
                self.nearby
                    .iter()
                    .map(|candidate| advertised_peer(&candidate.advert))
                    .collect(),
            )
        };
        Some(phase)
    }

    async fn command(&mut self, command: Command) {
        let (reply, answer) = match command {
            Command::Renew(reply) => {
                self.lifecycle.renew(Instant::now());
                (reply, Ok(()))
            }
            Command::Pair(target, reply) => {
                let answer = self.pair(target);
                (reply, answer)
            }
            Command::Confirm(reply) => {
                let answer = self.confirm().await;
                (reply, answer)
            }
            Command::Cancel(reply) => {
                self.cancel();
                (reply, Ok(()))
            }
            Command::Stop => {
                self.stop();
                return;
            }
        };
        let _ = reply.send(answer);
    }

    fn next_id(&mut self) -> u64 {
        self.next_attempt += 1;
        self.lifecycle.renew(Instant::now());
        self.next_attempt
    }

    fn pair(&mut self, target: FlowPairTarget) -> Result<(), FlowPairingCommandError> {
        // An attempt still flushing its last frames gives way; one whose
        // confirmation is on its way to a peer that saved this side does not.
        if self
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.stage.ending() != Some(Ending::Flushing))
        {
            return Err(FlowPairingCommandError::Busy);
        }
        let (peer, dial) = match target {
            FlowPairTarget::Nearby(key) => {
                let candidate = self
                    .nearby
                    .iter()
                    .find(|candidate| flow_key(candidate.advert.record.public_key) == key)
                    .ok_or(FlowPairingCommandError::UnknownPeer)?;
                let dial = Dial::Found {
                    key: candidate.advert.record.public_key,
                    addresses: candidate.addresses.clone(),
                };
                (Some(advertised_peer(&candidate.advert)), dial)
            }
            FlowPairTarget::Address(address) => (None, Dial::Address(address)),
        };
        // Dropping it also frees the pairing slot for this attempt.
        self.attempt = None;
        self.lifecycle.dismiss();
        let id = self.next_id();
        let address = match &dial {
            Dial::Address(address) => Some(address.clone()),
            Dial::Found { .. } => None,
        };
        let mut attempt = Attempt::new(id, address, Stage::dialing(peer, Instant::now()));
        let (endpoint, local_key, events) = (
            Arc::clone(&self.endpoint),
            self.local_key,
            self.events.clone(),
        );
        attempt.tasks.spawn(async move {
            let connected = dial.connect(&endpoint, local_key).await.map(Arc::new);
            let event = Event::Connected(connected);
            let _ = events.send(Tagged { attempt: id, event }).await;
        });
        self.attempt = Some(attempt);
        Ok(())
    }

    fn incoming(&mut self, incoming: IncomingConnection) {
        if self.attempt.is_some() || matches!(self.lifecycle, Lifecycle::Stopping) {
            incoming.refuse();
            return;
        }
        let id = self.next_id();
        let mut attempt = Attempt::new(id, None, Stage::accepting(Instant::now()));
        let events = self.events.clone();
        attempt.tasks.spawn(async move {
            let connected = incoming.establish().await.map(Arc::new).map_err(|error| {
                debug!(%error, "an inbound Flow pairing connection failed");
                FlowPairingFailure::ConnectionLost
            });
            let event = Event::Connected(connected);
            let _ = events.send(Tagged { attempt: id, event }).await;
        });
        self.attempt = Some(attempt);
    }

    async fn confirm(&mut self) -> Result<(), FlowPairingCommandError> {
        let path = self.inner.setup.config.clone();
        let Some(attempt) = &mut self.attempt else {
            return Err(FlowPairingCommandError::NothingToConfirm);
        };
        let address = attempt.address.clone();
        let Some(live) = attempt.stage.live_mut().filter(|live| {
            matches!(
                live.link.session.state(),
                PairingState::Prompted {
                    local_confirmed: false,
                    ..
                }
            )
        }) else {
            return Err(FlowPairingCommandError::NothingToConfirm);
        };
        let store = ConfigPeerStore {
            path,
            name: live.peer().name.clone(),
            address,
            saved: None,
        };
        let now = std::time::Instant::now();
        let (confirmed, saved) = live
            .link
            .confirm(store, move |session, store| {
                session.confirm_local(now, store)
            })
            .await;
        // An expired code confirms nothing, so the peer hears no
        // confirmation.
        let tell_peer = matches!(
            live.link.session.state(),
            PairingState::Prompted {
                local_confirmed: true,
                ..
            } | PairingState::Paired
        );
        if let Err(PairingError::Persistence(error)) = confirmed {
            let message = error.to_string();
            self.fail(FlowPairingFailure::SaveFailed { message }, Close::Now);
            return Ok(());
        }
        if tell_peer {
            let confirm = message_envelope(FrameKind::PairConfirm, &proto::PairConfirm::default());
            attempt.call(&self.events, Call::Confirm, confirm);
        }
        self.settle(saved);
        Ok(())
    }

    fn cancel(&mut self) {
        self.lifecycle.dismiss();
        if self
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.stage.shown())
        {
            self.fail_with(None, Close::Abort(PairingAbortReason::UserCancelled));
        }
    }

    /// Close pairing once the current attempt, cancelled, has told its peer.
    fn stop(&mut self) {
        self.cancel();
        if self
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.stage.ending().is_none())
        {
            self.attempt = None;
        }
        self.lifecycle = Lifecycle::Stopping;
        self.inner.observable.set_flow_pairing(None);
    }

    fn nearby_changed(&mut self, nearby: Option<&watch::Receiver<Vec<PairingCandidate>>>) {
        let Some(nearby) = nearby else {
            return;
        };
        let local_key = self.local_key;
        self.nearby = nearby
            .borrow()
            .iter()
            .filter(|candidate| candidate.advert.record.public_key != local_key)
            .cloned()
            .collect();
    }

    fn deadline(&mut self) {
        let Some(attempt) = &self.attempt else {
            return;
        };
        match attempt.stage.deadline() {
            Deadline::Linger(_) => self.attempt = None,
            Deadline::Timeout { failure, .. } => {
                info!("a Flow pairing attempt timed out");
                self.fail(failure, Close::Abort(PairingAbortReason::Timeout));
            }
        }
    }

    async fn event(&mut self, tagged: Tagged) {
        let Some(attempt) = self
            .attempt
            .as_mut()
            .filter(|attempt| attempt.id == tagged.attempt)
        else {
            return;
        };
        match tagged.event {
            Event::Connected(Ok(connection)) => self.connected(connection),
            Event::Connected(Err(failure)) => self.fail(failure, Close::Now),
            Event::Request(rpc) => self.request(rpc).await,
            Event::Notification(notification) => self.notification(&notification),
            Event::Answered(call, answer) => self.answered(call, answer),
            Event::Closed => {
                if attempt.stage.ending().is_some() {
                    self.attempt = None;
                } else {
                    let busy = attempt
                        .stage
                        .connection()
                        .is_some_and(|connection| connection.closed_as_pairing_busy());
                    let failure = if busy {
                        FlowPairingFailure::PeerBusy
                    } else {
                        FlowPairingFailure::ConnectionLost
                    };
                    self.fail(failure, Close::Now);
                }
            }
        }
    }

    fn connected(&mut self, connection: Arc<FlowConnection>) {
        let events = self.events.clone();
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        let link = Link {
            connection: Arc::clone(&connection),
            session: connection.pairing_session(),
        };
        let Ok(role) = attempt.stage.connect(hello_peer(&connection), link) else {
            return;
        };
        attempt
            .tasks
            .spawn(read(connection, events.clone(), attempt.id));
        if role == Role::Responder {
            return;
        }
        let start = attempt.stage.live_mut().and_then(|live| {
            let nonce = PairingNonce::random().ok()?;
            live.link.session.start(nonce).ok()
        });
        let Some(start) = start else {
            warn!("could not begin a Flow pairing ceremony");
            self.fail(FlowPairingFailure::ConnectionLost, Close::Now);
            return;
        };
        let start = message_envelope(FrameKind::PairStart, &start);
        attempt.call(&events, Call::Start, start);
    }

    async fn request(&mut self, rpc: IncomingRpc) {
        let (code_timeout_for_peer, path) = (
            self.inner.setup.code_timeout,
            self.inner.setup.config.clone(),
        );
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        let Some(live) = attempt.stage.live_mut() else {
            // The ceremony ended with the attempt, and nothing the peer
            // asks now starts it again.
            let refused = error_envelope(proto::ErrorCode::Invalid, "the pairing attempt ended");
            attempt.respond(rpc, refused);
            return;
        };
        let (role, name) = (live.role(), live.peer().name.clone());
        let link = &mut live.link;
        let now = std::time::Instant::now();
        let request = rpc.request();
        let mut failure = None;
        let mut saved = None;
        let response = match request.kind {
            FrameKind::PairStart if role == Role::Responder => {
                let prompted = request
                    .decode::<proto::PairStart>(InboundRole::Request)
                    .ok()
                    .zip(PairingNonce::random().ok())
                    .and_then(|(start, nonce)| {
                        link.session
                            .receive_start(&start, nonce, now, code_timeout_for_peer)
                            .ok()
                    })
                    .and_then(|prompted| {
                        let valid_for = code_timeout(prompted.timeout_ms);
                        attempt.stage.prompted(valid_for, Instant::now()).ok()?;
                        Some(prompted)
                    });
                match prompted {
                    Some(prompted) => {
                        self.lifecycle.dismiss();
                        message_envelope(FrameKind::PairPrompted, &prompted)
                    }
                    None => error_envelope(proto::ErrorCode::Invalid, "invalid PairStart"),
                }
            }
            FrameKind::PairReveal => {
                let revealed = request
                    .decode::<proto::PairReveal>(InboundRole::Request)
                    .ok()
                    .and_then(|reveal| link.session.receive_reveal(&reveal, now).ok());
                if let Some(revealed) = revealed {
                    message_envelope(FrameKind::PairRevealed, &revealed)
                } else {
                    failure = Some(refused_reveal(link.session.state()));
                    error_envelope(proto::ErrorCode::Invalid, "invalid PairReveal")
                }
            }
            FrameKind::PairConfirm => {
                let store = ConfigPeerStore {
                    path,
                    name,
                    address: attempt.address.clone(),
                    saved: None,
                };
                let outcome;
                (outcome, saved) = link
                    .confirm(store, move |session, store| {
                        session.receive_confirm(now, store)
                    })
                    .await;
                match outcome {
                    Ok(outcome) => message_envelope(FrameKind::PairOutcome, &outcome),
                    Err(PairingError::Persistence(error)) => {
                        let message = error.to_string();
                        failure = Some((FlowPairingFailure::SaveFailed { message }, Close::Flush));
                        error_envelope(proto::ErrorCode::Internal, "could not save the peer")
                    }
                    Err(_) => {
                        failure = Some((FlowPairingFailure::ConnectionLost, Close::Flush));
                        error_envelope(proto::ErrorCode::Invalid, "unexpected PairConfirm")
                    }
                }
            }
            FrameKind::PairStart => error_envelope(proto::ErrorCode::Invalid, "already pairing"),
            _ => error_envelope(
                proto::ErrorCode::UnsupportedKind,
                "only pairing requests are served",
            ),
        };
        attempt.respond(rpc, response);
        match failure {
            Some((failure, close)) => self.fail(failure, close),
            None => self.settle(saved),
        }
    }

    fn notification(&mut self, notification: &Envelope) {
        let Some(link) = self
            .attempt
            .as_mut()
            .and_then(|attempt| attempt.stage.live_mut())
            .map(|live| &mut live.link)
        else {
            return;
        };
        if notification.kind != FrameKind::PairAbort {
            return;
        }
        let Ok(abort) = notification.decode::<proto::PairAbort>(InboundRole::Notification) else {
            return;
        };
        if link.session.receive_abort(&abort).is_err() {
            return;
        }
        let failure = match link.session.state() {
            PairingState::Aborted(PairingAbortReason::CodeMismatch) => {
                FlowPairingFailure::CodeMismatch
            }
            PairingState::Aborted(PairingAbortReason::Timeout) => FlowPairingFailure::TimedOut,
            _ => FlowPairingFailure::PeerCancelled,
        };
        self.fail(failure, Close::Now);
    }

    fn answered(&mut self, call: Call, answer: Result<Envelope, TransportError>) {
        let events = self.events.clone();
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        if call == Call::Confirm {
            if attempt.stage.ending() == Some(Ending::Paired) {
                // The peer has this side's confirmation, so it saved this
                // side too.
                self.attempt = None;
                return;
            }
            // Only an attempt showing its code waits for this answer.
            if attempt.stage.confirm_answered().is_err() {
                return;
            }
        }
        let Some(link) = attempt.stage.live_mut().map(|live| &mut live.link) else {
            return;
        };
        if link.session.state() == PairingState::Paired {
            return;
        }
        let answer = match answer {
            Ok(answer) if answer.kind == expected_answer(call) => answer,
            // A refused reveal comes with a `PairAbort` saying why, or the
            // peer closes the connection.
            Ok(answer) if answer.kind == FrameKind::Error && call == Call::Reveal => return,
            Ok(answer) => {
                debug!(kind = ?answer.kind, ?call, "the Flow pairing peer refused a request");
                return self.fail(FlowPairingFailure::ConnectionLost, Close::Now);
            }
            Err(error) => {
                debug!(%error, ?call, "a Flow pairing request failed");
                let failure = if link.connection.closed_as_pairing_busy() {
                    FlowPairingFailure::PeerBusy
                } else {
                    FlowPairingFailure::ConnectionLost
                };
                return self.fail(failure, Close::Now);
            }
        };
        let now = std::time::Instant::now();
        let progressed = match call {
            Call::Start => match answer.decode::<proto::PairPrompted>(InboundRole::Request) {
                Ok(prompted) => match link.session.receive_prompted(&prompted, now) {
                    Ok(reveal) => attempt
                        .stage
                        .prompted(code_timeout(prompted.timeout_ms), Instant::now())
                        .map(|()| Some(reveal))
                        .map_err(drop),
                    Err(_) => Err(()),
                },
                Err(_) => Err(()),
            },
            Call::Reveal => link
                .session
                .receive_revealed(now)
                .map(|()| None)
                .map_err(drop),
            Call::Confirm => match answer.decode::<proto::PairOutcome>(InboundRole::Request) {
                Ok(outcome) => link
                    .session
                    .receive_outcome(&outcome)
                    .map(|()| None)
                    .map_err(drop),
                Err(_) => Err(()),
            },
        };
        match progressed {
            Ok(Some(reveal)) => {
                let reveal = message_envelope(FrameKind::PairReveal, &reveal);
                attempt.call(&events, Call::Reveal, reveal);
            }
            Ok(None) => self.settle(None),
            Err(()) => {
                debug!(?call, "the Flow pairing peer answered out of turn");
                let state = attempt.stage.live().map(|live| live.link.session.state());
                let failure = match state {
                    Some(PairingState::TimedOut) => FlowPairingFailure::TimedOut,
                    _ => FlowPairingFailure::ConnectionLost,
                };
                self.fail(failure, Close::Abort(PairingAbortReason::UserCancelled));
            }
        }
    }

    /// Act on where the ceremony now stands: `saved` names the peer if this
    /// step saved it.
    fn settle(&mut self, saved: Option<FlowPeerName>) {
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        let Some(state) = attempt.stage.live().map(|live| live.link.session.state()) else {
            return;
        };
        match state {
            PairingState::Paired => {
                let Some(name) = saved else {
                    return;
                };
                let Ok(live) = attempt.stage.paired(Instant::now()) else {
                    return;
                };
                if let Err(error) = live
                    .link
                    .connection
                    .promote_after_pairing(&live.link.session)
                {
                    debug!(%error, "the Flow pairing connection was not promoted");
                }
                info!(peer = %name, "Flow peer paired and saved");
                let peer = live.peer().clone();
                self.lifecycle.show(FlowPairingPhase::Paired { peer, name });
                self.inner.saved.send_modify(|saved| *saved += 1);
            }
            PairingState::Rejected => self.fail(FlowPairingFailure::PeerCancelled, Close::Now),
            PairingState::TimedOut => self.fail(
                FlowPairingFailure::TimedOut,
                Close::Abort(PairingAbortReason::Timeout),
            ),
            _ => {}
        }
    }

    /// End the attempt with `failure`, shown to the user if the attempt was.
    fn fail(&mut self, failure: FlowPairingFailure, close: Close) {
        self.fail_with(Some(failure), close);
    }

    /// End the attempt, showing `failure` if there is one and the user saw
    /// the attempt.
    fn fail_with(&mut self, failure: Option<FlowPairingFailure>, close: Close) {
        let Some(attempt) = &mut self.attempt else {
            return;
        };
        if let Some(reason) = failure.filter(|_| attempt.stage.shown()) {
            info!(?reason, "a Flow pairing attempt failed");
            self.lifecycle.show(FlowPairingPhase::Failed {
                peer: attempt.stage.peer().cloned(),
                reason,
            });
        }
        if let Close::Abort(reason) = close {
            attempt.abort(reason);
        }
        // Only a connected attempt has frames on their way to the peer.
        if close == Close::Now || attempt.stage.flush(Instant::now()).is_err() {
            self.attempt = None;
        }
    }
}

/// Where a pairing request goes.
enum Dial {
    /// A computer found with pairing open: only its advertised key may
    /// answer.
    Found {
        key: PublicKey,
        addresses: Vec<SocketAddr>,
    },
    /// An address the user typed: whichever computer answers there, other
    /// than this one.
    Address(String),
}

impl Dial {
    /// Dial every address at once and keep the first that authenticates as
    /// the expected computer.
    async fn connect(
        self,
        endpoint: &Arc<FlowEndpoint>,
        local_key: PublicKey,
    ) -> Result<FlowConnection, FlowPairingFailure> {
        let (addresses, expected) = match self {
            Self::Found { key, addresses } => (addresses, Some(key)),
            Self::Address(address) => {
                let source = ManualCandidateSource::with_port([address], PAIRING_PORT);
                let addresses = source.candidates(local_key).await.map_err(|error| {
                    debug!(%error, "a Flow pairing address did not resolve");
                    FlowPairingFailure::Unreachable
                })?;
                (addresses, None)
            }
        };
        let mut dials = JoinSet::new();
        for address in addresses {
            let endpoint = Arc::clone(endpoint);
            dials.spawn(async move { endpoint.dial(address).await });
        }
        let mut failure = FlowPairingFailure::Unreachable;
        while let Some(dialed) = dials.join_next().await {
            match dialed {
                Ok(Ok(dialed))
                    if dialed.peer_key() != local_key
                        && expected.is_none_or(|key| key == dialed.peer_key()) =>
                {
                    drop(dials);
                    return dialed.establish().await.map_err(|error| {
                        debug!(%error, "a Flow pairing handshake failed");
                        if error.is_version_disjoint() {
                            FlowPairingFailure::IncompatibleVersion
                        } else {
                            FlowPairingFailure::ConnectionLost
                        }
                    });
                }
                Ok(Err(error)) if error.is_refused() => failure = FlowPairingFailure::PeerBusy,
                Ok(Err(error)) => debug!(%error, "a Flow pairing dial failed"),
                _ => {}
            }
        }
        Err(failure)
    }
}

/// Read every stream the peer opens until the connection closes, then
/// report that it closed.
async fn read(connection: Arc<FlowConnection>, events: mpsc::Sender<Tagged>, attempt: u64) {
    let mut reads = JoinSet::new();
    loop {
        let room = reads.len() < MAX_STREAM_READS;
        tokio::select! {
            rpc = connection.accept_rpc_stream(), if room => {
                let Ok(inbound) = rpc else { break };
                let events = events.clone();
                reads.spawn(async move {
                    match inbound.read().await {
                        Ok(RpcEvent::Request(rpc)) => {
                            let event = Event::Request(rpc);
                            let _ = events.send(Tagged { attempt, event }).await;
                        }
                        Ok(RpcEvent::Rejected(code)) => {
                            debug!(?code, "refused a Flow pairing request");
                        }
                        Err(error) => debug!(%error, "a Flow pairing request stream failed"),
                    }
                });
            }
            notification = connection.accept_notification_stream(), if room => {
                let Ok(inbound) = notification else { break };
                let events = events.clone();
                reads.spawn(async move {
                    if let Ok(NotificationEvent::Notification(envelope)) = inbound.read().await {
                        let event = Event::Notification(envelope);
                        let _ = events.send(Tagged { attempt, event }).await;
                    }
                });
            }
            Some(_) = reads.join_next(), if !reads.is_empty() => {}
        }
    }
    // A notification the peer sent just before closing is still delivered.
    while reads.join_next().await.is_some() {}
    let _ = events
        .send(Tagged {
            attempt,
            event: Event::Closed,
        })
        .await;
}

/// Sleep until `at`, or forever without one.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The next value of `nearby`, or `false` once its browser is gone.
async fn changed(nearby: Option<&mut watch::Receiver<Vec<PairingCandidate>>>) -> bool {
    match nearby {
        Some(nearby) => nearby.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

/// Why the ceremony fails when this side refused the peer's `PairReveal`,
/// and what the peer is told, given where the session stands after it.
fn refused_reveal(state: PairingState) -> (FlowPairingFailure, Close) {
    match state {
        // The nonce did not open the commitment.
        PairingState::Aborted(PairingAbortReason::CodeMismatch) => (
            FlowPairingFailure::CodeMismatch,
            Close::Abort(PairingAbortReason::CodeMismatch),
        ),
        PairingState::TimedOut => (
            FlowPairingFailure::TimedOut,
            Close::Abort(PairingAbortReason::Timeout),
        ),
        _ => (
            FlowPairingFailure::ConnectionLost,
            Close::Abort(PairingAbortReason::UserCancelled),
        ),
    }
}

/// The answer each of this side's requests expects.
const fn expected_answer(call: Call) -> FrameKind {
    match call {
        Call::Start => FrameKind::PairPrompted,
        Call::Reveal => FrameKind::PairRevealed,
        Call::Confirm => FrameKind::PairOutcome,
    }
}

/// How long a code stays valid, from the responder's `timeout_ms`.
fn code_timeout(timeout_ms: u32) -> Duration {
    if timeout_ms == 0 {
        openlogi_flow::pairing::DEFAULT_PAIRING_TIMEOUT
    } else {
        Duration::from_millis(u64::from(timeout_ms))
    }
}

/// Bind the pairing endpoint on `address`, or on any port when that one is
/// taken, as by a second agent on this computer; mDNS still finds it.
fn bind_pairing(
    address: SocketAddr,
    identity: MachineIdentity,
) -> Result<FlowEndpoint, FlowPairingCommandError> {
    let local_key = identity.public_key();
    let trust = PeerTrust::pairing(std::iter::empty());
    match bind(address, identity.clone(), trust.clone(), hello(local_key)) {
        Err(TransportError::Io(error))
            if error.kind() == io::ErrorKind::AddrInUse && address.port() != 0 =>
        {
            debug!(%error, "the Flow pairing port is taken; pairing on another");
            bind(
                SocketAddr::new(address.ip(), 0),
                identity,
                trust,
                hello(local_key),
            )
        }
        bound => bound,
    }
    .map_err(|error| unavailable(&error))
}

fn advertise(local_key: PublicKey, port: u16) -> Option<MdnsAdvertiser> {
    let advert = PairingAdvert {
        record: MdnsRecord::new(local_key, PROTOCOL_MIN, PROTOCOL_MAX).ok()?,
        machine_name: machine_name(),
        platform: platform(),
    };
    MdnsAdvertiser::start_pairing(&advert, port)
        .inspect_err(|error| {
            warn!(%error, "Flow pairing cannot advertise; pairing by address still works");
        })
        .ok()
}

fn browse() -> Option<PairingBrowser> {
    PairingBrowser::browse(PROTOCOL_MIN, PROTOCOL_MAX)
        .inspect_err(|error| {
            warn!(%error, "Flow pairing cannot browse; pairing by address still works");
        })
        .ok()
}

fn flow_key(key: PublicKey) -> FlowPublicKey {
    FlowPublicKey::from_bytes(*key.as_bytes())
}

/// The longest name shown for another computer, in characters.
const MAX_SHOWN_NAME: usize = 64;

/// A name another computer gave itself, safe to show: no control
/// characters, no surrounding space, and bounded.
fn shown_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SHOWN_NAME)
        .collect();
    name.trim().to_owned()
}

fn shown_platform(platform: Option<proto::Platform>) -> FlowPlatform {
    match platform {
        Some(proto::Platform::Macos) => FlowPlatform::Macos,
        Some(proto::Platform::Windows) => FlowPlatform::Windows,
        Some(proto::Platform::LinuxX11 | proto::Platform::LinuxWayland) => FlowPlatform::Linux,
        _ => FlowPlatform::Unknown,
    }
}

fn advertised_peer(advert: &PairingAdvert) -> FlowPairingPeer {
    FlowPairingPeer {
        name: shown_name(&advert.machine_name),
        public_key: flow_key(advert.record.public_key),
        platform: shown_platform(Some(advert.platform)),
    }
}

fn hello_peer(connection: &FlowConnection) -> FlowPairingPeer {
    let hello = connection.peer_hello();
    FlowPairingPeer {
        name: shown_name(&hello.machine_name),
        public_key: flow_key(connection.peer_key()),
        platform: shown_platform(hello.platform.as_known()),
    }
}
