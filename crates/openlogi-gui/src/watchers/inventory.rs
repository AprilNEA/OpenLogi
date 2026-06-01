//! Polling HID inventory watcher.
//!
//! Spawns a dedicated OS thread with a one-shot tokio runtime that watches for
//! device connect/disconnect and forwards a fresh inventory over an unbounded
//! mpsc to the GPUI thread, which applies it via `AppState::refresh_inventories`.
//!
//! Polling beats hot-plug event registration on simplicity: HID transport
//! crates ship different listener APIs across platforms, and `async-hid 0.4`
//! does not expose any.
//!
//! Crucially, each tick first reads a *cheap presence signature*
//! (`openlogi_hid::present_keys`, which lists the HID registry without opening
//! anything) and only runs the full `enumerate` — which opens each device's
//! HID++ channel — when that signature changes. Re-opening a Bluetooth-direct
//! device's channel renegotiates the BLE link and visibly jitters the pointer,
//! so it must happen on connect/disconnect only, never on a bare timer.

use std::thread;
use std::time::Duration;

use openlogi_core::device::DeviceInventory;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Spawn the watcher and return a receiver of inventory snapshots. The
/// channel is unbounded so a slow GUI thread cannot back-pressure the HID
/// poll loop into stalling on a real device disconnect.
///
/// Dropping the receiver shuts the watcher down: the next `send` fails and
/// the loop exits cleanly.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<Vec<DeviceInventory>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawn_result = thread::Builder::new()
        .name("openlogi-inventory-watcher".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, "tokio runtime init failed; watcher exiting");
                    return;
                }
            };
            // `None` until the first successful presence read, so the initial
            // tick always runs a full enumerate and seeds the GUI.
            let mut last_keys: Option<Vec<(u16, u16, u16)>> = None;
            loop {
                match rt.block_on(openlogi_hid::present_keys()) {
                    Ok(keys) if last_keys.as_ref() != Some(&keys) => {
                        // Device set changed (or first run): re-probe + publish.
                        last_keys = Some(keys);
                        let inv = match rt.block_on(openlogi_hid::enumerate()) {
                            Ok(inv) => inv,
                            Err(e) => {
                                warn!(error = ?e, "enumerate failed during watch tick");
                                Vec::new()
                            }
                        };
                        if tx.send(inv).is_err() {
                            debug!("inventory watcher receiver dropped — exiting");
                            return;
                        }
                    }
                    Ok(_) => {} // unchanged — no channel opened, pointer undisturbed
                    Err(e) => warn!(error = ?e, "presence check failed during watch tick"),
                }
                thread::sleep(period);
            }
        });
    if let Err(e) = spawn_result {
        // OS thread limits / fork failures are non-fatal: the GUI can run
        // with the initial enumeration snapshot, just without hot-plug
        // detection. The dropped sender means the receiver immediately
        // closes on its first recv() and the GUI loop falls through.
        warn!(error = %e, "could not spawn inventory watcher — auto-reconnect disabled");
    }
    rx
}
