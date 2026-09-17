//! Autostart reconciliation for the background agent.
//!
//! Implements `launch_at_login` by keeping a platform-specific autostart
//! descriptor in sync whenever the setting changes. One module per OS —
//! [`reconcile`] dispatches to the platform's file, which owns its mechanism,
//! its logging, and its tests:
//!
//! - **macOS** ([`macos`]): migration only. The GUI owns registration via
//!   `SMAppService` (the API resolves the service plist against the *calling*
//!   app's bundle, so only the GUI can call it); the agent just removes the
//!   hand-written legacy `~/Library/LaunchAgents` plists. A hand-edited
//!   `config.toml` therefore takes effect the next time the GUI runs.
//! - **Linux** ([`linux`]): a systemd **user** unit, written/removed and
//!   `systemctl --user` enabled/disabled. `Restart=on-failure` mirrors the
//!   macOS service's `KeepAlive = {SuccessfulExit: false}` semantics.
//! - **Windows** ([`windows`]): an `HKCU\…\Run` registry value — login launch
//!   only, no crash respawn.
//!
//! Every arm is idempotent — it writes only when the content differs and
//! removes only what exists — and failures are logged, never propagated:
//! startup must not abort because an autostart directory is read-only or
//! systemd is unavailable.
//!
//! Every arm also runs alone. The IPC server spawns one task per request, and
//! each `reload_config` ends in a [`reconcile`], so two settings writes in
//! quick succession reach the arm concurrently. The Linux arm records its
//! claim on the enablement in one step and rolls it back on failure in
//! another; interleave two of those and the failing call can withdraw a
//! claim the succeeding one still relies on. Serialising here, rather than in
//! each arm, keeps that guarantee in one place.

use std::sync::Mutex;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Reconcile the agent's autostart state with `enabled`.
///
/// Reconciles never overlap; a second caller waits for the first to finish.
pub fn reconcile(enabled: bool) {
    serialized(|| reconcile_unlocked(enabled));
}

/// One reconcile at a time, process-wide.
///
/// A poisoned lock is taken anyway: the arms propagate nothing, so a panic
/// inside one left no state behind that the next reconcile needs to avoid.
fn serialized<T>(f: impl FnOnce() -> T) -> T {
    static RECONCILE: Mutex<()> = Mutex::new(());
    let _guard = RECONCILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f()
}

/// The per-platform dispatch, called with the lock held.
fn reconcile_unlocked(enabled: bool) {
    #[cfg(target_os = "macos")]
    macos::reconcile(enabled);
    #[cfg(target_os = "linux")]
    linux::reconcile(enabled);
    #[cfg(target_os = "windows")]
    windows::reconcile(enabled);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        if enabled {
            tracing::debug!("launch_at_login set but no autostart backend on this platform");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::serialized;

    /// Two reconciles released at once must run one after the other: the
    /// in-flight count seen inside the critical section never exceeds one.
    #[test]
    fn concurrent_reconciles_do_not_overlap() {
        const RACERS: usize = 8;
        let start = Arc::new(Barrier::new(RACERS));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicUsize::new(0));

        let racers: Vec<_> = (0..RACERS)
            .map(|_| {
                let start = Arc::clone(&start);
                let in_flight = Arc::clone(&in_flight);
                let overlapped = Arc::clone(&overlapped);
                thread::spawn(move || {
                    start.wait();
                    serialized(|| {
                        if in_flight.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlapped.fetch_add(1, Ordering::SeqCst);
                        }
                        thread::yield_now();
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                })
            })
            .collect();
        for racer in racers {
            racer.join().expect("racer panicked");
        }

        assert_eq!(
            overlapped.load(Ordering::SeqCst),
            0,
            "reconciles ran concurrently"
        );
    }

    /// A panic inside one reconcile must not wedge every later one.
    #[test]
    fn a_poisoned_lock_is_still_taken() {
        let _ = thread::spawn(|| serialized(|| panic!("poison")))
            .join()
            .expect_err("the panic reaches the join");
        assert_eq!(serialized(|| 7), 7);
    }
}
