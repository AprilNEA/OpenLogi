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
