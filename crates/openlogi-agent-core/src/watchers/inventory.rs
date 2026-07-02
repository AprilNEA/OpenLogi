//! HID inventory watcher: periodic polling, woken early by hotplug events.
//!
//! Spawns a dedicated OS thread with a one-shot tokio runtime that calls
//! `openlogi_hid::enumerate` every `period` and forwards each completed
//! snapshot over an unbounded mpsc to the agent's select loop, which applies
//! it via `Orchestrator::refresh_inventory`.
//!
//! An OS hotplug event (`openlogi_hid::watch_hotplug`) cuts the wait short so
//! a just-plugged device is probed — and gets its persisted settings applied —
//! within a settle delay instead of a full period. The periodic tick stays as
//! the reconciliation pass (battery refresh, missed events, platforms where
//! the hotplug stream is unavailable).

use std::thread;
use std::time::{Duration, SystemTime};

use futures_lite::StreamExt as _;
use openlogi_core::device::DeviceInventory;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Consecutive *initial* enumerate failures before the watcher declares
/// enumeration [`InventoryEvent::Unavailable`]. Only counts before the first
/// success: a mid-session failure keeps the last good snapshot instead (see
/// the error arm below), and a later success upgrades `Unavailable` back to a
/// live inventory.
const INITIAL_FAILURE_LIMIT: u8 = 3;

/// Pause between a hotplug event and the early enumerate, so a just-connected
/// node finishes registering with the OS before the probe opens it.
const HOTPLUG_SETTLE: Duration = Duration::from_millis(400);

/// Wall-clock slack past `period` before a late tick is read as a sleep/wake
/// gap. Generously above the worst honest iteration (period + a fully
/// timed-out probe pass), so only a genuine suspend trips it; a rare false
/// positive (e.g. a large NTP step) merely re-applies settings the devices
/// already have.
const WAKE_GAP: Duration = Duration::from_secs(60);

/// What the watcher tells the agent.
pub enum InventoryEvent {
    /// A completed enumeration — empty means "checked, no devices".
    Snapshot(Vec<DeviceInventory>),
    /// Enumeration has never succeeded and won't be treated as "still
    /// starting" any longer; without this the GUI would show its scanning
    /// state forever on a broken HID backend.
    Unavailable,
    /// The wall clock jumped far past the polling period — the system almost
    /// certainly slept and woke. Devices may have power-cycled while their
    /// set/route/online state looks unchanged across the gap, so the agent
    /// re-applies volatile settings on the next snapshot (#189). Detected by
    /// wall clock because the monotonic clock pauses during sleep on macOS.
    SystemWake,
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
    ///   reality. A transient per-node probe miss never reaches here as an empty
    ///   `Ok`: `openlogi_hid`'s `NodeLedger` replays the node's last inventory
    ///   (#218/#222).
    /// - `Err(..)` means enumeration itself failed (OS-level HID enumerate
    ///   error): emit nothing, so the agent keeps its last good device set and
    ///   live bindings instead of wiping them for ~one period. Before the *first*
    ///   success there is no good set to keep, so persistent initial failure is
    ///   reported once as [`InventoryEvent::Unavailable`]; the loop keeps
    ///   retrying and a later success recovers.
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
            let mut last_tick = SystemTime::now();
            // `block_on` installs runtime context so a backend that registers an
            // `AsyncFd` (Linux udev) fails as a catchable `Err`, not a panic that
            // would take down the whole watcher thread.
            let mut hotplug = match rt.block_on(async { openlogi_hid::watch_hotplug() }) {
                Ok(stream) => Some(stream),
                Err(e) => {
                    warn!(error = ?e, "hotplug watch unavailable — polling only");
                    None
                }
            };
            loop {
                // A tick arriving far past its period means the system slept;
                // `duration_since` errs when the clock stepped backwards, in
                // which case there is nothing to conclude — just re-anchor.
                let now = SystemTime::now();
                if let Ok(elapsed) = now.duration_since(last_tick)
                    && elapsed > period + WAKE_GAP
                {
                    info!(?elapsed, "wall-clock gap — assuming a system wake");
                    if worker_tx.send(InventoryEvent::SystemWake).is_err() {
                        return;
                    }
                }
                last_tick = now;
                let result = rt.block_on(enumerator.enumerate());
                if let Some(event) = state.classify(result)
                    && worker_tx.send(event).is_err()
                {
                    debug!("inventory watcher receiver dropped — exiting");
                    return;
                }
                let stream_alive = rt.block_on(async {
                    let Some(stream) = hotplug.as_mut() else {
                        tokio::time::sleep(period).await;
                        return true;
                    };
                    tokio::select! {
                        () = tokio::time::sleep(period) => true,
                        event = stream.next() => match event {
                            Some(event) => {
                                debug!(?event, "hotplug event — enumerating early");
                                tokio::time::sleep(HOTPLUG_SETTLE).await;
                                // Drain the burst so one enumerate covers every node that just
                                // arrived; a `None` here is the stream closing, so report it now
                                // rather than after a spurious extra enumerate next tick.
                                let mut alive = true;
                                while let Some(drained) =
                                    futures_lite::future::poll_once(stream.next()).await
                                {
                                    if drained.is_none() {
                                        alive = false;
                                        break;
                                    }
                                }
                                alive
                            }
                            None => false,
                        },
                    }
                });
                if !stream_alive && hotplug.take().is_some() {
                    warn!("hotplug stream ended — falling back to pure polling");
                }
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

    /// A transport-level enumerate failure — what the watcher's `Err` arm now
    /// sees (a partial per-node read is replayed by the hid ledger as `Ok`).
    fn enumerate_failed() -> InventoryError {
        InventoryError::Hid(async_hid::HidError::Disconnected)
    }

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
    fn failure_after_a_success_keeps_the_last_snapshot() {
        let mut state = WatchState::default();
        // A good tick first, so there is a last-known-good set to preserve.
        assert!(matches!(
            state.classify(Ok(vec![])),
            Some(InventoryEvent::Snapshot(_))
        ));
        // Then transient enumerate failures emit nothing — the agent keeps the
        // last snapshot instead of flapping to "No devices" (#218).
        assert!(state.classify(Err(enumerate_failed())).is_none());
        assert!(state.classify(Err(enumerate_failed())).is_none());
    }

    #[test]
    fn persistent_initial_failure_reports_unavailable_once_then_recovers() {
        let mut state = WatchState::default();
        // No snapshot has ever landed, so repeated failure must eventually stop
        // looking like "still scanning".
        for _ in 0..INITIAL_FAILURE_LIMIT - 1 {
            assert!(state.classify(Err(enumerate_failed())).is_none());
        }
        assert!(matches!(
            state.classify(Err(enumerate_failed())),
            Some(InventoryEvent::Unavailable)
        ));
        // Reported once, not on every later failure.
        assert!(state.classify(Err(enumerate_failed())).is_none());
        // …and a later success recovers with a live snapshot.
        assert!(matches!(
            state.classify(Ok(vec![])),
            Some(InventoryEvent::Snapshot(_))
        ));
    }
}
