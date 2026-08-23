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

use std::path::{Path, PathBuf};
use std::process::Command;

use openlogi_ipc::RUN_ENV;
use succession::eviction::{self, AnonymousOutcome, Policy};
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
            // The anonymous verdict repeats every poll for as long as the
            // tenant lives, and answering it walks the process table. Answer
            // once per spell of anonymity and stay quiet until the role
            // changes hands.
            let mut pressed_anonymous = false;
            loop {
                if let Err(error) = supervisor.tick(&mut spawn, &mut |event| {
                    report(&event, &mut pressed_anonymous);
                }) {
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

/// Log what the supervisor did, and evict a tenant this agent has superseded.
///
/// Eviction is the migration path: an overlay that predates the claim record
/// cannot recognize this agent as a different run, so it never yields on its
/// own. Which of the two evictions applies depends on what the tenant said
/// about itself, and neither takes a pid on faith — a record is checked
/// against the live process ([`succession::Tenant::compare`]), and a tenant
/// with no record at all is only ever recognized by the image we start the
/// overlay from.
fn report(event: &Event<'_>, pressed_anonymous: &mut bool) {
    if !matches!(event, Event::SupersededAnonymously) {
        *pressed_anonymous = false;
    }
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
        // A tenant holding the role with no readable claim record: an overlay
        // from an install that predates the record, or one whose publish
        // failed. Nothing identifies it, so `succession` falls back on the
        // image we start the overlay from and refuses unless exactly one
        // process matches — otherwise the role stays wedged for as long as
        // that process lives and the Actions Ring never comes up (#842).
        Event::SupersededAnonymously => {
            if std::mem::replace(pressed_anonymous, true) {
                tracing::debug!("{event}");
                return;
            }
            warn!("{event}");
            evict_unidentified_overlay();
        }
        Event::Occupied(_) => tracing::debug!("{event}"),
        _ => info!("{event}"),
    }
}

/// Ask an unidentified tenant to leave, trying every image our overlay could
/// be running from.
///
/// One path is not enough. A tenant started before an update runs the image
/// that install shipped, and macOS keeps reporting that path for the life of
/// the process even after the file is renamed or deleted — so the tenant most
/// in need of evicting is precisely the one whose image is not the one we
/// would launch today (#842).
fn evict_unidentified_overlay() {
    for image in overlay_images() {
        match eviction::evict_anonymous(&image, &Policy::default()) {
            // Not this image. The tenant may be running another of ours.
            AnonymousOutcome::NoCandidate => {}
            AnonymousOutcome::Ambiguous { running } => {
                warn!(
                    running,
                    image = %image.display(),
                    "several processes share this overlay image — left them alone rather \
                     than guess which one holds the role"
                );
                return;
            }
            outcome => {
                info!(
                    ?outcome,
                    image = %image.display(),
                    "asked the unidentified overlay to leave"
                );
                return;
            }
        }
    }
    warn!("no process is running any overlay image of ours — the role is held by something else");
}

/// Every path our overlay could be running from, in the order the launcher
/// prefers them.
///
/// Existence is deliberately not a filter: a process outlives the file it was
/// started from, and evicting one means recognizing the path it still reports.
/// [`overlay_binary_path`] applies the filter, because launching does need a
/// file that is there.
fn overlay_images() -> Vec<PathBuf> {
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut images = overlay_images_beside(&executable);
    images.extend(find_on_path("openlogi-overlay"));
    images
}

/// The layout-derived half of [`overlay_images`], taking the agent's own path
/// so the derivation can be tested against a bundle that is not this one.
fn overlay_images_beside(executable: &Path) -> Vec<PathBuf> {
    let mut images: Vec<PathBuf> = executable
        .parent()
        .map(|directory| {
            directory.join(format!("openlogi-overlay{}", std::env::consts::EXE_SUFFIX))
        })
        .into_iter()
        .collect();

    #[cfg(target_os = "macos")]
    for app in executable.ancestors().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }) {
        images.extend(
            [
                "Contents/Library/LoginItems/OpenLogi Overlay Dev.app/Contents/MacOS/openlogi-overlay",
                "Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay",
                // Bundles built before the helpers were renamed to their display names.
                "Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
            ]
            .into_iter()
            .map(|relative| app.join(relative)),
        );
    }

    images
}

fn overlay_binary_path() -> Option<PathBuf> {
    overlay_images().into_iter().find(|image| image.is_file())
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

    /// The tenant that wedges the role is one started before an update, and
    /// after the helpers were renamed its image is a path that no longer
    /// exists. macOS keeps reporting that path for the life of the process, so
    /// dropping absent paths here would leave exactly that tenant
    /// unrecognizable (#842).
    #[test]
    #[cfg(target_os = "macos")]
    fn eviction_candidates_include_images_that_are_no_longer_installed() {
        let agent = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        let legacy = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
        );
        assert!(
            !legacy.exists(),
            "this test only means something while that path is absent"
        );

        let images = overlay_images_beside(agent);
        assert!(
            images.iter().any(|image| image == legacy),
            "the pre-rename image must stay a candidate: {images:?}"
        );
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
                "Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            ),
            Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            )
        );
    }
}
