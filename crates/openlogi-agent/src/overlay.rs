//! Supervision of the warm Actions Ring overlay helper.
//!
//! The helper owns no device state and exits harmlessly when its binary is not
//! packaged. Keeping it warm removes process-start latency from panel presses.
//!
//! Exactly one overlay may exist, enforced by the `overlay.lock` single-instance
//! guard the helper takes at startup. This supervisor therefore waits for that
//! role to be free instead of launching into it: an agent restart can leave the
//! previous agent's overlay running for a few seconds, and starting copies that
//! immediately die on the lock is pure churn — on Windows each doomed launch
//! flashes the global busy cursor (#621). The orphan yields on its own once it
//! sees this agent's instance token, which is what frees the lock.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Lock file the overlay holds for its whole life.
const OVERLAY_LOCK: &str = "overlay.lock";

/// How often to re-probe an occupied overlay role.
const LOCK_POLL: Duration = Duration::from_millis(500);

/// Restart delay after an overlay that had actually been running exits.
const RESTART_DELAY: Duration = Duration::from_secs(2);

/// Ceiling for the backoff applied to an overlay that keeps dying at startup.
const MAX_RESTART_DELAY: Duration = Duration::from_mins(1);

/// How long a run has to last to count as "it worked, then it stopped" rather
/// than "it cannot start", which is what resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(30);

/// The delay before the next start attempt, given the previous delay and how
/// long the run that just ended lasted.
///
/// A helper that cannot start at all (a broken binary, a missing display) would
/// otherwise be relaunched twice a second forever; backing off keeps that
/// failure quiet without giving up on it.
fn next_delay(previous: Duration, ran_for: Duration) -> Duration {
    if ran_for >= HEALTHY_RUN {
        RESTART_DELAY
    } else {
        (previous * 2).min(MAX_RESTART_DELAY)
    }
}

/// Start the overlay supervisor on a dedicated thread.
pub fn spawn() {
    let Some(binary) = overlay_binary_path() else {
        warn!("Actions Ring overlay binary not found — overlay disabled");
        return;
    };
    let result = std::thread::Builder::new()
        .name("openlogi-overlay-supervisor".into())
        .spawn(move || {
            let mut delay = RESTART_DELAY;
            loop {
                while openlogi_core::single_instance::is_held(OVERLAY_LOCK) {
                    std::thread::sleep(LOCK_POLL);
                }
                let started = Instant::now();
                match Command::new(&binary).spawn() {
                    Ok(mut child) => {
                        info!(path = %binary.display(), "Actions Ring overlay started");
                        match child.wait() {
                            Ok(status) => info!(?status, "Actions Ring overlay exited"),
                            Err(error) => warn!(%error, "could not wait for Actions Ring overlay"),
                        }
                    }
                    Err(error) => {
                        warn!(%error, path = %binary.display(), "could not start Actions Ring overlay");
                    }
                }
                delay = next_delay(delay, started.elapsed());
                std::thread::sleep(delay);
            }
        });
    if let Err(error) = result {
        warn!(%error, "could not start Actions Ring overlay supervisor");
    }
}

fn overlay_binary_path() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let sibling = executable
        .parent()?
        .join(format!("openlogi-overlay{}", std::env::consts::EXE_SUFFIX));
    if sibling.is_file() {
        return Some(sibling);
    }

    #[cfg(target_os = "macos")]
    for app in executable.ancestors().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }) {
        let candidate = app.join(
            "Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
        );
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    find_on_path("openlogi-overlay")
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_helper_that_dies_at_startup_is_retried_ever_more_slowly() {
        let instant = Duration::from_millis(20);
        let first = next_delay(RESTART_DELAY, instant);
        assert_eq!(first, RESTART_DELAY * 2);
        // The ceiling holds however long the failure persists.
        let mut delay = first;
        for _ in 0..10 {
            delay = next_delay(delay, instant);
        }
        assert_eq!(delay, MAX_RESTART_DELAY);
    }

    #[test]
    fn a_run_that_lasted_resets_the_backoff() {
        assert_eq!(next_delay(MAX_RESTART_DELAY, HEALTHY_RUN), RESTART_DELAY);
    }

    #[test]
    fn path_search_returns_none_for_an_impossible_name() {
        assert_eq!(
            find_on_path("openlogi-overlay-this-file-does-not-exist"),
            None
        );
    }

    #[test]
    fn nested_overlay_path_has_expected_layout() {
        let outer = Path::new("/Applications/OpenLogi.app");
        assert_eq!(
            outer.join(
                "Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay"
            ),
            Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay"
            )
        );
    }
}
