//! Connecting a client to the agent, handshake and all.
//!
//! Every client — the settings app, the overlay helper, the CLI — has to
//! connect, wrap the stream, spawn the tarpc client, check the agent's protocol
//! version, and declare what kind of client it is before any real RPC. That
//! sequence is a policy, and a policy every consumer must apply identically has
//! exactly one owner. This module exports the *decision* — [`connect_as`]
//! either yields a usable [`AgentClient`] or says why not in [`ConnectError`] —
//! and keeps the ingredients (the raw version number, the declare call) out of
//! the public surface, so no consumer can recombine them a second way.
//!
//! The one caller that legitimately judges a version without declaring itself
//! is the agent's own takeover handshake, which must recognise an *older* lock
//! holder without waking it. It gets [`probe_version`] and judges the answer
//! with [`ProtocolSkew::check`], the same rule.
//!
//! Both entry points reach the socket and finish the handshake within
//! [`HANDSHAKE_TIMEOUT`], so no caller wraps them in a timeout of its own: an
//! agent that cannot accept a connection and answer the two handshake calls
//! from memory in that window is wedged, not busy.
//!
//! The rest of what every observing client repeats lives here too, as
//! [`Observer`]: a connection that owns its generation ledger and keeps
//! exactly one observe call in flight, under a request deadline that outlasts
//! the agent's hold.

use std::cmp::Ordering;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use tarpc::client::{self, RpcError};
use tarpc::context::{self, Context};

use crate::{
    AgentClient, ClientKind, Generation, OBSERVE_HOLD, Observation, PROTOCOL_VERSION,
    RingObservation, transport,
};

/// Why a client could not be established.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The agent's socket could not be reached: it is not running, not
    /// listening yet, or the endpoint name could not be resolved.
    #[error("could not reach the agent's IPC endpoint: {0}")]
    Endpoint(#[from] std::io::Error),
    /// The socket accepted the connection but the agent never finished the
    /// handshake — a hung or dying agent rather than an absent one.
    #[error("the agent did not complete the IPC handshake: {0}")]
    Handshake(#[from] RpcError),
    /// The agent answered, but speaks a different protocol. The variant says
    /// which side is stale.
    #[error(transparent)]
    Skew(#[from] ProtocolSkew),
    /// Reaching the socket and finishing the handshake together outran
    /// [`HANDSHAKE_TIMEOUT`]: a wedged agent, or an endpoint that accepts and
    /// never answers, and best treated as absent.
    #[error("the agent did not complete the IPC handshake within {} s", HANDSHAKE_TIMEOUT.as_secs())]
    Timeout,
}

/// How long reaching the socket and completing the handshake may take,
/// together.
///
/// Accepting a local connection and answering the two handshake calls are all
/// served from memory, so an agent that cannot manage them in this window is
/// wedged, not busy. A client treats it as absent and keeps retrying; the
/// takeover handshake leaves such a holder alone rather than reason about it.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// A protocol mismatch between this build and the agent, judged once here.
///
/// The direction is the whole point. An older agent is a leftover waiting to
/// be replaced — by launchd's respawn, its own update watch, or the GUI's
/// spawn — so a client keeps retrying. A newer one means *this process* is the
/// stale side, and only its relaunch helps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolSkew {
    /// The agent is behind this build.
    #[error(
        "the agent speaks protocol v{agent}, older than this client's v{PROTOCOL_VERSION}; it is waiting to be replaced"
    )]
    AgentOlder {
        /// What the agent answered to `protocol_version`.
        agent: u32,
    },
    /// The agent is ahead of this build.
    #[error(
        "the agent speaks protocol v{agent}, newer than this client's v{PROTOCOL_VERSION}; this process needs a relaunch"
    )]
    AgentNewer {
        /// What the agent answered to `protocol_version`.
        agent: u32,
    },
}

impl ProtocolSkew {
    /// Judge an agent's answer to `protocol_version` against this build's.
    ///
    /// # Errors
    ///
    /// The skew, when the two differ.
    pub fn check(agent: u32) -> Result<(), Self> {
        match agent.cmp(&PROTOCOL_VERSION) {
            Ordering::Less => Err(Self::AgentOlder { agent }),
            Ordering::Greater => Err(Self::AgentNewer { agent }),
            Ordering::Equal => Ok(()),
        }
    }

    /// The version the agent reported.
    #[must_use]
    pub const fn agent(self) -> u32 {
        match self {
            Self::AgentOlder { agent } | Self::AgentNewer { agent } => agent,
        }
    }
}

/// Connect to the agent as `kind`: reach the socket, verify the protocol,
/// declare — all within [`HANDSHAKE_TIMEOUT`].
///
/// The declaration comes last, and only once the versions agree: it is what
/// arms a dormant agent when `kind` is [`ClientKind::Gui`], and a mismatched
/// client must never wake one.
///
/// # Errors
///
/// [`ConnectError::Endpoint`] when the socket cannot be reached,
/// [`ConnectError::Handshake`] when the agent drops out before the handshake
/// completes, [`ConnectError::Skew`] when the two sides disagree on the
/// protocol, [`ConnectError::Timeout`] when the whole sequence outruns the
/// timeout.
pub async fn connect_as(kind: ClientKind) -> Result<AgentClient, ConnectError> {
    connect_with(open(), kind).await
}

/// Ask whichever agent holds the socket which protocol it speaks, and nothing
/// else — within [`HANDSHAKE_TIMEOUT`].
///
/// For the agent's takeover handshake, which must judge a lock holder without
/// declaring itself a client of it. Everything else uses [`connect_as`].
///
/// # Errors
///
/// [`ConnectError::Endpoint`] when the socket cannot be reached,
/// [`ConnectError::Handshake`] when the holder drops out before answering,
/// [`ConnectError::Timeout`] when reaching it and asking outrun the timeout.
pub async fn probe_version() -> Result<u32, ConnectError> {
    probe_with(open()).await
}

/// A tarpc client on a fresh connection to the agent's socket.
async fn open() -> Result<AgentClient, ConnectError> {
    let stream = transport::connect().await?;
    Ok(AgentClient::new(client::Config::default(), transport::wrap(stream)).spawn())
}

/// [`connect_as`] over any way of opening a client, so the timeout can be
/// exercised against an in-memory agent — or an endpoint that never opens.
async fn connect_with(
    open: impl Future<Output = Result<AgentClient, ConnectError>>,
    kind: ClientKind,
) -> Result<AgentClient, ConnectError> {
    within_timeout(async { handshake(open.await?, kind).await }).await
}

/// [`probe_version`] over any way of opening a client.
async fn probe_with(
    open: impl Future<Output = Result<AgentClient, ConnectError>>,
) -> Result<u32, ConnectError> {
    within_timeout(async {
        let client = open.await?;
        Ok(client.protocol_version(context::current()).await?)
    })
    .await
}

/// Bound one connect sequence — opening included — by [`HANDSHAKE_TIMEOUT`].
async fn within_timeout<T>(
    sequence: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, ConnectError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, sequence)
        .await
        .unwrap_or(Err(ConnectError::Timeout))
}

/// The handshake proper: verify the protocol, then declare. `protocol_version`
/// is method 0 and wire-stable across every version, so it is the only call
/// worth making before the two sides are known to agree.
async fn handshake(client: AgentClient, kind: ClientKind) -> Result<AgentClient, ConnectError> {
    let version = client.protocol_version(context::current()).await?;
    ProtocolSkew::check(version)?;
    client.declare_client(context::current(), kind).await?;
    Ok(client)
}

/// An answer to an observe call: anything stamped with the agent's
/// [`Generation`]. Public only because it bounds [`Observer`]; the ledger that
/// reads the stamp is not.
pub trait Stamped {
    /// The generation this answer describes.
    fn generation(&self) -> Generation;
}

impl Stamped for Observation {
    fn generation(&self) -> Generation {
        self.generation
    }
}

impl Stamped for RingObservation {
    fn generation(&self) -> Generation {
        self.generation
    }
}

/// One connection's view of the agent's generation counter.
///
/// Starts at 0 — "I have seen nothing" — so the first answer is the agent's
/// whole state, and lives no longer than its connection: a replacement agent
/// numbers its own generations from 1 again, so a ledger carried across a
/// reconnect would make the new agent's first answers look stale.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ledger {
    seen: Generation,
}

impl Ledger {
    /// A ledger that has seen nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self { seen: 0 }
    }

    /// What to pass as `since` on the next observe call.
    #[must_use]
    pub const fn seen(self) -> Generation {
        self.seen
    }

    /// Fold an answer in.
    ///
    /// `Some` only for a generation newer than everything this connection has
    /// seen. An equal one is the hold elapsing with nothing new; a lower one
    /// is a stale reply. Neither may move a client's state backwards.
    pub fn accept<T: Stamped>(&mut self, answer: T) -> Option<T> {
        if answer.generation() <= self.seen {
            return None;
        }
        self.seen = answer.generation();
        Some(answer)
    }
}

/// How much longer than the agent's hold an observe request may take before
/// the client gives up on the connection.
const OBSERVE_GRACE: Duration = Duration::from_secs(5);

/// A request context for an observe call.
///
/// Its deadline sits above [`OBSERVE_HOLD`]: tarpc cancels a handler whose
/// deadline passes, so a shorter one would kill the hold instead of waiting it
/// out.
#[must_use]
pub(crate) fn observe_context() -> Context {
    let mut ctx = context::current();
    ctx.deadline = Instant::now() + OBSERVE_HOLD + OBSERVE_GRACE;
    ctx
}

/// An observe call in flight. Boxed because it is stored across the turns of
/// a client's loop; it owns a clone of the client.
type InFlight<T> = Pin<Box<dyn Future<Output = Result<T, RpcError>> + Send>>;

/// One connection observing the agent: the client, what that connection has
/// seen, and the one observe call it keeps in flight.
///
/// The three live and die together. A ledger carried over to the next
/// connection would make a replacement agent's first answers look stale, and
/// an answer from a replaced connection must never reach the client's state;
/// dropping the `Observer` with its connection rules out both, because the
/// call in flight goes with it.
pub struct Observer<T: Stamped> {
    client: AgentClient,
    ledger: Ledger,
    in_flight: InFlight<T>,
    arm: fn(&AgentClient, Generation) -> InFlight<T>,
}

impl Observer<Observation> {
    /// Observe the agent's state over `client`.
    #[must_use]
    pub fn state(client: AgentClient) -> Self {
        Self::new(client, |client, since| {
            let client = client.clone();
            Box::pin(async move { client.observe(observe_context(), since).await })
        })
    }
}

impl Observer<RingObservation> {
    /// Observe the ring the overlay should be showing over `client`.
    #[must_use]
    pub fn action_ring(client: AgentClient) -> Self {
        Self::new(client, |client, since| {
            let client = client.clone();
            Box::pin(async move { client.observe_action_ring(observe_context(), since).await })
        })
    }
}

impl<T: Stamped> Observer<T> {
    fn new(client: AgentClient, arm: fn(&AgentClient, Generation) -> InFlight<T>) -> Self {
        let ledger = Ledger::new();
        let in_flight = arm(&client, ledger.seen());
        Self {
            client,
            ledger,
            in_flight,
            arm,
        }
    }

    /// The connection, for the other calls that share it. tarpc multiplexes
    /// requests, so they never disturb the observe call in flight.
    #[must_use]
    pub fn client(&self) -> &AgentClient {
        &self.client
    }

    /// Wait for the call in flight, then arm the next one.
    ///
    /// `Ok(Some)` is an answer newer than everything this connection has
    /// seen. `Ok(None)` is the agent's hold elapsing as a heartbeat, or a
    /// stale reply: the connection is alive and there is nothing to apply.
    ///
    /// Cancel-safe: the call in flight is a field, not part of this future,
    /// so a `select!` that drops this future loses nothing and the next call
    /// picks the same request up again.
    ///
    /// # Errors
    ///
    /// The transport's error once the connection is gone. The `Observer` has
    /// nothing left to offer then; drop it with the connection.
    pub async fn next(&mut self) -> Result<Option<T>, RpcError> {
        let answered = (&mut self.in_flight).await;
        let fresh = answered.map(|answer| self.ledger.accept(answer));
        // A finished call must never be polled again, whatever the caller
        // does with this result. The next one is only sent once it is polled.
        self.in_flight = (self.arm)(&self.client, self.ledger.seen());
        fresh
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::testing::in_memory_agent;
    use crate::{AgentRequest, AgentResponse};

    /// An agent that answers the handshake with `version` and records who
    /// declares themselves to it.
    fn agent_speaking(version: u32) -> (AgentClient, Arc<Mutex<Vec<ClientKind>>>) {
        let declared = Arc::new(Mutex::new(Vec::new()));
        let seen = declared.clone();
        let client = in_memory_agent(
            move |request| {
                let seen = seen.clone();
                Box::pin(async move {
                    match request {
                        AgentRequest::ProtocolVersion {} => {
                            Ok(AgentResponse::ProtocolVersion(version))
                        }
                        AgentRequest::DeclareClient { kind } => {
                            seen.lock().unwrap().push(kind);
                            Ok(AgentResponse::DeclareClient(()))
                        }
                        other => panic!("the handshake sent an unexpected request: {other:?}"),
                    }
                })
            },
            std::future::pending(),
        );
        (client, declared)
    }

    /// The socket already reached: what `open` yields in production.
    fn opened(client: AgentClient) -> impl Future<Output = Result<AgentClient, ConnectError>> {
        std::future::ready(Ok(client))
    }

    #[tokio::test]
    async fn a_matching_agent_is_declared_to() {
        let (client, declared) = agent_speaking(PROTOCOL_VERSION);

        connect_with(opened(client), ClientKind::Overlay)
            .await
            .expect("matching versions establish a client");

        assert_eq!(*declared.lock().unwrap(), [ClientKind::Overlay]);
    }

    #[tokio::test]
    async fn an_older_agent_is_left_undeclared_to() {
        // Declaring is what arms a dormant agent; a client that cannot talk
        // to it must not wake it.
        let (client, declared) = agent_speaking(PROTOCOL_VERSION - 1);

        let Err(error) = connect_with(opened(client), ClientKind::Gui).await else {
            panic!("an older agent is not usable");
        };

        assert!(matches!(
            error,
            ConnectError::Skew(ProtocolSkew::AgentOlder { agent }) if agent == PROTOCOL_VERSION - 1
        ));
        assert!(
            declared.lock().unwrap().is_empty(),
            "no declaration to a stale agent"
        );
    }

    #[tokio::test]
    async fn a_newer_agent_makes_this_client_the_stale_side() {
        let (client, declared) = agent_speaking(PROTOCOL_VERSION + 1);

        let Err(error) = connect_with(opened(client), ClientKind::Cli).await else {
            panic!("a newer agent is not usable");
        };

        assert!(matches!(
            error,
            ConnectError::Skew(ProtocolSkew::AgentNewer { agent }) if agent == PROTOCOL_VERSION + 1
        ));
        assert!(declared.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_agent_is_given_up_on() {
        // A socket that accepts and never answers is a wedged agent, not a
        // slow one; the handshake is answered from memory.
        let silent = in_memory_agent(|_| Box::pin(std::future::pending()), std::future::pending());

        let Err(error) = connect_with(opened(silent), ClientKind::Gui).await else {
            panic!("a silent agent is not usable");
        };

        assert!(matches!(error, ConnectError::Timeout), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_that_never_opens_is_given_up_on() {
        // The timeout covers reaching the agent, not only its two answers:
        // a socket nobody accepts on must not hang the caller.
        let Err(error) = connect_with(std::future::pending(), ClientKind::Cli).await else {
            panic!("an endpoint that never opens is not usable");
        };
        assert!(matches!(error, ConnectError::Timeout), "{error}");

        let Err(error) = probe_with(std::future::pending()).await else {
            panic!("nor can its version be probed");
        };
        assert!(matches!(error, ConnectError::Timeout), "{error}");
    }

    struct Stamp(Generation);

    impl Stamped for Stamp {
        fn generation(&self) -> Generation {
            self.0
        }
    }

    #[test]
    fn the_ledger_only_moves_forward() {
        let mut ledger = Ledger::new();
        assert_eq!(ledger.seen(), 0, "a fresh ledger asks for everything");

        assert!(
            ledger.accept(Stamp(2)).is_some(),
            "a newer generation is accepted"
        );
        assert_eq!(ledger.seen(), 2);

        assert!(
            ledger.accept(Stamp(1)).is_none(),
            "a stale reply is dropped"
        );
        assert!(
            ledger.accept(Stamp(2)).is_none(),
            "the hold's heartbeat is dropped"
        );
        assert_eq!(ledger.seen(), 2, "neither rewinds the ledger");

        assert!(ledger.accept(Stamp(3)).is_some());
        assert_eq!(ledger.seen(), 3);
    }

    /// An agent whose ring channel answers with `generations` in turn and
    /// then holds the request open, as a quiet agent does. Every `since` it is
    /// asked with is recorded; `release` lets the first answer out, so a test
    /// can keep a request in flight for as long as it needs.
    fn ring_agent(
        generations: impl IntoIterator<Item = Generation>,
        release: Arc<tokio::sync::Notify>,
    ) -> (AgentClient, Arc<Mutex<Vec<Generation>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(
            generations
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let recorded = asked.clone();
        let client = in_memory_agent(
            move |request| {
                let recorded = recorded.clone();
                let answers = answers.clone();
                let release = release.clone();
                Box::pin(async move {
                    let AgentRequest::ObserveActionRing { since } = request else {
                        panic!("an observer sent an unexpected request: {request:?}");
                    };
                    let first = {
                        let mut recorded = recorded.lock().unwrap();
                        recorded.push(since);
                        recorded.len() == 1
                    };
                    if first {
                        release.notified().await;
                    }
                    let next = answers.lock().unwrap().pop_front();
                    match next {
                        Some(generation) => Ok(AgentResponse::ObserveActionRing(RingObservation {
                            generation,
                            invocation: None,
                        })),
                        None => std::future::pending().await,
                    }
                })
            },
            std::future::pending(),
        );
        (client, asked)
    }

    /// A released [`ring_agent`]: every answer goes out at once.
    fn answering(
        generations: impl IntoIterator<Item = Generation>,
    ) -> (AgentClient, Arc<Mutex<Vec<Generation>>>) {
        let release = Arc::new(tokio::sync::Notify::new());
        release.notify_one();
        ring_agent(generations, release)
    }

    /// Poll `observer` until the agent has heard from it and gone quiet.
    async fn until_held(observer: &mut Observer<RingObservation>) {
        let held = tokio::time::timeout(Duration::from_secs(1), observer.next()).await;
        assert!(held.is_err(), "a quiet agent holds the request open");
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_newer_generation_is_an_answer_and_every_reply_rearms() {
        let (client, asked) = answering([2, 2, 1, 3]);
        let mut observer = Observer::action_ring(client);

        let mut seen = Vec::new();
        for _ in 0..4 {
            let fresh = observer.next().await.expect("the agent is up");
            seen.push(fresh.map(|observed| observed.generation));
        }

        assert_eq!(
            seen,
            [Some(2), None, None, Some(3)],
            "the heartbeat and the stale reply are not answers"
        );
        assert_eq!(
            *asked.lock().unwrap(),
            [0, 2, 2, 2],
            "each reply armed the next call with what the connection had seen"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_replacement_connection_starts_from_generation_zero() {
        // A replacement agent numbers its own generations from 1 again; a
        // cursor carried across the reconnect would make its first answers
        // look stale.
        let (first, first_asked) = answering([17]);
        let mut observer = Observer::action_ring(first);
        assert!(
            observer.next().await.expect("the agent is up").is_some(),
            "the first answer on a connection is news"
        );
        until_held(&mut observer).await;
        assert_eq!(*first_asked.lock().unwrap(), [0, 17]);

        let (replacement, replacement_asked) = answering([]);
        let mut observer = Observer::action_ring(replacement);
        until_held(&mut observer).await;

        assert_eq!(*replacement_asked.lock().unwrap(), [0]);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_next_mid_hold_keeps_the_same_call_in_flight() {
        // A client loop selects over `next()` and its other sources, so the
        // future is dropped every time another arm wins. The request the
        // agent is holding must survive that, or every command would restart
        // the hold and an answer racing the drop would be lost.
        let release = Arc::new(tokio::sync::Notify::new());
        let (client, asked) = ring_agent([5], release.clone());
        let mut observer = Observer::action_ring(client);

        until_held(&mut observer).await;
        release.notify_one();
        let fresh = observer.next().await.expect("the agent is up");

        assert_eq!(fresh.map(|observed| observed.generation), Some(5));
        assert_eq!(
            *asked.lock().unwrap(),
            [0],
            "the answer came from the call the dropped future had started"
        );
    }

    #[tokio::test]
    async fn a_closed_connection_is_an_error_every_time_it_is_asked() {
        let gone = in_memory_agent(|_| Box::pin(std::future::pending()), std::future::ready(()));
        let mut observer = Observer::action_ring(gone);

        observer.next().await.unwrap_err();
        assert!(
            observer.next().await.is_err(),
            "asking again must not poll the finished call"
        );
    }
}
