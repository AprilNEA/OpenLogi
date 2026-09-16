//! On-demand device-pairing watcher.
//!
//! Unlike the polling watchers, this one is event-driven: it idles until the
//! "Add device" window sends [`Control::Start`], then runs a single
//! [`openlogi_hid::run_pairing`] session — forwarding the user's device pick
//! and cancel into it — and streams [`SessionEvent`]s back to the agent.
//! When the session ends it returns to idle, ready for the next open.
//!
//! Keeping the thread long-lived means the consumer's select loop can own one
//! fixed [`SessionEvent`] receiver and one [`Control`] sender (published as a
//! global), instead of wiring a fresh channel on every window open.

use std::future::Future;
use std::thread;

use openlogi_hid::{
    DiscoveredDevice, PairingCommand, PairingError, PairingEvent, ReceiverSelector,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::hardware::HardwareContext;

#[cfg(test)]
mod replay_tests;

/// Commands the UI sends to the pairing watcher.
#[derive(Debug)]
pub enum Control {
    /// Begin a pairing session against the chosen receiver.
    Start {
        /// Identity assigned by the agent when it admits the session.
        session: SessionId,
        /// Receiver the session should open.
        selector: ReceiverSelector,
    },
    /// Bolt: pair with a discovered device.
    Pair {
        /// Session that discovered the device.
        session: SessionId,
        /// Full device data retained inside the agent.
        device: DiscoveredDevice,
    },
    /// Abort the in-progress session.
    Cancel {
        /// Session to cancel.
        session: SessionId,
    },
}

/// Process-local identity for one admitted pairing session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(u64);

impl SessionId {
    /// Construct an identity from the agent's monotonic session counter.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Pairing event tagged with the session that produced it.
#[derive(Debug)]
pub struct SessionEvent {
    /// Session that produced `event`.
    pub session: SessionId,
    /// Progress or terminal result from the HID pairing flow.
    pub event: PairingEvent,
}

/// Spawn the watcher. Returns a sender for [`Control`] messages and a receiver
/// of [`SessionEvent`]s. Dropping the control sender stops the watcher after
/// the current session.
#[must_use]
pub fn spawn() -> (
    mpsc::UnboundedSender<Control>,
    mpsc::UnboundedReceiver<SessionEvent>,
) {
    spawn_with_hardware(HardwareContext::production())
}

/// Spawn the watcher over one coherent hardware context. Production callers
/// normally obtain this context from the agent's shared runtime; tests may
/// inject a replay backend and lifecycle gate.
#[must_use]
pub fn spawn_with_hardware(
    hardware: HardwareContext,
) -> (
    mpsc::UnboundedSender<Control>,
    mpsc::UnboundedReceiver<SessionEvent>,
) {
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();

    let spawn_result = thread::Builder::new()
        .name("openlogi-pairing-watcher".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, "tokio runtime init failed; pairing watcher exiting");
                    return;
                }
            };
            rt.block_on(run(ctrl_rx, evt_tx, hardware));
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn pairing watcher thread");
    }
    (ctrl_tx, evt_rx)
}

/// Idle ↔ session driver. Returns when every [`Control`] sender is dropped.
async fn run(
    ctrl_rx: mpsc::UnboundedReceiver<Control>,
    evt_tx: mpsc::UnboundedSender<SessionEvent>,
    hardware: HardwareContext,
) {
    run_with(
        ctrl_rx,
        evt_tx,
        move |_session, target, commands, events| {
            let hardware = hardware.clone();
            async move { hardware.run_pairing(target, commands, events).await }
        },
    )
    .await;
}

/// The session driver is parameterized so teardown ordering can be tested
/// without opening a host HID device.
async fn run_with<F, Fut>(
    mut ctrl_rx: mpsc::UnboundedReceiver<Control>,
    evt_tx: mpsc::UnboundedSender<SessionEvent>,
    mut start_session: F,
) where
    F: FnMut(
        SessionId,
        ReceiverSelector,
        mpsc::UnboundedReceiver<PairingCommand>,
        mpsc::UnboundedSender<PairingEvent>,
    ) -> Fut,
    Fut: Future<Output = Result<(), PairingError>>,
{
    let mut queued_start = None;
    loop {
        // Idle until a Start arrives; ignore stray in-session commands.
        let (session_id, target) = if let Some(start) = queued_start.take() {
            start
        } else {
            loop {
                match ctrl_rx.recv().await {
                    Some(Control::Start { session, selector }) => break (session, selector),
                    // Stray Pair/Cancel while idle: ignore and keep waiting.
                    Some(_) => {}
                    None => return,
                }
            }
        };

        // One session: a fresh command channel feeds run_pairing while we relay
        // the user's Pair/Cancel into it. Events first land on a session-local
        // channel so the terminal event is not published until `run_pairing`
        // has finished restoring the receiver.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<PairingCommand>();
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
        let mut session = Box::pin(start_session(session_id, target, cmd_rx, raw_tx));
        let mut terminal = None;
        let mut events_open = true;

        let result = loop {
            tokio::select! {
                result = &mut session => break result,
                event = raw_rx.recv(), if events_open => match event {
                    Some(event) => relay_event(session_id, event, &evt_tx, &mut terminal),
                    None => events_open = false,
                },
                ctrl = ctrl_rx.recv() => match ctrl {
                    Some(Control::Pair { session, device }) if session == session_id => {
                        let _ = cmd_tx.send(PairingCommand::Pair(device));
                    }
                    Some(Control::Cancel { session }) if session == session_id => {
                        let _ = cmd_tx.send(PairingCommand::Cancel);
                    }
                    // Carry a start received during the old session's wind-down
                    // into the next idle iteration instead of consuming it.
                    Some(Control::Start { session, selector }) => {
                        if queued_start.is_none() {
                            queued_start = Some((session, selector));
                        } else {
                            warn!(?session, "pairing start received while another start is queued");
                        }
                    }
                    Some(Control::Pair { .. } | Control::Cancel { .. }) => {}
                    // App shutting down: dropping `session` cancels it.
                    None => return,
                },
            }
        };

        // `run_pairing` sends its terminal event before returning. Drain any
        // event that became ready in the same select turn, then publish one
        // terminal result only after receiver restoration has completed.
        while let Ok(event) = raw_rx.try_recv() {
            relay_event(session_id, event, &evt_tx, &mut terminal);
        }
        log_session_end(&result);
        let terminal = terminal.unwrap_or_else(|| {
            warn!(
                ?session_id,
                "pairing session ended without a terminal event"
            );
            PairingEvent::Failed(match result {
                Ok(()) => PairingError::Hid("pairing session ended without a result".to_string()),
                Err(error) => error,
            })
        });
        let _ = evt_tx.send(SessionEvent {
            session: session_id,
            event: terminal,
        });
    }
}

fn relay_event(
    session: SessionId,
    event: PairingEvent,
    events: &mpsc::UnboundedSender<SessionEvent>,
    terminal: &mut Option<PairingEvent>,
) {
    if matches!(event, PairingEvent::Paired { .. } | PairingEvent::Failed(_)) {
        if terminal.is_none() {
            *terminal = Some(event);
        } else {
            warn!(
                ?session,
                "pairing session emitted more than one terminal event"
            );
        }
    } else {
        let _ = events.send(SessionEvent { session, event });
    }
}

fn log_session_end(result: &Result<(), PairingError>) {
    match result {
        Ok(()) => debug!("pairing session ended"),
        Err(e) => debug!(error = %e, "pairing session ended with error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tokio::sync::oneshot;

    #[tokio::test]
    async fn start_received_during_teardown_runs_as_the_next_session() {
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finish_first_tx, finish_first_rx) = oneshot::channel();
        let (finish_second_tx, finish_second_rx) = oneshot::channel();
        let finishes = Arc::new(Mutex::new(VecDeque::from([
            finish_first_rx,
            finish_second_rx,
        ])));

        let driver = tokio::spawn(run_with(ctrl_rx, evt_tx, {
            let finishes = Arc::clone(&finishes);
            move |session, _selector, _commands, events| {
                let finish = finishes
                    .lock()
                    .expect("finish queue lock should not be poisoned")
                    .pop_front()
                    .expect("each fake session has a completion signal");
                let started_tx = started_tx.clone();
                async move {
                    let _ = started_tx.send(session);
                    let error = PairingError::Cancelled;
                    let _ = events.send(PairingEvent::Failed(error.clone()));
                    let _ = finish.await;
                    Err(error)
                }
            }
        }));

        let first = SessionId::new(1);
        let second = SessionId::new(2);
        ctrl_tx
            .send(Control::Start {
                session: first,
                selector: ReceiverSelector::First,
            })
            .expect("watcher control channel should be open");
        assert_eq!(started_rx.recv().await, Some(first));

        ctrl_tx
            .send(Control::Start {
                session: second,
                selector: ReceiverSelector::First,
            })
            .expect("watcher control channel should be open");
        assert!(
            evt_rx.try_recv().is_err(),
            "terminal event must wait for the old session's teardown"
        );

        finish_first_tx
            .send(())
            .expect("first fake session should still be running");
        let first_terminal = evt_rx.recv().await.expect("first terminal event");
        assert_eq!(first_terminal.session, first);
        assert!(matches!(
            first_terminal.event,
            PairingEvent::Failed(PairingError::Cancelled)
        ));
        assert_eq!(started_rx.recv().await, Some(second));

        finish_second_tx
            .send(())
            .expect("second fake session should still be running");
        let second_terminal = evt_rx.recv().await.expect("second terminal event");
        assert_eq!(second_terminal.session, second);
        assert!(matches!(
            second_terminal.event,
            PairingEvent::Failed(PairingError::Cancelled)
        ));

        drop(ctrl_tx);
        driver.await.expect("watcher driver should exit cleanly");
    }
}
