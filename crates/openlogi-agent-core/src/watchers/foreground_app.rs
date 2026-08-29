//! Foreground application watcher.

use std::time::Duration;

use openlogi_core::app::ForegroundApp;
use tokio::sync::mpsc;

use super::poll;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use super::poll::Poll;

#[cfg(target_os = "macos")]
use std::sync::mpsc::RecvTimeoutError;
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(any(target_os = "macos", test))]
use std::time::Instant;
#[cfg(target_os = "macos")]
use tracing::{debug, warn};

/// Long-stop recovery after the native activation path has been quiet.
///
/// This is deliberately not a foreground polling cadence: every delivered
/// native activation defers it. It remains armed because AppKit can lose a
/// notification or retain an observer that has silently stopped calling back;
/// neither condition disconnects the callback channel.
#[cfg(any(target_os = "macos", test))]
const MACOS_IDLE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);

#[cfg(any(target_os = "macos", test))]
fn next_idle_recovery_deadline(now: Instant) -> Instant {
    now + MACOS_IDLE_RECOVERY_INTERVAL
}

/// Channel item: `Some(app)` when an app is frontmost; `None` for "no
/// foreground app" (rare on macOS — Finder is usually frontmost even when
/// nothing else is).
pub type ForegroundUpdate = Option<ForegroundApp>;

/// Watch foreground application changes.
///
/// macOS uses native activation notifications plus a slow recovery read. Linux
/// and Windows keep their platform-specific readers on the supplied polling
/// cadence.
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        // No way to read the frontmost app, so per-app profiles never switch.
        return poll::never();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = period;
        spawn_macos()
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        Poll {
            name: "openlogi-app-watcher",
            period,
            degrades: "per-app profiles are disabled",
        }
        .on_change(openlogi_hook::frontmost_application)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        unreachable!("unsupported platforms returned above")
    }
}

/// The last value published to the orchestrator, used only to suppress
/// duplicate snapshots. `NSWorkspace`, not this adapter, remains the source of
/// truth for the current application.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct ForegroundChanges {
    published: Option<ForegroundUpdate>,
}

#[cfg(any(target_os = "macos", test))]
impl ForegroundChanges {
    fn observe(&mut self, current: &ForegroundUpdate) -> bool {
        if self.published.as_ref() == Some(current) {
            return false;
        }
        self.published = Some(current.clone());
        true
    }
}

/// Observe native app activations from each notification's application payload.
/// After 30 seconds without a native activation, one authoritative read recovers
/// from a notification the OS failed to deliver or from an observer that remains
/// registered but has silently stopped delivering callbacks. Successful native
/// delivery keeps deferring that deadline, so this is not periodic polling.
#[cfg(target_os = "macos")]
fn spawn_macos() -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawned = thread::Builder::new()
        .name("openlogi-app-watcher".into())
        .spawn(move || {
            let (activation_tx, activation_rx) = std::sync::mpsc::channel();
            let _observer =
                openlogi_hook::watch_frontmost_application_activations(move |activation| {
                    let _ = activation_tx.send(activation);
                });
            let mut changes = ForegroundChanges::default();

            // Register first so an activation racing this initial snapshot is
            // either reflected by the read or queued for the loop below.
            if !publish_current(&tx, &mut changes) {
                return;
            }
            let mut idle_recovery_deadline = next_idle_recovery_deadline(Instant::now());

            loop {
                let timeout = idle_recovery_deadline.saturating_duration_since(Instant::now());
                match activation_rx.recv_timeout(timeout) {
                    Ok(mut activation) => {
                        // Coalesce a burst to its latest authoritative AppKit
                        // payload; intermediate activations were never stable
                        // foreground state for the consumer.
                        while let Ok(next) = activation_rx.try_recv() {
                            activation = next;
                        }
                        if !publish(&tx, &mut changes, activation) {
                            return;
                        }
                        idle_recovery_deadline = next_idle_recovery_deadline(Instant::now());
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if tx.is_closed() {
                            debug!("foreground-app watcher receiver dropped — exiting");
                            return;
                        }
                        if Instant::now() >= idle_recovery_deadline {
                            if !publish_current(&tx, &mut changes) {
                                return;
                            }
                            idle_recovery_deadline = next_idle_recovery_deadline(Instant::now());
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        // `_observer` retains the callback and therefore its
                        // sender for this loop's lifetime. Silence does not
                        // disconnect it (the idle recovery above handles that),
                        // so disconnection means the observer ownership
                        // contract itself broke and there is no producer left.
                        warn!(
                            "foreground-app activation observer stopped — per-app profiles are disabled"
                        );
                        return;
                    }
                }
            }
        });
    if let Err(error) = spawned {
        warn!(error = %error, "could not spawn foreground-app watcher — per-app profiles are disabled");
    }
    rx
}

#[cfg(target_os = "macos")]
fn publish_current(
    tx: &mpsc::UnboundedSender<ForegroundUpdate>,
    changes: &mut ForegroundChanges,
) -> bool {
    publish(tx, changes, openlogi_hook::frontmost_application())
}

#[cfg(target_os = "macos")]
fn publish(
    tx: &mpsc::UnboundedSender<ForegroundUpdate>,
    changes: &mut ForegroundChanges,
    current: ForegroundUpdate,
) -> bool {
    if !changes.observe(&current) {
        return true;
    }
    debug!(value = ?current, "foreground application changed");
    if tx.send(current).is_err() {
        debug!("foreground-app watcher receiver dropped — exiting");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> ForegroundApp {
        ForegroundApp::unnamed(id.to_owned())
    }

    #[test]
    fn first_snapshot_and_only_changes_are_published() {
        let mut changes = ForegroundChanges::default();

        // `None` is a real foreground snapshot, distinct from "not published".
        assert!(changes.observe(&None));
        assert!(!changes.observe(&None));
        assert!(changes.observe(&Some(app("com.example.One"))));
        assert!(!changes.observe(&Some(app("com.example.One"))));
        assert!(changes.observe(&Some(app("com.example.Two"))));
        assert!(changes.observe(&None));
    }

    #[test]
    fn native_activity_defers_the_idle_recovery_read() {
        let started = Instant::now();
        let original = next_idle_recovery_deadline(started);
        let activation = started + Duration::from_secs(12);
        let deferred = next_idle_recovery_deadline(activation);

        assert_eq!(
            original.saturating_duration_since(started),
            MACOS_IDLE_RECOVERY_INTERVAL
        );
        assert_eq!(
            deferred.saturating_duration_since(activation),
            MACOS_IDLE_RECOVERY_INTERVAL
        );
        assert!(deferred > original);
    }
}
