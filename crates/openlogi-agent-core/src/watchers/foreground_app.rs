//! Foreground application watcher.

use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::app::ForegroundApp;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::poll;

/// Recovery after the native event path has been quiet.
///
/// This is deliberately not a foreground polling cadence: every delivered
/// native event defers it. It remains armed because native observers and event
/// transports can miss a change without disconnecting their callback.
const IDLE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);

/// Retry cadence only while native observer setup or health is failing.
const OBSERVER_RETRY_INTERVAL: Duration = Duration::from_secs(2);

fn next_idle_recovery_deadline(now: Instant) -> Instant {
    now + IDLE_RECOVERY_INTERVAL
}

/// Channel item: `Some(app)` when an app is frontmost; `None` for "no
/// foreground app" (rare on macOS — Finder is usually frontmost even when
/// nothing else is).
pub type ForegroundUpdate = Option<ForegroundApp>;

/// Watch foreground application changes.
///
/// macOS, Linux, and Windows use native platform events plus a slow idle
/// recovery read. Unsupported targets return a receiver that never yields.
#[must_use]
pub fn spawn() -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        // No way to read the frontmost app, so per-app profiles never switch.
        return poll::never();
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        spawn_native()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        unreachable!("unsupported platforms returned above")
    }
}

/// The last value published to the orchestrator, used only to suppress
/// duplicate snapshots. `NSWorkspace`, not this adapter, remains the source of
/// truth for the current application.
#[derive(Default)]
struct ForegroundChanges {
    published: Option<ForegroundUpdate>,
}

impl ForegroundChanges {
    fn observe(&mut self, current: &ForegroundUpdate) -> bool {
        if self.published.as_ref() == Some(current) {
            return false;
        }
        self.published = Some(current.clone());
        true
    }
}

/// Treat native callbacks as invalidations, then read the hook crate's current
/// application SSOT. After 30 seconds without an event, one authoritative read
/// and health check recover from a missed event or silent observer failure.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn spawn_native() -> mpsc::UnboundedReceiver<ForegroundUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawned = thread::Builder::new()
        .name("openlogi-app-watcher".into())
        .spawn(move || {
            let mut changes = ForegroundChanges::default();
            let mut recovery_deadline = Instant::now();

            loop {
                // Every callback means only "the authoritative value may have
                // changed", so capacity one preserves all semantics while
                // bounding native bursts before this worker can coalesce them.
                let (native_tx, native_rx) = std::sync::mpsc::sync_channel(1);
                let observer = match openlogi_hook::watch_frontmost_application_changes(move || {
                    let _ = native_tx.try_send(());
                }) {
                    Ok(observer) => observer,
                    Err(error) => {
                        warn!(%error, "could not start native foreground-app observer; retrying");
                        let now = Instant::now();
                        if now >= recovery_deadline {
                            if !publish_current(&tx, &mut changes) {
                                return;
                            }
                            recovery_deadline = next_idle_recovery_deadline(now);
                        }
                        thread::sleep(OBSERVER_RETRY_INTERVAL);
                        if tx.is_closed() {
                            debug!("foreground-app watcher receiver dropped — exiting");
                            return;
                        }
                        continue;
                    }
                };
                recovery_deadline = next_idle_recovery_deadline(Instant::now());

                loop {
                    let timeout = recovery_deadline.saturating_duration_since(Instant::now());
                    match native_rx.recv_timeout(timeout) {
                        Ok(()) => {
                            while native_rx.try_recv().is_ok() {}
                            if !publish_current(&tx, &mut changes) {
                                return;
                            }
                            recovery_deadline = next_idle_recovery_deadline(Instant::now());
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if tx.is_closed() {
                                debug!("foreground-app watcher receiver dropped — exiting");
                                return;
                            }
                            if let Err(error) = observer.check_health() {
                                warn!(%error, "native foreground-app observer stopped; restarting");
                                break;
                            }
                            if !publish_current(&tx, &mut changes) {
                                return;
                            }
                            recovery_deadline = next_idle_recovery_deadline(Instant::now());
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            warn!("native foreground-app observer disconnected; restarting");
                            break;
                        }
                    }
                }

                drop(observer);
                if tx.is_closed() {
                    debug!("foreground-app watcher receiver dropped — exiting");
                    return;
                }
                thread::sleep(OBSERVER_RETRY_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        warn!(error = %error, "could not spawn foreground-app watcher — per-app profiles are disabled");
    }
    rx
}

fn publish_current(
    tx: &mpsc::UnboundedSender<ForegroundUpdate>,
    changes: &mut ForegroundChanges,
) -> bool {
    publish(tx, changes, openlogi_hook::frontmost_application())
}

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
            IDLE_RECOVERY_INTERVAL
        );
        assert_eq!(
            deferred.saturating_duration_since(activation),
            IDLE_RECOVERY_INTERVAL
        );
        assert!(deferred > original);
    }
}
