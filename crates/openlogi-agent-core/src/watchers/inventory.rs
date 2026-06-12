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

/// The watcher's cross-tick memory, factored out of the poll loop so the
/// tick → event decision is unit-testable without spawning the thread or
/// touching real HID.
#[derive(Default)]
struct WatchState {
    /// Set once any enumeration has completed. After that, a failed tick keeps
    /// the last good snapshot forever instead of ever reporting `Unavailable`.
    succeeded: bool,
    /// Consecutive failures, counted only before the first success.
    initial_failures: u8,
}

impl WatchState {
    /// Decide what (if anything) a watch tick emits.
    ///
    /// - `Ok(snapshot)` — a completed enumeration (an empty one included: that's
    ///   a genuine disconnect) — is forwarded so the agent's device set tracks
    ///   reality.
    /// - `Err(..)` means "couldn't fully check" — a transient HID++ timeout or a
    ///   partial receiver read ([`openlogi_hid::InventoryError::Incomplete`]):
    ///   emit nothing, so the agent keeps its last good device set and live
    ///   bindings instead of wiping them for ~one period — the flap behind #218.
    ///   Before the *first* success there is no good set to keep, so persistent
    ///   initial failure is reported once as [`InventoryEvent::Unavailable`]; the
    ///   loop keeps retrying and a later success recovers.
    fn classify(
        &mut self,
        result: Result<Vec<DeviceInventory>, openlogi_hid::InventoryError>,
    ) -> Option<InventoryEvent> {
        match result {
            Ok(inv) => {
                self.succeeded = true;
                Some(InventoryEvent::Snapshot(inv))
            }
            Err(e) => {
                warn!(error = ?e, "enumerate failed during watch tick — keeping last snapshot");
                if self.succeeded {
                    return None;
                }
                self.initial_failures = self.initial_failures.saturating_add(1);
                (self.initial_failures == INITIAL_FAILURE_LIMIT)
                    .then_some(InventoryEvent::Unavailable)
            }
        }
    }
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
            let mut state = WatchState::default();
            loop {
                let result = rt.block_on(enumerator.enumerate());
                if let Some(event) = state.classify(result)
                    && worker_tx.send(event).is_err()
                {
                    debug!("inventory watcher receiver dropped — exiting");
                    return;
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

#[cfg(test)]
mod tests {
    use openlogi_hid::InventoryError;

    use super::{INITIAL_FAILURE_LIMIT, InventoryEvent, WatchState};

    #[test]
    fn completed_enumeration_is_forwarded_even_when_empty() {
        let mut state = WatchState::default();
        // A genuine "checked, nothing there" still propagates as a disconnect —
        // the resilience must not swallow a real empty.
        assert!(matches!(
            state.classify(Ok(vec![])),
            Some(InventoryEvent::Snapshot(snap)) if snap.is_empty()
        ));
        assert!(state.succeeded);
    }

    #[test]
    fn incomplete_read_after_a_success_keeps_the_last_snapshot() {
        let mut state = WatchState::default();
        // A good tick first, so there is a last-known-good set to preserve.
        assert!(matches!(
            state.classify(Ok(vec![])),
            Some(InventoryEvent::Snapshot(_))
        ));
        // Then transient partial reads emit nothing — the agent keeps the last
        // snapshot instead of flapping to "No devices" (#218).
        assert!(state.classify(Err(InventoryError::Incomplete)).is_none());
        assert!(state.classify(Err(InventoryError::Incomplete)).is_none());
    }

    #[test]
    fn persistent_initial_failure_reports_unavailable_once_then_recovers() {
        let mut state = WatchState::default();
        // No snapshot has ever landed, so repeated failure must eventually stop
        // looking like "still scanning".
        for _ in 1..INITIAL_FAILURE_LIMIT {
            assert!(state.classify(Err(InventoryError::Incomplete)).is_none());
        }
        assert!(matches!(
            state.classify(Err(InventoryError::Incomplete)),
            Some(InventoryEvent::Unavailable)
        ));
        // Reported once, not on every later failure.
        assert!(state.classify(Err(InventoryError::Incomplete)).is_none());
        // …and a later success recovers with a live snapshot.
        assert!(matches!(
            state.classify(Ok(vec![])),
            Some(InventoryEvent::Snapshot(_))
        ));
    }
}
