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
#[cfg(target_os = "macos")]
use std::time::Instant;
#[cfg(target_os = "macos")]
use tracing::{debug, warn};

#[cfg(target_os = "macos")]
const MACOS_RECONCILE_PERIOD: Duration = Duration::from_secs(30);

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
/// A slow authoritative reconciliation read recovers from any notification the
/// OS failed to deliver and notices a dropped consumer without a separate poll.
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
            let mut next_reconcile = Instant::now() + MACOS_RECONCILE_PERIOD;

            loop {
                let timeout = next_reconcile.saturating_duration_since(Instant::now());
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
                        next_reconcile = Instant::now() + MACOS_RECONCILE_PERIOD;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if tx.is_closed() {
                            debug!("foreground-app watcher receiver dropped — exiting");
                            return;
                        }
                        if Instant::now() >= next_reconcile {
                            if !publish_current(&tx, &mut changes) {
                                return;
                            }
                            next_reconcile = Instant::now() + MACOS_RECONCILE_PERIOD;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
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
}
