//! The agent link as the loop sees it: one typestate, each phase owning the
//! facts that are true only in that phase.
//!
//! [`Up`] owns the client, the generation [`Ledger`], and the one `observe` in
//! flight — so dropping it cancels the poll, and no answer from a replaced
//! connection can ever be mistaken for the live one's. [`Down`] owns the outage
//! clock and what the GUI has already been told about it, so a fresh outage
//! starts with a clean slate by construction rather than by resetting flags at
//! every reconnect site.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use openlogi_ipc::client::{ConnectError, Ledger, ProtocolSkew, observe_context};
use openlogi_ipc::{AgentClient, AgentSnapshot, Observation};
use tarpc::client::RpcError;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::{Effects, GuiUpdate};

/// How long the client may go without a usable connection before the GUI is
/// told the agent is genuinely unreachable rather than still starting (agent
/// start plus a worst-case first enumeration is ~6 s).
const UNREACHABLE_AFTER: Duration = Duration::from_secs(15);

/// The connection to the agent, or the outage in its place.
pub(super) enum Link {
    Down(Down),
    Up(Up),
}

/// Why there is no connection. The two differ in who else might act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outage {
    /// No connection has ever existed — down since process start. Nobody else
    /// is coming, so the spawn reflex fires at once.
    Cold,
    /// An established connection dropped. Launchd's respawn, the agent's
    /// self-exec, and the tray-Quit deep link all announce themselves within
    /// [`super::reflex::SPAWN_AFTER_LOSS`], so the reflex waits them out.
    Lost,
}

/// A stretch without a usable connection.
pub(super) struct Down {
    pub(super) since: Instant,
    pub(super) outage: Outage,
    /// The last attempt found a live agent *newer* than this GUI. Spawning
    /// cannot help — kickstart is a no-op on a running service and a fresh copy
    /// exits as a duplicate — only a GUI relaunch does.
    pub(super) agent_is_newer: bool,
    /// The last notice the GUI got about this outage. Each goes out when it
    /// becomes true and again only after the other has superseded it, so the
    /// window always shows the current reason and never a stale one.
    told: Option<Notice>,
}

/// What the GUI can be told about an outage; see [`GuiUpdate::Unreachable`]
/// and [`GuiUpdate::OutdatedGui`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
    Unreachable,
    Outdated,
}

impl Down {
    pub(super) fn new(outage: Outage, since: Instant) -> Self {
        Self {
            since,
            outage,
            agent_is_newer: false,
            told: None,
        }
    }

    /// Fold a failed connect attempt in, and say what the GUI is owed for it.
    ///
    /// A newer agent is a fact about this process, told once; the retry that
    /// finds it every quarter second would otherwise spam both the log and the
    /// window.
    pub(super) fn connect_failed(&mut self, error: &ConnectError) -> Option<GuiUpdate> {
        match error {
            ConnectError::Skew(skew @ ProtocolSkew::AgentNewer { .. }) => {
                self.agent_is_newer = true;
                if self.told == Some(Notice::Outdated) {
                    debug!(%skew, "still the stale side");
                    return None;
                }
                warn!(%skew, "this GUI is the stale side — only a relaunch helps");
                self.told = Some(Notice::Outdated);
                Some(GuiUpdate::OutdatedGui)
            }
            error => {
                debug!(%error, "no usable agent");
                self.agent_is_newer = false;
                None
            }
        }
    }

    /// The unreachable notice, after [`UNREACHABLE_AFTER`] of this outage.
    /// Before that the agent may simply be starting — and a newer agent is
    /// not unreachable at all, so the relaunch notice stands while it is live.
    pub(super) fn unreachable_notice(&mut self, now: Instant) -> Option<GuiUpdate> {
        if self.agent_is_newer
            || self.told == Some(Notice::Unreachable)
            || now.saturating_duration_since(self.since) < UNREACHABLE_AFTER
        {
            return None;
        }
        self.told = Some(Notice::Unreachable);
        Some(GuiUpdate::Unreachable)
    }
}

/// A declared, version-matched connection with its long-poll in flight.
pub(super) struct Up {
    pub(super) client: AgentClient,
    ledger: Ledger,
    poll: ObserveFuture,
}

impl Up {
    pub(super) fn new(client: AgentClient) -> Self {
        let ledger = Ledger::new();
        let poll = observe(&client, ledger);
        Self {
            client,
            ledger,
            poll,
        }
    }

    /// Fold an answered poll in and arm the next one. `Some` only for a
    /// genuinely newer snapshot: an equal generation is the hold elapsing as a
    /// heartbeat, and neither that nor a stale reply moves the window back.
    fn answered(&mut self, observation: Observation) -> Option<AgentSnapshot> {
        let fresh = self.ledger.accept(observation);
        self.poll = observe(&self.client, self.ledger);
        fresh.map(|observed| observed.snapshot)
    }
}

/// A long-poll in flight. Boxed because it is stored across loop turns; it
/// owns a clone of the client and dies with the [`Up`] that holds it.
type ObserveFuture = Pin<Box<dyn Future<Output = Result<Observation, RpcError>> + Send>>;

/// Ask for the next state newer than what this connection has seen.
fn observe(client: &AgentClient, ledger: Ledger) -> ObserveFuture {
    let client = client.clone();
    Box::pin(async move { client.observe(observe_context(), ledger.seen()).await })
}

impl Link {
    /// Down since process start.
    pub(super) fn cold(now: Instant) -> Self {
        Self::Down(Down::new(Outage::Cold, now))
    }

    /// The live connection dropped. A no-op while already down: the original
    /// outage keeps its start.
    pub(super) fn lose(&mut self, now: Instant) {
        if matches!(self, Self::Up(_)) {
            *self = Self::Down(Down::new(Outage::Lost, now));
        }
    }

    pub(super) fn is_down(&self) -> bool {
        matches!(self, Self::Down(_))
    }

    pub(super) fn down_mut(&mut self) -> Option<&mut Down> {
        match self {
            Self::Down(down) => Some(down),
            Self::Up(_) => None,
        }
    }

    /// The live client, if any.
    pub(super) fn client(&self) -> Option<&AgentClient> {
        match self {
            Self::Up(up) => Some(&up.client),
            Self::Down(_) => None,
        }
    }

    /// Ensure a connection, connecting on demand. `None` when the attempt
    /// failed — the GUI has then been told whatever that failure means for it.
    pub(super) async fn ensure(
        &mut self,
        effects: &mut impl Effects,
        updates: &mpsc::UnboundedSender<GuiUpdate>,
    ) -> Option<&AgentClient> {
        if let Self::Down(down) = self {
            match effects.connect().await {
                Ok(client) => {
                    debug!("connected to agent IPC socket");
                    *self = Self::Up(Up::new(client));
                }
                Err(error) => {
                    if let Some(notice) = down.connect_failed(&error) {
                        let _ = updates.send(notice);
                    }
                    return None;
                }
            }
        }
        self.client()
    }

    /// The in-flight poll's answer. Pends forever while down, so a select arm
    /// on it is simply inert until there is a connection.
    pub(super) async fn observed(&mut self) -> Result<Observation, RpcError> {
        match self {
            Self::Up(up) => (&mut up.poll).await,
            Self::Down(_) => std::future::pending().await,
        }
    }

    /// Fold an answered poll into the live connection and re-arm it.
    pub(super) fn answered(&mut self, observation: Observation) -> Option<AgentSnapshot> {
        match self {
            Self::Up(up) => up.answered(observation),
            Self::Down(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_ipc::testing::in_memory_agent;

    use super::*;

    fn newer_agent() -> ConnectError {
        ConnectError::Skew(ProtocolSkew::AgentNewer { agent: u32::MAX })
    }

    fn socket_down() -> ConnectError {
        std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into()
    }

    #[test]
    fn a_newer_agent_is_reported_once_per_outage() {
        let mut link = Link::cold(Instant::now());
        let down = link.down_mut().expect("cold is down");

        assert!(matches!(
            down.connect_failed(&newer_agent()),
            Some(GuiUpdate::OutdatedGui)
        ));
        assert!(down.agent_is_newer);
        assert!(
            down.connect_failed(&newer_agent()).is_none(),
            "the retry every quarter second must not repeat the notice"
        );

        // The newer agent going away (quit or replaced) clears the fact, so
        // the spawn reflex may act on the next failed attempt.
        assert!(down.connect_failed(&socket_down()).is_none());
        assert!(!down.agent_is_newer);
    }

    #[test]
    fn a_live_newer_agent_is_never_called_unreachable() {
        let t0 = Instant::now();
        let mut link = Link::cold(t0);
        let down = link.down_mut().expect("cold is down");
        let long_after = t0 + UNREACHABLE_AFTER + UNREACHABLE_AFTER;

        assert!(down.connect_failed(&newer_agent()).is_some());
        assert!(
            down.unreachable_notice(long_after).is_none(),
            "the relaunch notice must not be overwritten while the newer agent is live"
        );

        // Once it is gone the outage is an ordinary one, and when it comes back
        // the relaunch notice is owed again: the window shows the current
        // reason, not the first one.
        assert!(down.connect_failed(&socket_down()).is_none());
        assert!(matches!(
            down.unreachable_notice(long_after),
            Some(GuiUpdate::Unreachable)
        ));
        assert!(matches!(
            down.connect_failed(&newer_agent()),
            Some(GuiUpdate::OutdatedGui)
        ));
    }

    #[test]
    fn the_unreachable_notice_outwaits_a_normal_start_and_fires_once() {
        let t0 = Instant::now();
        let mut link = Link::cold(t0);
        let down = link.down_mut().expect("cold is down");

        assert!(
            down.unreachable_notice(t0 + UNREACHABLE_AFTER.saturating_sub(Duration::from_secs(1)))
                .is_none()
        );
        assert!(matches!(
            down.unreachable_notice(t0 + UNREACHABLE_AFTER),
            Some(GuiUpdate::Unreachable)
        ));
        assert!(
            down.unreachable_notice(t0 + UNREACHABLE_AFTER + Duration::from_secs(1))
                .is_none()
        );
    }

    #[tokio::test]
    async fn losing_the_link_starts_one_fresh_outage() {
        let client = in_memory_agent(|_| Box::pin(std::future::pending()), std::future::pending());
        let mut link = Link::Up(Up::new(client));
        let lost_at = Instant::now();

        link.lose(lost_at);
        let Link::Down(down) = &link else {
            panic!("a lost link is down");
        };
        assert_eq!(down.outage, Outage::Lost);
        assert_eq!(down.since, lost_at);

        link.lose(lost_at + Duration::from_secs(5));
        let Link::Down(down) = &link else {
            panic!("still down");
        };
        assert_eq!(down.since, lost_at, "an outage keeps its original start");
    }
}
