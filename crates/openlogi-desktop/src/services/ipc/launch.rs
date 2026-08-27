//! Bringing the agent up when the socket is unreachable.
//!
//! On macOS this is a preference cascade, supervised-first: `launchctl
//! kickstart` the registered launchd service, register the service on demand
//! when it is absent (registration *is* the supervised start), and only then
//! the legacy direct-launch paths — `open -g -n` for the packaged helper,
//! a `disclaim` exec for a bare binary — so a broken registration degrades
//! instead of wedging the retry loop. Elsewhere only the direct launch
//! exists. The suite-quitting flag keeps the unreachable→spawn reflex from
//! resurrecting an agent the user deliberately quit.

use std::path::PathBuf;

use tracing::{info, warn};

/// Set when the agent announced a deliberate suite shutdown (the tray's Quit
/// arrives as the `openlogi://quit` deep link before the agent exits). The
/// spawn fallback below must not resurrect a process the user just quit: the
/// agent's socket can close while this GUI is still tearing down, and the
/// unreachable→spawn reflex has been observed winning that race and bringing
/// the agent back seconds after Quit. Never cleared — this process is
/// quitting too. Automatic recovery is for faults, not for user intent.
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
    // A registered launchd service is launchd's to start: kickstart is
    // idempotent (a no-op on a running service), the process comes up
    // *supervised* (crash respawn per the service plist), and launchd makes it
    // its own TCC responsible process. Every failure falls through rung by
    // rung to the legacy direct-launch paths below, so a broken registration
    // degrades to today's behavior instead of wedging the retry loop.
    #[cfg(target_os = "macos")]
    {
        if kickstart_registered_agent() {
            return;
        }
        // Second rung: the service may simply never have been registered —
        // on a fresh install this reflex outruns the backgrounded startup
        // ensure, and the direct launch below would bring up an *unmanaged*
        // agent that then shadows the service for the rest of the session (a
        // same-version takeover never displaces it). Registering here IS the
        // supervised start: `register` bootstraps the job immediately. Dev
        // profiles never register implicitly (a login item pointing into
        // `target/` goes stale); the master switch and register failures fall
        // through, with the re-run kickstart doubling as the "did the
        // registration actually take?" check.
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
        }
    }
    let Some(path) = agent_binary_path() else {
        warn!(
            "agent not reachable and its binary wasn't found next to the GUI — \
             start it via launchd or by hand"
        );
        return;
    };
    // Spawn the agent under its *own* macOS TCC identity, not the GUI's:
    // otherwise it inherits the GUI's responsibility and the Accessibility /
    // Input-Monitoring grants the user gave the agent look missing (#192, #214).
    // The packaged helper goes through LaunchServices so it is its own TCC
    // responsible process; everything else is a `disclaim` exec (a no-op
    // pass-through to `std::process::Command` off macOS).
    // "started", not "launched": on the packaged path success here only means
    // `open` was handed the bundle — the waiter inside `launch_agent` reports
    // the definitive outcome, so a LaunchServices rejection is not preceded by
    // a success claim it then contradicts.
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
        // `open` exits as soon as it hands the bundle to LaunchServices, and
        // its exit status is the only signal that the handoff failed (damaged
        // bundle, LaunchServices refusal) — a successful spawn alone proves
        // nothing. Reap it off-thread and log the failure the spawn hides.
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
/// switched it off in Login Items, or launchctl itself failed) sends the
/// caller down the direct-launch paths.
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
                "launchctl kickstart failed — falling back to a direct launch"
            );
            false
        }
        Err(e) => {
            warn!(error = %e, "could not run launchctl — falling back to a direct launch");
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
    // spawn retry — the only thing that restarts an updated agent there, since
    // Windows has no exec and the Run-key autostart only fires at login —
    // silently never works.
    let sibling = dir.join(format!("openlogi-agent{}", std::env::consts::EXE_SUFFIX));
    if sibling.exists() {
        return Some(sibling);
    }
    // Packaged: …/OpenLogi.app/Contents/MacOS/openlogi-desktop → the helper at
    // …/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent
    // Every family names its directory after the display name, so the privacy
    // panes' filename fallback (used when bundle metadata is stale) shows the
    // real name. The last entry keeps finding helpers in bundles built before
    // the rename.
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
