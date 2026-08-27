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

/// Record a failed attempt to reach an agent, and say whether to give up.
///
/// `unreachable_since` is armed by the first failure of a run and cleared by
/// the caller on every success, so a helper whose agent keeps flapping back
/// within the deadline never accumulates its way to an exit.
fn give_up(unreachable_since: &mut Option<Instant>, now: Instant) -> bool {
    let armed = *unreachable_since.get_or_insert(now);
    now.duration_since(armed) >= GIVE_UP_AFTER
}

async fn poll_invocations(tx: mpsc::UnboundedSender<Option<ActionRingInvocation>>) {
    let mut client = None;
    // Generation 0 says "I have seen nothing", so the first answer is whatever
    // is showing right now — an overlay restarted mid-ring paints the live one
    // instead of having missed its invocation. Reset on every disconnect: the
    // replacement agent numbers its own generations.
    let mut seen: Generation = 0;
    // Armed while no agent is answering; see `give_up`.
    let mut unreachable_since: Option<Instant> = None;
    loop {
        if client.is_none() {
            client = connect().await;
            seen = 0;
        }
        let Some(active) = client.as_ref() else {
            if give_up(&mut unreachable_since, Instant::now()) {
                stand_down(&format!("no agent has answered for {GIVE_UP_AFTER:?}"));
            }
            tokio::time::sleep(RETRY_PERIOD).await;
            continue;
        };
        unreachable_since = None;
        let mut ctx = context::current();
        // Above the agent's hold, or tarpc would cancel the handler mid-wait.
        ctx.deadline = std::time::Instant::now() + OBSERVE_HOLD + Duration::from_secs(5);
        match active.observe_action_ring(ctx, seen).await {
            Ok(observed) => {
                if observed.generation == seen {
                    continue; // the hold elapsed: still alive, still nothing new
                }
                seen = observed.generation;
                if tx.send(observed.invocation).is_err() {
                    return;
                }
            }
            Err(error) => {
                debug!(?error, "Actions Ring state channel disconnected");
                client = None;
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
mod tests {
    use super::*;

    #[test]
    fn the_first_failed_attempt_only_arms_the_give_up_clock() {
        let mut since = None;
        let now = Instant::now();
        assert!(!give_up(&mut since, now));
        assert_eq!(since, Some(now));
    }

    #[test]
    fn an_agent_that_stays_away_past_the_deadline_ends_the_overlay() {
        let start = Instant::now();
        let mut since = None;
        assert!(!give_up(&mut since, start));
        assert!(!give_up(&mut since, start + GIVE_UP_AFTER / 2));
        assert!(give_up(&mut since, start + GIVE_UP_AFTER));
    }

    #[test]
    fn an_agent_that_keeps_coming_back_never_accumulates_its_way_to_an_exit() {
        let start = Instant::now();
        let mut since = None;
        // Each round the agent is gone for half the deadline, then answers —
        // which is what clears the clock at the call site. Five rounds is two
        // and a half deadlines in total, so a version that kept the clock
        // running across outages would have exited by the third.
        for round in 1..=5 {
            let gone = start + (GIVE_UP_AFTER / 2) * round;
            assert!(
                !give_up(&mut since, gone),
                "a reachable agent must not inherit the previous outage's clock"
            );
            since = None;
        }
    }

    #[test]
    fn activation_takes_priority_over_queued_hover_updates() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let activation = OverlayCommand::Activate {
            session_id: 1,
            slot: ActionRingSlot::Right,
        };
        assert!(matches!(
            coalesce_command(hover, activation),
            OverlayCommand::Activate {
                slot: ActionRingSlot::Right,
                ..
            }
        ));
        assert!(matches!(
            coalesce_command(activation, hover),
            OverlayCommand::Activate { .. }
        ));
    }

    /// Rings open back to back — dismiss one, open the next — and the view
    /// only emits a hover when the hovered slot *changes*. Swallowing the new
    /// ring's first hover therefore loses it entirely for as long as the
    /// pointer stays put: no hover buzz, and the agent believing nothing is
    /// hovered.
    #[test]
    fn a_new_sessions_hover_survives_the_previous_sessions_dismissal() {
        let closing = OverlayCommand::Cancel { session_id: 1 };
        let hover = OverlayCommand::Hover {
            session_id: 2,
            slot: ActionRingSlot::Top,
        };

        assert!(matches!(
            coalesce_command(closing, hover),
            OverlayCommand::Hover { session_id: 2, .. }
        ));
    }

    #[tokio::test]
    async fn newer_activation_supersedes_a_stale_retry_immediately() {
        let stale = OverlayCommand::Cancel { session_id: 1 };
        let replacement = OverlayCommand::Activate {
            session_id: 2,
            slot: ActionRingSlot::Right,
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(replacement).unwrap();

        let (pending, _) = tokio::time::timeout(
            Duration::from_millis(20),
            wait_for_retry(&mut rx, stale, Some(Instant::now() + DISPLAY_LIFETIME)),
        )
        .await
        .expect("queued replacement should interrupt the retry delay")
        .expect("replacement command should remain pending");

        assert_eq!(pending, replacement);
    }

    #[tokio::test]
    async fn newer_activation_supersedes_a_stalled_terminal_request() {
        let stale = OverlayCommand::Cancel { session_id: 1 };
        let replacement = OverlayCommand::Activate {
            session_id: 2,
            slot: ActionRingSlot::Right,
        };
        let stale_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let replacement_sent = std::sync::Arc::new(tokio::sync::Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let stale_started = std::sync::Arc::clone(&stale_started);
            let replacement_sent = std::sync::Arc::clone(&replacement_sent);
            async move {
                send_commands_with(
                    &mut rx,
                    || Box::pin(async { Some(()) }),
                    move |(), command| {
                        let stale_started = std::sync::Arc::clone(&stale_started);
                        let replacement_sent = std::sync::Arc::clone(&replacement_sent);
                        Box::pin(async move {
                            if command == stale {
                                stale_started.notify_one();
                                std::future::pending().await
                            } else {
                                replacement_sent.notify_one();
                                true
                            }
                        })
                    },
                )
                .await;
            }
        });

        tx.send(stale).unwrap();
        tokio::time::timeout(Duration::from_millis(100), stale_started.notified())
            .await
            .expect("stale request should start");
        tx.send(replacement).unwrap();
        tokio::time::timeout(Duration::from_millis(100), replacement_sent.notified())
            .await
            .expect("replacement should cancel the stalled request");
        drop(tx);
        tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .expect("command worker should stop")
            .expect("command worker should not panic");
    }

    #[tokio::test]
    async fn stalled_hover_stops_when_the_command_channel_closes() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let request_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let request_started = std::sync::Arc::clone(&request_started);
            async move {
                send_commands_with(
                    &mut rx,
                    || Box::pin(async { Some(()) }),
                    move |(), _| {
                        let request_started = std::sync::Arc::clone(&request_started);
                        Box::pin(async move {
                            request_started.notify_one();
                            std::future::pending().await
                        })
                    },
                )
                .await;
            }
        });

        tx.send(hover).unwrap();
        tokio::time::timeout(Duration::from_millis(100), request_started.notified())
            .await
            .expect("hover request should start");
        drop(tx);
        tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .expect("closing the channel should stop the command worker")
            .expect("command worker should not panic");
    }

    #[test]
    fn only_terminal_commands_are_retryable() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let activation = OverlayCommand::Activate {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let cancellation = OverlayCommand::Cancel { session_id: 1 };
        assert!(!hover.is_terminal());
        assert!(activation.is_terminal());
        assert!(cancellation.is_terminal());
    }

    #[test]
    fn terminal_retries_last_only_until_the_session_deadline() {
        assert!(retry_before(Some(Instant::now() + Duration::from_secs(1))));
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert!(!retry_before(Some(past)));
        assert!(!retry_before(None));
    }
}
