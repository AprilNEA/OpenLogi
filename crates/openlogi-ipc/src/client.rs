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
//! The rest of what every observing client repeats lives here too: the
//! per-connection generation [`Ledger`], the request [`observe_context`] whose
//! deadline outlasts the agent's hold, and the dedicated thread the GPUI
//! processes run their client loop on ([`spawn_client_thread`]).

use std::cmp::Ordering;
use std::future::Future;
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
}

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
/// declare.
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
/// protocol.
pub async fn connect_as(kind: ClientKind) -> Result<AgentClient, ConnectError> {
    establish(open().await?, kind).await
}

/// Ask whichever agent holds the socket which protocol it speaks, and nothing
/// else.
///
/// For the agent's takeover handshake, which must judge a lock holder without
/// declaring itself a client of it. Everything else uses [`connect_as`].
///
/// # Errors
///
/// [`ConnectError::Endpoint`] when the socket cannot be reached,
/// [`ConnectError::Handshake`] when the holder does not answer.
pub async fn probe_version() -> Result<u32, ConnectError> {
    Ok(open().await?.protocol_version(context::current()).await?)
}

/// A tarpc client on a fresh connection to the agent's socket.
async fn open() -> Result<AgentClient, ConnectError> {
    let stream = transport::connect().await?;
    Ok(AgentClient::new(client::Config::default(), transport::wrap(stream)).spawn())
}

/// The policy half of [`connect_as`], separated from the socket so it can be
/// exercised against an in-memory agent: `protocol_version` is method 0 and
/// wire-stable across every version, so it is the only call worth making
/// before the two sides are known to agree.
async fn establish(client: AgentClient, kind: ClientKind) -> Result<AgentClient, ConnectError> {
    let version = client.protocol_version(context::current()).await?;
    ProtocolSkew::check(version)?;
    client.declare_client(context::current(), kind).await?;
    Ok(client)
}

/// An answer to an observe call: anything stamped with the agent's
/// [`Generation`].
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
pub struct Ledger {
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
pub fn observe_context() -> Context {
    let mut ctx = context::current();
    ctx.deadline = Instant::now() + OBSERVE_HOLD + OBSERVE_GRACE;
    ctx
}

/// Run an IPC client loop on a thread of its own.
///
/// The GPUI processes own no async runtime, so their agent client lives on a
/// dedicated OS thread with a current-thread tokio runtime, and results cross
/// back to the GPUI loop over channels. `run` is called on that thread and its
/// future driven to completion there.
///
/// # Errors
///
/// Fails only if the runtime or the thread cannot be created; the caller
/// decides what an app without its agent link does.
pub fn spawn_client_thread<F, Fut>(name: &str, run: F) -> std::io::Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || runtime.block_on(run()))?;
    Ok(())
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

    #[tokio::test]
    async fn a_matching_agent_is_declared_to() {
        let (client, declared) = agent_speaking(PROTOCOL_VERSION);

        establish(client, ClientKind::Overlay)
            .await
            .expect("matching versions establish a client");

        assert_eq!(*declared.lock().unwrap(), [ClientKind::Overlay]);
    }

    #[tokio::test]
    async fn an_older_agent_is_left_undeclared_to() {
        // Declaring is what arms a dormant agent; a client that cannot talk
        // to it must not wake it.
        let (client, declared) = agent_speaking(PROTOCOL_VERSION - 1);

        let Err(error) = establish(client, ClientKind::Gui).await else {
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

        let Err(error) = establish(client, ClientKind::Cli).await else {
            panic!("a newer agent is not usable");
        };

        assert!(matches!(
            error,
            ConnectError::Skew(ProtocolSkew::AgentNewer { agent }) if agent == PROTOCOL_VERSION + 1
        ));
        assert!(declared.lock().unwrap().is_empty());
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
}
