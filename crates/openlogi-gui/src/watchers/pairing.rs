//! On-demand device-pairing watcher.
//!
//! Unlike the polling watchers, this one is event-driven: it idles until the
//! "Add device" window sends [`Control::Start`], then runs a single
//! [`openlogi_hid::run_pairing`] session — forwarding the user's device pick
//! and cancel into it — and streams [`PairingEvent`]s back to the GPUI thread.
//! When the session ends it returns to idle, ready for the next open.
//!
//! Keeping the thread long-lived means the GPUI [`crate::main`] select loop can
//! own one fixed `PairingEvent` receiver and one [`Control`] sender (published
//! as a global), instead of wiring a fresh channel on every window open.

use std::{thread, time::Duration};

use openlogi_hid::{
    DiscoveredDevice, PairingCommand, PairingError, PairingEvent, ReceiverSelector,
    WindowsPairingDevice, list_windows_pairing_devices, pair_windows_device, run_pairing,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Commands the UI sends to the pairing watcher.
#[derive(Debug)]
pub enum Control {
    /// Begin a pairing session against the chosen receiver.
    Start(ReceiverSelector),
    /// Enumerate devices through Windows Bluetooth pairing.
    StartWindows,
    /// Bolt: pair with a discovered device.
    Pair(DiscoveredDevice),
    /// Pair with a Windows Bluetooth candidate.
    PairWindows(WindowsPairingDevice),
    /// Abort the in-progress session.
    Cancel,
}

enum SessionRequest {
    Receiver(ReceiverSelector),
    Windows,
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
        // Idle until a start command arrives; ignore stray in-session commands.
        let request = loop {
            match ctrl_rx.recv().await {
                Some(Control::Start(target)) => {
                    info!(?target, "pairing start requested");
                    break SessionRequest::Receiver(target);
                }
                Some(Control::StartWindows) => {
                    info!("Windows Bluetooth pairing start requested");
                    break SessionRequest::Windows;
                }
                // Stray Pair/Cancel while idle: ignore and keep waiting.
                Some(control) => debug!(?control, "ignoring pairing control while idle"),
                None => {
                    debug!("pairing control channel closed; watcher exiting");
                    return;
                }
            }
        };

        let keep_running = match request {
            SessionRequest::Receiver(target) => {
                run_receiver_session(target, &mut ctrl_rx, evt_tx.clone()).await
            }
            SessionRequest::Windows => run_windows_session(&mut ctrl_rx, evt_tx.clone()).await,
        };
        if !keep_running {
            return;
        }
    }
}

async fn run_receiver_session(
    target: ReceiverSelector,
    ctrl_rx: &mut mpsc::UnboundedReceiver<Control>,
    evt_tx: mpsc::UnboundedSender<PairingEvent>,
) -> bool {
    // One session: a fresh command channel feeds run_pairing while we relay
    // the user's Pair/Cancel into it, racing against the session finishing.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<PairingCommand>();
    info!(?target, "pairing session spawned");
    let mut session = Box::pin(run_pairing(target, cmd_rx, evt_tx));

    loop {
        tokio::select! {
            result = &mut session => {
                log_session_end(&result);
                return true;
            }
            ctrl = ctrl_rx.recv() => match ctrl {
                Some(Control::Pair(device)) => {
                    info!(name = %device.name, "pairing device selected");
                    if cmd_tx.send(PairingCommand::Pair(device)).is_err() {
                        return true;
                    }
                }
                Some(Control::Cancel) => {
                    info!("pairing cancel requested");
                    let _ = cmd_tx.send(PairingCommand::Cancel);
                }
                // Already mid-session; a second Start is a no-op.
                Some(Control::Start(_) | Control::StartWindows) => {
                    debug!("ignoring duplicate pairing start");
                }
                Some(Control::PairWindows(device)) => {
                    debug!(name = %device.name, "ignoring Windows device during receiver session");
                }
                // App shutting down: dropping `session` cancels it.
                None => {
                    debug!("pairing control channel closed during session");
                    return false;
                }
            },
        }
    }
}

async fn run_windows_session(
    ctrl_rx: &mut mpsc::UnboundedReceiver<Control>,
    evt_tx: mpsc::UnboundedSender<PairingEvent>,
) -> bool {
    let _ = evt_tx.send(PairingEvent::WindowsSearching);
    let devices = match list_windows_pairing_devices().await {
        Ok(devices) => devices,
        Err(error) => {
            let _ = evt_tx.send(PairingEvent::Failed(PairingError::Windows(
                error.to_string(),
            )));
            return true;
        }
    };
    if devices.is_empty() {
        let _ = evt_tx.send(PairingEvent::Failed(PairingError::Windows(
            "No Windows Bluetooth pairing candidates were found.".into(),
        )));
        return true;
    }
    for device in devices {
        let _ = evt_tx.send(PairingEvent::WindowsDeviceFound(device));
    }

    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => {
                let _ = evt_tx.send(PairingEvent::Failed(PairingError::Timeout));
                return true;
            }
            ctrl = ctrl_rx.recv() => match ctrl {
                Some(Control::PairWindows(device)) => {
                    let name = device.name.clone();
                    info!(name = %name, "Windows Bluetooth device selected");
                    let _ = evt_tx.send(PairingEvent::WindowsPairing { name: name.clone() });
                    match pair_windows_device(device.id).await {
                        Ok(outcome) if outcome.succeeded() => {
                            let _ = evt_tx.send(PairingEvent::WindowsPaired {
                                name: outcome.device.name,
                                status: outcome.status,
                            });
                        }
                        Ok(outcome) => {
                            let _ =
                                evt_tx.send(PairingEvent::Failed(PairingError::WindowsStatus(
                                    outcome.status,
                                )));
                        }
                        Err(error) => {
                            let _ = evt_tx.send(PairingEvent::Failed(PairingError::Windows(
                                error.to_string(),
                            )));
                        }
                    }
                    return true;
                }
                Some(Control::Cancel) => {
                    info!("Windows Bluetooth pairing cancel requested");
                    let _ = evt_tx.send(PairingEvent::Failed(PairingError::Cancelled));
                    return true;
                }
                Some(Control::Start(_) | Control::StartWindows) => {
                    debug!("ignoring duplicate pairing start");
                }
                Some(Control::Pair(device)) => {
                    debug!(name = %device.name, "ignoring receiver device during Windows session");
                }
                None => {
                    debug!("pairing control channel closed during Windows session");
                    return false;
                }
            },
        }
    }
}

fn log_session_end(result: &Result<(), PairingError>) {
    match result {
        Ok(()) => info!("pairing session ended"),
        Err(e) => info!(error = %e, "pairing session ended with error"),
    }
}
