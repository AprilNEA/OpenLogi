//! Singleton handoff for supervised macOS Agent starts.
//!
//! Registration belongs to the GUI's SMAppService integration. A supervised
//! successor may still race a manually started or legacy Agent during an
//! update; it waits for the singleton without creating a second input hook.
//! Explicit Quit and stale-marker handling retain the previous recovery contract.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tracing::{debug, info, warn};

const LAUNCHD_ARGUMENT: &str = "--launchd";
const CLEAN_EXIT_MARKER: &str = "launchd-clean-exit";
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Whether this process was started from the generated LaunchAgent.
pub fn started_by_launchd() -> bool {
    std::env::args_os()
        .skip(1)
        .any(|argument| argument == LAUNCHD_ARGUMENT)
}

/// Wait for a manually-started predecessor to release the agent lock.
///
/// launchd starts the registered job immediately. Parking that copy behind the
/// existing agent transfers ownership without running two hooks. An abnormal
/// predecessor exit lets this copy continue; an explicit Quit writes a
/// one-shot marker and this copy exits successfully, preserving Quit semantics.
pub fn wait_for_predecessor(
    lock_path: &std::path::Path,
) -> Option<openlogi_core::single_instance::InstanceGuard> {
    use openlogi_core::single_instance::{self, InstanceError};

    info!(path = %lock_path.display(), "launchd agent waiting to take ownership from the running agent");
    loop {
        match single_instance::acquire("agent.lock") {
            Ok(guard) => {
                if consume_clean_exit_marker() {
                    info!(
                        "running agent exited by user request — launchd successor staying stopped"
                    );
                    return None;
                }
                info!("launchd agent took ownership after predecessor exit");
                return Some(guard);
            }
            Err(InstanceError::AlreadyRunning { .. }) => {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => {
                warn!(%error, "launchd agent could not acquire the singleton lock");
                return None;
            }
        }
    }
}

/// Preserve the user-visible contract that menu-bar Quit stops the agent.
pub fn mark_user_quit() {
    let path = clean_exit_marker_path();
    let result = path.and_then(|path| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, std::process::id().to_string())
    });
    if let Err(error) = result {
        warn!(%error, "could not mark the explicit agent Quit for launchd");
    }
}

/// Remove a marker left by an earlier launchd-owned process that exited via
/// menu-bar Quit. A newly acquired singleton lock starts a new explicit run;
/// the old marker must not suppress its future crash handoff.
pub fn clear_stale_clean_exit_marker() {
    let Ok(path) = clean_exit_marker_path() else {
        return;
    };
    match std::fs::remove_file(path) {
        Ok(()) => debug!("cleared stale explicit agent Quit marker"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => warn!(%error, "could not clear stale explicit agent Quit marker"),
    }
}

fn consume_clean_exit_marker() -> bool {
    let Ok(path) = clean_exit_marker_path() else {
        return false;
    };
    consume_marker_at(&path)
}

fn consume_marker_at(path: &std::path::Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            warn!(%error, "could not consume the explicit agent Quit marker");
            false
        }
    }
}

fn clean_exit_marker_path() -> io::Result<PathBuf> {
    openlogi_core::paths::config_dir()
        .map(|directory| directory.join(CLEAN_EXIT_MARKER))
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::consume_marker_at;

    #[test]
    fn explicit_quit_marker_stops_only_one_successor() {
        let directory = tempfile::tempdir().expect("temporary marker directory");
        let marker = directory.path().join("launchd-clean-exit");
        std::fs::write(&marker, "123").expect("write explicit Quit marker");

        assert!(consume_marker_at(&marker));
        assert!(!consume_marker_at(&marker));
        assert!(!marker.exists());
    }

    #[test]
    fn abnormal_exit_without_a_marker_allows_handoff() {
        let directory = tempfile::tempdir().expect("temporary marker directory");
        assert!(!consume_marker_at(
            &directory.path().join("launchd-clean-exit")
        ));
    }
}
