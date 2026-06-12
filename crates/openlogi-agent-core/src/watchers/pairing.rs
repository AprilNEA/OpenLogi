//! On-demand device-pairing watcher.
//!
//! Unlike the polling watchers, this one is event-driven: it idles until the
//! "Add device" window sends [`Control::Start`], then runs a single
//! [`openlogi_hid::run_pairing`] session — forwarding the user's device pick
//! and cancel into it — and streams [`PairingEvent`]s back to the GPUI thread.
//! When the session ends it returns to idle, ready for the next open.
//!
//! Keeping the thread long-lived means the consumer's select loop can own one
//! fixed `PairingEvent` receiver and one [`Control`] sender (published as a
//! global), instead of wiring a fresh channel on every window open.

use std::thread;

use openlogi_hid::{
    DiscoveredDevice, PairingCommand, PairingError, PairingEvent, ReceiverSelector, run_pairing,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Commands the UI sends to the pairing watcher.
#[derive(Debug)]
pub enum Control {
    /// Begin a pairing session against the chosen receiver.
    Start(ReceiverSelector),
    /// Bolt: pair with a discovered device.
    Pair(DiscoveredDevice),
    /// Abort the in-progress session.
    Cancel,
}

/// Spawn the watcher. Returns a sender for [`Control`] messages and a receiver
/// of [`PairingEvent`]s. Dropping the control sender stops the watcher after
/// the current session.
#[must_use]
pub fn spawn() -> (
    mpsc::UnboundedSender<Control>,
    mpsc::UnboundedReceiver<PairingEvent>,
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
            rt.block_on(run(ctrl_rx, evt_tx));
        });
    if let Err(e) = spawn_result {
        warn!(error = %e, "could not spawn pairing watcher thread");
    }
    (ctrl_tx, evt_rx)
}

/// Idle ↔ session driver. Returns when every [`Control`] sender is dropped.
async fn run(
    mut ctrl_rx: mpsc::UnboundedReceiver<Control>,
    evt_tx: mpsc::UnboundedSender<PairingEvent>,
) {
    loop {
        // Idle until a Start arrives; ignore stray in-session commands.
        let target = loop {
            match ctrl_rx.recv().await {
                Some(Control::Start(target)) => break target,
                // Stray Pair/Cancel while idle: ignore and keep waiting.
                Some(_) => {}
                None => return,
            }
        };

        // One session: a fresh command channel feeds run_pairing while we relay
        // the user's Pair/Cancel into it, racing against the session finishing.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<PairingCommand>();
        let mut session = Box::pin(run_pairing(target, cmd_rx, evt_tx.clone()));

        loop {
            tokio::select! {
                result = &mut session => {
                    log_session_end(&result);
                    break;
                }
                ctrl = ctrl_rx.recv() => match ctrl {
                    Some(Control::Pair(device)) => {
                        if cmd_tx.send(PairingCommand::Pair(device)).is_err() {
                            break;
                        }
                    }
                    Some(Control::Cancel) => {
                        let _ = cmd_tx.send(PairingCommand::Cancel);
                    }
                    // Already mid-session; a second Start is a no-op.
                    Some(Control::Start(_)) => {}
                    // App shutting down: dropping `session` cancels it.
                    None => return,
                },
            }
        }
    }
}

fn log_session_end(result: &Result<(), PairingError>) {
    match result {
        Ok(()) => debug!("pairing session ended"),
        Err(e) => debug!(error = %e, "pairing session ended with error"),
    }
}
