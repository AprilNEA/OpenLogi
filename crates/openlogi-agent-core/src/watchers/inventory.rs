//! Polling HID inventory watcher.
//!
//! Spawns a dedicated OS thread with a one-shot tokio runtime that calls
//! `openlogi_hid::enumerate` every `period` and forwards each completed
//! snapshot over an unbounded mpsc to the agent's select loop, which applies
//! it via `Orchestrator::refresh_inventory`.
//!
//! Polling beats hot-plug event registration on simplicity: HID transport
//! crates ship different listener APIs across platforms, and `async-hid 0.4`
//! does not expose any. A 2 s tick is cheap (one HID enumerate per cycle ≤
//! a few hundred milliseconds) and matches the human-perceptible reconnect
//! latency budget in PLAN.md.

use std::thread;
use std::time::Duration;

use openlogi_core::device::DeviceInventory;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Consecutive *initial* enumerate failures before the watcher declares
/// enumeration [`InventoryEvent::Unavailable`]. Only counts before the first
/// success: a mid-session failure keeps the last good snapshot instead (see
/// the error arm below), and a later success upgrades `Unavailable` back to a
/// live inventory.
const INITIAL_FAILURE_LIMIT: u8 = 3;

/// What the watcher tells the agent.
pub enum InventoryEvent {
    /// A completed enumeration — empty means "checked, no devices".
    Snapshot(Vec<DeviceInventory>),
    /// Enumeration has never succeeded and won't be treated as "still
    /// starting" any longer; without this the GUI would show its scanning
    /// state forever on a broken HID backend.
    Unavailable,
}

/// Spawn the watcher and return a receiver of inventory events. The
/// channel is unbounded so a slow consumer cannot back-pressure the HID
/// poll loop into stalling on a real device disconnect.
///
/// Dropping the receiver shuts the watcher down: the next `send` fails and
/// the loop exits cleanly. The watcher dying instead (a panic inside the HID
/// backend) closes the channel — the agent select loop maps that closure to
/// `Unavailable` too.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<InventoryEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    let worker_tx = tx.clone();
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
            // A persistent enumerator so its per-device probe cache survives
            // across ticks — a known device's immutable data (model, features)
            // is reused instead of being re-handshaked every poll.
            let mut enumerator = openlogi_hid::Enumerator::default();
            let mut succeeded = false;
            let mut initial_failures: u8 = 0;
            loop {
                match rt.block_on(enumerator.enumerate()) {
                    Ok(inv) => {
                        succeeded = true;
                        if worker_tx.send(InventoryEvent::Snapshot(inv)).is_err() {
                            debug!("inventory watcher receiver dropped — exiting");
                            return;
                        }
                    }
                    // A failed enumerate means "couldn't check", NOT "no devices":
                    // skip the tick so the agent keeps its last good device set
                    // and live bindings instead of wiping them for ~one period. A
                    // genuine disconnect comes back as an `Ok` empty snapshot,
                    // which we DO forward. Before the *first* success there is no
                    // good set to keep, so persistent failure is reported once —
                    // the loop keeps retrying, and a later success recovers.
                    Err(e) => {
                        warn!(error = ?e, "enumerate failed during watch tick — keeping last snapshot");
                        if !succeeded {
                            initial_failures = initial_failures.saturating_add(1);
                            if initial_failures == INITIAL_FAILURE_LIMIT
                                && worker_tx.send(InventoryEvent::Unavailable).is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                thread::sleep(period);
            }
        });
    if let Err(e) = spawn_result {
        // OS thread / fork limits are non-fatal for the agent as a whole, but
        // enumeration will never run. Say so — sending an empty *snapshot*
        // here would forge a "checked, no devices" answer for a check that
        // never happened.
        warn!(error = %e, "could not spawn inventory watcher — device scanning unavailable");
        let _ = tx.send(InventoryEvent::Unavailable);
    }
    rx
}
