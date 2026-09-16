//! Bringing the agent up when the socket is unreachable.
//!
//! On macOS production is supervised-only: `launchctl kickstart` the
//! registered service, register it on demand when absent (registration *is*
//! the supervised start), and stop there — a login item the user switched
//! off stays off, and a bundle whose registration fails needs its install
//! fixed, not an unmanaged shadow agent. Only dev profiles (which never
//! register) fall through to the direct launch (`open -g -n` / `disclaim`),
//! which elsewhere is the only path.

use std::path::PathBuf;

use tracing::{info, warn};

/// Set when the agent announced a deliberate suite shutdown (the tray's Quit
/// arrives as the `openlogi://quit` deep link before the agent exits) — the
/// unreachable→spawn reflex has been observed resurrecting the agent seconds
/// after Quit. Never cleared: this process is quitting too.
static SUITE_QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the user quit the whole suite from the agent's tray, so the
/// IPC client stops respawning the agent during this GUI's teardown.
pub fn mark_suite_quitting() {
    SUITE_QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Launch the agent once when the socket is unreachable. Detached so it
/// outlives the GUI (the agent is the always-on process); logs and moves on if
/// the binary can't be found / started — the user may start it via launchd or by
/// hand, and the poll loop keeps retrying the connection regardless.
pub(super) fn spawn_agent() {
    if SUITE_QUITTING.load(std::sync::atomic::Ordering::Relaxed) {
        info!("suite is quitting — leaving the agent down");
        return;
    }
    // Kickstart is idempotent, the process comes up supervised, and launchd
    // makes it its own TCC responsible process. Production tries only the two
    // supervised rungs below; direct launch is reserved for dev profiles.
    #[cfg(target_os = "macos")]
    {
        if kickstart_registered_agent() {
            return;
        }
        // Second rung: on a fresh install this reflex outruns the
        // backgrounded startup ensure — registering IS the supervised start,
        // and the re-run kickstart doubles as the "did it take?" check. Dev
        // profiles never register implicitly (a login item into `target/`
        // goes stale).
        if !openlogi_core::paths::is_dev_profile() {
            match crate::platform::registration::ensure_registered() {
                Ok(()) => {
                    if kickstart_registered_agent() {
                        return;
                    }
                }
                Err(error) => {
                    warn!(error, "on-demand agent service registration failed");
                }
            }
            // Production stops here: the supervised rungs are the only
            // launchers, so a Login Items disable is a real off switch and a
            // failed registration is a broken install to fix, not something
            // to shadow with an unmanaged agent that outlives this window.
            warn!(
                status = ?crate::platform::registration::status(),
                "agent is not running and the supervised launch paths could not start it — leaving it down"
            );
            return;
        }
    }
    let Some(path) = agent_binary_path() else {
        warn!(
            "agent not reachable and its binary wasn't found next to the GUI — \
             start it via launchd or by hand"
        );
        return;
    };
    // "started", not "launched": on the packaged path success only means
    // `open` was handed the bundle — the reaper inside `launch_agent` reports
    // the definitive outcome.
    match launch_agent(&path) {
        Ok(()) => info!(path = %path.display(), "agent not running — launch started"),
        Err(e) => warn!(error = %e, path = %path.display(), "could not launch the agent"),
    }
}

/// Launch the agent binary at `path` under its own TCC identity.
fn launch_agent(path: &std::path::Path) -> std::io::Result<()> {
    // The packaged helper goes through LaunchServices so the agent is its own
    // TCC responsible process; a direct exec attributes its Accessibility
    // check to the parent GUI and the grant flips with the launch path (#192).
    #[cfg(target_os = "macos")]
    if let Some(bundle) = helper_bundle(path) {
        let mut child = std::process::Command::new("/usr/bin/open")
            .arg("-g")
            .arg("-n")
            .arg(bundle)
            .spawn()?;
        // `open`'s exit status is the only signal the handoff failed — a
        // successful spawn proves nothing. Reap off-thread and log.
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => {
                warn!(%status, "`open` could not launch the agent bundle");
            }
            Err(e) => warn!(error = %e, "could not reap the `open` helper"),
            Ok(_) => {}
        });
        return Ok(());
    }
    // Any other layout (bare dev binary, Windows, Linux): exec the binary
    // directly while disclaiming the GUI's TCC responsibility (#214).
    disclaim::Command::new(path).spawn().map(|_| ())
}

/// `launchctl kickstart` the agent's registered launchd service. Returns
/// whether the start was handed to launchd — `false` (not registered, user
/// switched it off in Login Items, or launchctl itself failed) lets the caller
/// try registration before deciding whether its profile permits direct launch.
#[cfg(target_os = "macos")]
fn kickstart_registered_agent() -> bool {
    use crate::platform::registration;

    if registration::status() != registration::ServiceStatus::Enabled {
        return false;
    }
    let Some(uid) = current_uid() else {
        return false;
    };
    let target = format!("gui/{uid}/{}", registration::agent_service_label());
    match std::process::Command::new("/bin/launchctl")
        .arg("kickstart")
        .arg(&target)
        .output()
    {
        Ok(out) if out.status.success() => {
            info!(%target, "agent not running — kickstarted the registered service");
            true
        }
        Ok(out) => {
            warn!(
                %target,
                status = %out.status,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "launchctl kickstart failed"
            );
            false
        }
        Err(e) => {
            warn!(error = %e, "could not run launchctl");
            false
        }
    }
}

/// The current user's uid, read from the home directory's owner: `launchctl`
/// addresses the per-user launchd domain as `gui/<uid>`, and std exposes no
/// direct getuid.
#[cfg(target_os = "macos")]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;

    let home = openlogi_core::paths::home_dir().ok()?;
    std::fs::metadata(home).ok().map(|meta| meta.uid())
}

/// The `.app` root of a packaged helper binary, `None` for a bare dev binary.
#[cfg(target_os = "macos")]
fn helper_bundle(path: &std::path::Path) -> Option<&std::path::Path> {
    let bundle = path.ancestors().nth(3)?;
    (bundle.extension()? == "app").then_some(bundle)
}

/// Resolve the agent executable relative to the running GUI: a sibling in the
/// cargo target dir (dev, and the flat Windows install layout), else the
/// embedded `OpenLogi Agent.app` login-item helper (packaged macOS build).
fn agent_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // EXE_SUFFIX, or the Windows lookup misses `openlogi-agent.exe` and the
    // spawn retry — the only agent restart path there — silently never works.
    let sibling = dir.join(format!("openlogi-agent{}", std::env::consts::EXE_SUFFIX));
    if sibling.exists() {
        return Some(sibling);
    }
    // Packaged: the login-item helper inside the outer bundle. Directories
    // carry the display name (the privacy panes' filename fallback shows it);
    // the last entry still finds pre-rename bundles.
    #[cfg(target_os = "macos")]
    {
        let contents = dir.parent()?;
        for relative in [
            "Library/LoginItems/OpenLogi Agent Dev.app/Contents/MacOS/openlogi-agent",
            "Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
            "Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent",
        ] {
            let helper = contents.join(relative);
            if helper.exists() {
                return Some(helper);
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn helper_bundle_resolves_only_the_packaged_layout() {
        let packaged = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle(packaged),
            Some(Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app"
            ))
        );
        let dev = Path::new(
            "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            helper_bundle(dev),
            Some(Path::new(
                "/Users/me/OpenLogi/target/dev/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent Dev.app"
            ))
        );
        assert_eq!(
            helper_bundle(Path::new("target/debug/openlogi-agent")),
            None
        );
    }
}
