//! Supervision of the warm Actions Ring overlay helper.
//!
//! The helper owns no device state and exits harmlessly when its binary is not
//! packaged. Keeping it warm removes process-start latency from panel presses.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use tracing::{info, warn};

const RESTART_DELAY: Duration = Duration::from_secs(2);

/// Start the overlay supervisor on a dedicated thread.
pub fn spawn() {
    let Some(binary) = overlay_binary_path() else {
        warn!("Actions Ring overlay binary not found — overlay disabled");
        return;
    };
    let result = std::thread::Builder::new()
        .name("openlogi-overlay-supervisor".into())
        .spawn(move || loop {
            match Command::new(&binary).spawn() {
                Ok(mut child) => {
                    info!(path = %binary.display(), "Actions Ring overlay started");
                    match child.wait() {
                        Ok(status) => warn!(?status, "Actions Ring overlay exited; restarting"),
                        Err(error) => warn!(%error, "could not wait for Actions Ring overlay"),
                    }
                }
                Err(error) => warn!(%error, path = %binary.display(), "could not start Actions Ring overlay"),
            }
            std::thread::sleep(RESTART_DELAY);
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
