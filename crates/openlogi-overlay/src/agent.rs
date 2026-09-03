//! The agent side: connecting, receiving ring invocations, and reporting what
//! the user does with them.
//!
//! Reporting is not fire-and-forget. A hover that never lands leaves the ring
//! showing the wrong slot, and a dropped activation loses the user's click
//! outright — so terminal commands retry until the session's own deadline. A
//! newer command for the same ring supersedes a stalled one rather than
//! queueing behind it, which is what keeps the ring responsive when the agent
//! is briefly slow.

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use openlogi_core::action_ring::DISPLAY_LIFETIME;
use openlogi_core::binding::ActionRingSlot;
use openlogi_ipc::{
    ActionRingInvocation, AgentClient, ClientKind, Generation, OBSERVE_HOLD, PROTOCOL_VERSION,
};
use succession::Standing;
use tarpc::context;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::session::allegiance;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayCommand {
    Hover {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Activate {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Cancel {
        session_id: u64,
    },
}

impl OverlayCommand {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Activate { .. } | Self::Cancel { .. })
    }
}

pub(crate) struct Ipc {
    /// The ring the agent says should be showing, each time that changes.
    /// `None` is no ring — including a dismissal, which is why there is no
    /// separate "close" message to recognise.
    pub(crate) invocations: mpsc::UnboundedReceiver<Option<ActionRingInvocation>>,
    /// Where the view reports hover, activation, and cancellation.
    pub(crate) commands: mpsc::UnboundedSender<OverlayCommand>,
}

pub(crate) fn spawn_ipc() -> Ipc {
    let (invocation_tx, invocations) = mpsc::unbounded_channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "overlay IPC runtime initialization failed");
                return;
            }
        };
        runtime.block_on(async move {
            tokio::join!(
                poll_invocations(invocation_tx),
                send_commands(&mut command_rx)
            );
        });
    });
    Ipc {
        invocations,
        commands,
    }
}

async fn connect() -> Option<AgentClient> {
    let connection = openlogi_ipc::client::connect().await.ok()?;
    // A mismatch in either direction is not transient — this binary is from a
    // superseded install, and the agent will start the overlay that matches it
    // as soon as this one releases the role.
    if connection.version != PROTOCOL_VERSION {
        stand_down(&format!(
            "agent speaks protocol {} and this overlay speaks {PROTOCOL_VERSION}",
            connection.version
        ));
    }
    let client = connection.client;
    // Declare before anything else: an overlay reconnecting on its own is an
    // orphan of a previous run and must not wake a dormant agent — an armed
    // agent spawns its own overlay.
    client
        .declare_client(context::current(), ClientKind::Overlay)
        .await
        .ok()?;
    let identity = client.identity(context::current()).await.ok()?;
    if let Standing::Superseded(because) = allegiance().observe(identity) {
        stand_down(&because.to_string());
    }
    Some(client)
}

/// Exit, releasing the overlay role.
///
/// Staying alive is worse than useless in every case that reaches here. A
/// superseded helper cannot serve a ring it can no longer be asked about, and
/// its claim on the role is exactly what stops the agent's supervisor from
/// starting the one that can; an orphaned helper has no agent to serve at all.
#[expect(
    clippy::exit,
    reason = "the IPC tasks run off the GPUI main thread and cannot return a status to `main`, which is parked in the application run loop; releasing the role by exiting is the point"
)]
pub(crate) fn stand_down(because: &str) -> ! {
    info!("{because} — exiting and releasing the Actions Ring overlay role");
    std::process::exit(0)
}

/// How long to keep reaching for an agent before giving up and exiting.
///
/// Nothing else bounds this process's life. The agent starts it detached and
/// never kills it — the menu-bar Quit is a `process::exit`, which runs no
/// destructors — and every other exit path here needs a *replacement* agent to
/// answer. So an agent that stops for good leaves its helper reconnecting
/// forever; one was found still at it three days on. Giving up costs almost
/// nothing: the helper only exists to keep process-start latency off the first
/// ring press, which is worth exactly zero while no agent is running, and the
/// supervisor starts a fresh one within its restart backoff once an agent is
/// back.
const GIVE_UP_AFTER: Duration = Duration::from_mins(1);

/// How long to wait between attempts to reach an agent.
const RETRY_PERIOD: Duration = Duration::from_secs(1);

/// Invocation observer phase and the state meaningful within each phase.
///
/// A successful connection owns its generation cursor; a reconnect episode
/// owns its give-up clock. Transitioning between them resets the fact from the
/// previous phase by construction.
enum InvocationPollState<C> {
    Reconnecting { unreachable_since: Option<Instant> },
    Observing { client: C, seen: Generation },
}

impl<C> Default for InvocationPollState<C> {
    fn default() -> Self {
        Self::Reconnecting {
            unreachable_since: None,
        }
    }
}

impl<C> InvocationPollState<C> {
    fn connected(&mut self, client: C) {
        *self = Self::Observing { client, seen: 0 };
    }

    fn connection_failed(&mut self, now: Instant) -> bool {
        let Self::Reconnecting { unreachable_since } = self else {
            return false;
        };
        let armed = *unreachable_since.get_or_insert(now);
        now.duration_since(armed) >= GIVE_UP_AFTER
    }

    fn observation(&self) -> Option<(&C, Generation)> {
        match self {
            Self::Observing { client, seen } => Some((client, *seen)),
            Self::Reconnecting { .. } => None,
        }
    }

    fn observed(&mut self, generation: Generation) {
        if let Self::Observing { seen, .. } = self {
            *seen = generation;
        }
    }

    fn disconnected(&mut self) {
        *self = Self::Reconnecting {
            unreachable_since: None,
        };
    }
}

async fn poll_invocations(tx: mpsc::UnboundedSender<Option<ActionRingInvocation>>) {
    let mut state = InvocationPollState::default();
    loop {
        if matches!(&state, InvocationPollState::Reconnecting { .. }) {
            if let Some(client) = connect().await {
                // Generation 0 says "I have seen nothing", so the first
                // answer is whatever is showing right now. A replacement
                // agent numbers its own generations, so every connection
                // starts there independently.
                state.connected(client);
            } else {
                if state.connection_failed(Instant::now()) {
                    stand_down(&format!("no agent has answered for {GIVE_UP_AFTER:?}"));
                }
                tokio::time::sleep(RETRY_PERIOD).await;
                continue;
            }
        }
        let Some((active, seen)) = state.observation() else {
            continue;
        };
        let mut ctx = context::current();
        // Above the agent's hold, or tarpc would cancel the handler mid-wait.
        ctx.deadline = std::time::Instant::now() + OBSERVE_HOLD + Duration::from_secs(5);
        match active.observe_action_ring(ctx, seen).await {
            Ok(observed) => {
                if observed.generation == seen {
                    continue; // the hold elapsed: still alive, still nothing new
                }
                state.observed(observed.generation);
                if tx.send(observed.invocation).is_err() {
                    return;
                }
            }
            Err(error) => {
                debug!(?error, "Actions Ring state channel disconnected");
                state.disconnected();
            }
        }
    }
}

/// Fold a newly-produced command into the one still waiting to be sent.
///
/// A hover is dropped once its own session has already been activated or
/// cancelled — the buzz would be for a ring that is closing. It must be the
/// *same* session though: rings open back to back, and the view only emits a
/// hover when the hovered slot changes, so swallowing the new ring's first
/// hover loses it for as long as the pointer stays where it is.
fn coalesce_command(current: OverlayCommand, next: OverlayCommand) -> OverlayCommand {
    match (next, current) {
        (
            OverlayCommand::Hover { session_id, .. },
            OverlayCommand::Activate {
                session_id: closing,
                ..
            }
            | OverlayCommand::Cancel {
                session_id: closing,
            },
        ) if session_id == closing => current,
        _ => next,
    }
}

type CommandFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

async fn send_command(client: &AgentClient, command: OverlayCommand) -> bool {
    let ctx = context::current();
    match command {
        OverlayCommand::Hover { session_id, slot } => client
            .action_ring_hover(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Activate { session_id, slot } => client
            .action_ring_activate(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Cancel { session_id } => {
            client.action_ring_cancel(ctx, session_id).await.is_ok()
        }
    }
}

async fn send_commands(rx: &mut mpsc::UnboundedReceiver<OverlayCommand>) {
    send_commands_with(
        rx,
        || Box::pin(connect()),
        |client, command| Box::pin(send_command(client, command)),
    )
    .await;
}

async fn send_commands_with<C>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    mut connect_client: impl FnMut() -> CommandFuture<'static, Option<C>>,
    mut send: impl for<'a> FnMut(&'a C, OverlayCommand) -> CommandFuture<'a, bool>,
) {
    let mut client = None;
    while let Some(mut command) = rx.recv().await {
        while let Ok(next) = rx.try_recv() {
            command = coalesce_command(command, next);
        }
        let mut deadline = command_deadline(command);
        loop {
            while let Ok(next) = rx.try_recv() {
                (command, deadline) = merge_pending(command, deadline, next);
            }
            if client.is_none() {
                match await_command_attempt(rx, command, deadline, connect_client()).await {
                    CommandAttempt::Completed(connected) => client = connected,
                    CommandAttempt::Superseded(next, next_deadline) => {
                        command = next;
                        deadline = next_deadline;
                        continue;
                    }
                    CommandAttempt::Expired => break,
                    CommandAttempt::Closed => return,
                }
            }
            let Some(active) = client.as_ref() else {
                let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await
                else {
                    break;
                };
                command = next;
                deadline = next_deadline;
                continue;
            };
            match await_command_attempt(rx, command, deadline, send(active, command)).await {
                CommandAttempt::Completed(false) => client = None,
                CommandAttempt::Superseded(next, next_deadline) => {
                    command = next;
                    deadline = next_deadline;
                    continue;
                }
                CommandAttempt::Completed(true) | CommandAttempt::Expired => break,
                CommandAttempt::Closed => return,
            }
            let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await else {
                break;
            };
            command = next;
            deadline = next_deadline;
        }
    }
}

#[derive(Debug)]
enum CommandAttempt<T> {
    Completed(T),
    Superseded(OverlayCommand, Option<Instant>),
    Expired,
    Closed,
}

async fn await_command_attempt<T>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
    attempt: impl Future<Output = T>,
) -> CommandAttempt<T> {
    tokio::pin!(attempt);
    loop {
        tokio::select! {
            result = &mut attempt => return CommandAttempt::Completed(result),
            next = rx.recv() => {
                let Some(next) = next else {
                    return CommandAttempt::Closed;
                };
                let mut pending = merge_pending(command, deadline, next);
                while let Ok(next) = rx.try_recv() {
                    pending = merge_pending(pending.0, pending.1, next);
                }
                if pending.0 != command {
                    return CommandAttempt::Superseded(pending.0, pending.1);
                }
            }
            () = deadline_elapsed(deadline) => return CommandAttempt::Expired,
        }
    }
}

async fn deadline_elapsed(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline.into()).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn command_deadline(command: OverlayCommand) -> Option<Instant> {
    command
        .is_terminal()
        .then(|| Instant::now() + DISPLAY_LIFETIME)
}

fn merge_pending(
    command: OverlayCommand,
    deadline: Option<Instant>,
    next: OverlayCommand,
) -> (OverlayCommand, Option<Instant>) {
    let pending = coalesce_command(command, next);
    let deadline = if pending == command {
        deadline
    } else {
        command_deadline(pending)
    };
    (pending, deadline)
}

async fn wait_for_retry(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
) -> Option<(OverlayCommand, Option<Instant>)> {
    if !retry_before(deadline) {
        return None;
    }
    tokio::select! {
        () = tokio::time::sleep(Duration::from_millis(100)) => Some((command, deadline)),
        next = rx.recv() => {
            let mut pending = merge_pending(command, deadline, next?);
            while let Ok(next) = rx.try_recv() {
                pending = merge_pending(pending.0, pending.1, next);
            }
            Some(pending)
        }
    }
}

fn retry_before(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() < deadline)
}

#[cfg(test)]
mod tests;
