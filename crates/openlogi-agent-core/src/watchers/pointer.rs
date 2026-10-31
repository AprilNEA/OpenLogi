//! Off-tap pointer-window discovery with a coalescing publication.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, Thread};
use std::time::Duration;
#[cfg(target_os = "windows")]
use std::time::Instant;

#[cfg(target_os = "windows")]
use openlogi_core::app::ForegroundApp;
#[cfg(target_os = "windows")]
use openlogi_core::brand;
use openlogi_core::config::MouseProfileTarget;
use openlogi_hook::{PointerContext, PointerTarget};
use tokio::sync::watch;
use tracing::warn;

/// The fastest re-query cadence: every poll off Windows, and the cap on Windows
/// while mouse input keeps arriving.
const MOTION_INTERVAL: Duration = Duration::from_millis(50);

/// The longest a focused watcher waits to notice its receivers closed. On
/// Windows this also bounds resting-pointer rechecks: a window opening or
/// closing under it need not report mouse input or move focus.
const IDLE_RECHECK: Duration = Duration::from_secs(1);

/// Whether mouse profiles follow the pointer. While they follow focus the
/// orchestrator ignores every pointer context, so the watcher parks instead.
static FOLLOWING: AtomicBool = AtomicBool::new(true);

/// The watcher thread, unparked when [`FOLLOWING`] turns back on.
static WATCHER: Mutex<Option<Thread>> = Mutex::new(None);

/// Observe the window or desktop under the pointer. Only context transitions
/// are published; pointer motion within one window does not rebuild mappings.
/// Native queries never run on the input callback, and a slow consumer retains
/// only the latest snapshot rather than an unbounded history of hover changes.
///
/// On Windows the worker sleeps until the input hook reports mouse input or the
/// foreground window changes, re-checking a resting pointer once a second and
/// a surface that can vanish unannounced as often as it used to poll;
/// elsewhere it polls. It parks entirely while [`set_profile_target`] says
/// mouse profiles follow focus.
#[must_use]
pub fn spawn() -> watch::Receiver<PointerContext> {
    let supported = openlogi_hook::pointer_context_supported();
    let (tx, rx) = watch::channel(PointerContext {
        app: None,
        target: if supported {
            PointerTarget::Unavailable
        } else {
            PointerTarget::Unsupported
        },
    });
    if !supported {
        return rx;
    }
    let spawned = thread::Builder::new()
        .name("openlogi-pointer-context".into())
        .spawn(move || run(&tx, &FOLLOWING));
    if let Err(error) = spawned {
        warn!(%error, "could not start pointer context watcher — pointer context remains unidentified");
    }
    rx
}

/// Follow `mouse_profile_target`: with [`MouseProfileTarget::Focused`] the
/// watcher stops querying, and switching back to
/// [`MouseProfileTarget::Pointer`] resumes it with an immediate query. The
/// orchestrator calls this when it is built and on every config reload; until
/// then, the watcher follows the pointer.
pub fn set_profile_target(target: MouseProfileTarget) {
    let following = target == MouseProfileTarget::Pointer;
    // Only a resume has a parked watcher to wake.
    if FOLLOWING.swap(following, Ordering::AcqRel) == following || !following {
        return;
    }
    if let Some(watcher) = WATCHER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        watcher.unpark();
    }
}

fn run(tx: &watch::Sender<PointerContext>, following: &AtomicBool) {
    // Registered under the lock `set_profile_target` takes before unparking,
    // so a resume that lands first is still seen by the check below.
    *WATCHER.lock().unwrap_or_else(PoisonError::into_inner) = Some(thread::current());
    #[cfg(target_os = "windows")]
    let mut idle_wait = IDLE_RECHECK;
    while !tx.is_closed() {
        if !following.load(Ordering::Acquire) {
            // Receiver closure does not unpark this thread. Bound the wait so
            // shutdown is still observed without querying in focused mode.
            thread::park_timeout(IDLE_RECHECK);
            continue;
        }
        let current = openlogi_hook::pointer_context();
        #[cfg(target_os = "windows")]
        let surface = Surface::of(&current);
        tx.send_if_modified(|previous| {
            if *previous == current {
                return false;
            }
            *previous = current;
            true
        });
        #[cfg(target_os = "windows")]
        {
            idle_wait = wait_for_activity(idle_wait, surface);
        }
        #[cfg(not(target_os = "windows"))]
        thread::sleep(MOTION_INTERVAL);
    }
}

/// Windows: what a query found under the pointer, as far as how soon to look
/// again goes.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    /// A window that stays put until mouse input or a focus change says
    /// otherwise.
    Settled,
    /// Something that can vanish with neither: the Actions Ring's window,
    /// which a HID++ press or its own timeout closes, or a surface no lookup
    /// could identify.
    Fleeting,
    /// No cursor to read: another desktop has the input. Nothing under the
    /// pointer changes until the user is back.
    Away,
}

#[cfg(target_os = "windows")]
impl Surface {
    fn of(context: &PointerContext) -> Self {
        if context.target == PointerTarget::Unavailable {
            if PointerContext::cursor_is_readable() {
                Self::Fleeting
            } else {
                Self::Away
            }
        } else if context.app.as_ref().is_some_and(is_overlay) {
            Self::Fleeting
        } else {
            Self::Settled
        }
    }
}

/// Whether `app` is the Actions Ring helper. Its ring window takes clicks, so
/// the pointer over it resolves to that process rather than to
/// [`PointerTarget::Unavailable`].
#[cfg(target_os = "windows")]
fn is_overlay(app: &ForegroundApp) -> bool {
    app.display_name
        .eq_ignore_ascii_case(brand::Helper::Overlay.executable())
}

/// Windows: after a query that found `surface`, park until mouse input, a
/// foreground change, or `idle_wait`, then hold re-queries at least
/// [`MOTION_INTERVAL`] apart. Returns how long the next wait may idle.
#[cfg(target_os = "windows")]
fn wait_for_activity(idle_wait: Duration, surface: Surface) -> Duration {
    let started = Instant::now();
    let timeout = match surface {
        // The user comes back with mouse input; the recheck bounds a return
        // made with the keyboard alone.
        Surface::Away => IDLE_RECHECK,
        // Poll as before while the surface can vanish unannounced, or while no
        // hook reports mouse input at all.
        Surface::Fleeting => MOTION_INTERVAL,
        Surface::Settled if !PointerContext::motion_is_tracked() => MOTION_INTERVAL,
        Surface::Settled => idle_wait,
    };
    let woke = PointerContext::wait_for_activity(timeout);
    if let Some(rest) = MOTION_INTERVAL.checked_sub(started.elapsed()) {
        thread::sleep(rest);
    }
    // The hook reports input before Windows applies it, so the query a burst's
    // last event triggers can still see the old window: look once more after
    // activity stops.
    if woke { MOTION_INTERVAL } else { IDLE_RECHECK }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_watcher_exits_after_the_last_receiver_drops() {
        let (tx, rx) = watch::channel(PointerContext {
            app: None,
            target: PointerTarget::Unavailable,
        });
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            // Keep this worker's mode independent of orchestrator tests that
            // publish configuration changes through the process-global mode.
            run(&tx, &AtomicBool::new(false));
            done_tx.send(()).unwrap();
        });
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "the focused worker stays alive while a receiver owns its output"
        );
        drop(rx);
        let stopped = done_rx.recv_timeout(Duration::from_secs(2));
        // Release even the broken worker before reporting the regression.
        worker.thread().unpark();
        worker.join().unwrap();
        stopped.expect("a focused worker must notice that its last receiver closed");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_ring_window_counts_as_the_overlay_helper() {
        let app = |path: &str, stem: &str| ForegroundApp {
            id: path.to_owned(),
            display_name: stem.to_owned(),
        };
        assert!(is_overlay(&app(
            r"c:\users\me\appdata\local\programs\openlogi\openlogi-overlay.exe",
            "openlogi-overlay",
        )));
        assert!(!is_overlay(&app(
            r"c:\users\me\appdata\local\programs\openlogi\openlogi.exe",
            "OpenLogi",
        )));
    }
}
