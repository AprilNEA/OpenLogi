//! Supervision of the warm Actions Ring overlay helper.
//!
//! The helper owns no device state and exits harmlessly when its binary is not
//! packaged. Keeping it warm removes process-start latency from panel presses.
//!
//! Exactly one overlay may exist, and it belongs to one agent run. Both halves
//! are enforced by the `succession` crate: this supervisor waits while the role
//! is filled by its own child, and evicts a tenant left behind by a previous
//! agent — which is what stops an orphaned overlay from wedging its
//! replacement out of the lock forever (#621, #644). The run token travels to
//! the child in the environment, so a helper started by a previous agent is
//! recognizable on sight rather than after a timeout.

use std::path::PathBuf;
use std::process::Command;

use openlogi_ipc::RUN_ENV;
use succession::eviction::{self, Policy};
use succession::supervision::{Event, Supervisor};
use succession::{Role, Run};
use tracing::{info, warn};

/// Start the overlay supervisor on a dedicated thread.
pub fn spawn() {
    let Some(binary) = overlay_binary_path() else {
        warn!("Actions Ring overlay binary not found — overlay disabled");
        return;
    };
    let Ok(directory) = openlogi_core::paths::config_dir() else {
        warn!("could not resolve the config directory — overlay disabled");
        return;
    };
    let mine = Run::mint();
    let mut supervisor = Supervisor::new(Role::new(directory, "overlay"), mine);
    let result = std::thread::Builder::new()
        .name("openlogi-overlay-supervisor".into())
        .spawn(move || {
            let mut spawn = move || {
                Command::new(&binary)
                    .env(RUN_ENV, mine.get().to_string())
                    .spawn()
            };
            loop {
                if let Err(error) = supervisor.tick(&mut spawn, &mut |event| report(&event)) {
                    // A role that cannot be probed is treated as free by the
                    // next tick; refusing to look again would wait forever.
                    warn!(%error, "could not read the Actions Ring overlay role");
                }
            }
        });
    if let Err(error) = result {
        warn!(%error, "could not start the Actions Ring overlay supervisor");
    }
}

/// Ask the overlay to leave, on the way out of a deliberate agent shutdown.
///
/// The helper is spawned detached and the menu-bar Quit is a `process::exit`
/// that runs no destructors, so without this the overlay outlives the agent
/// until its own give-up deadline — a minute of a stray GPUI process in
/// Activity Monitor after the user asked for everything to stop. Nothing here
/// is load-bearing: the overlay leaves either way, so the policy is tuned for a
/// Quit that still feels instant rather than for a guaranteed exit.
///
/// Only the tray platforms have a deliberate shutdown to hook. Elsewhere the
/// agent runs until something kills it, and the overlay's own deadline is all
/// there is.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn evict_on_quit() {
    use std::time::Duration;

    use succession::Occupancy;

    let Ok(directory) = openlogi_core::paths::config_dir() else {
        return;
    };
    let Ok(Occupancy::HeldBy(record)) = Role::new(directory, "overlay").occupancy() else {
        return;
    };
    let outcome = eviction::evict(
        &record,
        &Policy {
            escalate_after: Some(Duration::from_millis(150)),
            deadline: Duration::from_millis(750),
            ..Policy::default()
        },
    );
    info!(?outcome, "asked the overlay to leave before exiting");
}

/// Log what the supervisor did, and evict a tenant from a finished run.
///
/// Eviction is the migration path: an overlay that predates the claim record
/// cannot recognize this agent as a different run, so it never yields on its
/// own. Signalling is refused unless the live process still matches the record
/// (see [`succession::Tenant::compare`]) — a pid alone never justifies it.
fn report(event: &Event<'_>) {
    match *event {
        Event::Superseded(record) => {
            info!("{event}");
            match eviction::evict(record, &Policy::default()) {
                eviction::Outcome::Refused(sameness) => {
                    warn!(
                        ?sameness,
                        "left the overlay alone — its pid no longer matches"
                    );
                }
                outcome => info!(?outcome, "asked the superseded overlay to leave"),
            }
        }
        Event::SupersededAnonymously => {
            warn!("{event}");
        }
        Event::Occupied(_) => tracing::debug!("{event}"),
        _ => info!("{event}"),
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
        for relative in [
            "Contents/Library/LoginItems/OpenLogi Overlay Dev.app/Contents/MacOS/openlogi-overlay",
            "Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay",
            // Bundles built before the helpers were renamed to their display names.
            "Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
        ] {
            let candidate = app.join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
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
                "Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            ),
            Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            )
        );
    }
}
