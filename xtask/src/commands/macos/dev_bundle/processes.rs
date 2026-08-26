//! What is already running when a dev build starts, and what to do about it.
//!
//! The dev agent is launched through LaunchServices (`open -g -n`, so it is its
//! own TCC responsible process), which also means it is not a child of the GUI
//! and not in the terminal's process group: closing the window or pressing
//! Ctrl-C ends the GUI and leaves the agent and its overlay running, invisible
//! but for the menu-bar icon. Leaving them is not neutral either — this command
//! rewrites the binaries the resident agent watches, so ~20 s into the session
//! its own update watcher would relaunch it, mid-test. Stopping them first
//! makes every run start from a known state.

use std::path::Path;

use anyhow::{Result, bail};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// How long to wait for a signalled process to actually go, so the agent the
/// GUI is about to spawn does not lose the singleton lock to a corpse.
const EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);
const EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The processes this checkout owns.
const OURS: [&str; 2] = ["openlogi-agent", "openlogi-overlay"];

/// Stop this checkout's leftovers, and refuse to share the machine with an
/// agent from anywhere else.
///
/// An agent whose executable lives outside `app` and `target` is the
/// developer's production install: the GUI would connect to *that* one and make
/// GUI+agent testing meaningless, so it is reported rather than killed.
/// `OPENLOGI_ALLOW_EXTERNAL_AGENT=1` says the developer meant it.
pub(super) fn reap_leftovers(app: &Path, target: &Path) -> Result<()> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );

    let mut ours = Vec::new();
    let mut external = Vec::new();
    for (pid, process) in system.processes() {
        let Some(name) = process.name().to_str() else {
            continue;
        };
        if !OURS.contains(&name) {
            continue;
        }
        let Some(exe) = process.exe() else {
            continue;
        };
        if exe.starts_with(app) || exe.starts_with(target) {
            ours.push(*pid);
        } else if name == "openlogi-agent" {
            external.push((*pid, exe.to_path_buf()));
        }
    }

    if !external.is_empty() && std::env::var("OPENLOGI_ALLOW_EXTERNAL_AGENT").as_deref() != Ok("1")
    {
        let listed = external
            .iter()
            .map(|(pid, exe)| format!("  pid {pid}: {}", exe.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let service = crate::commands::macos::bundle::agent_service_label(
            crate::commands::macos::bundle::identity::Channel::Production,
        );
        bail!(
            "an external openlogi-agent is already running.\n\n\
             The dev GUI would connect to that agent instead of the freshly built dev\n\
             agent, which makes GUI+agent testing misleading. Stop it first: Quit from\n\
             its menu-bar icon, or\n\n  \
             pkill -x openlogi-agent\n\n\
             (for an install registered as a login item, also:\n  \
             launchctl bootout \"gui/$(id -u)/{service}\")\n\n\
             Running external agent(s):\n{listed}\n\n\
             If this is intentional, rerun with OPENLOGI_ALLOW_EXTERNAL_AGENT=1."
        );
    }

    if ours.is_empty() {
        return Ok(());
    }
    for pid in &ours {
        if let Some(process) = system.process(*pid) {
            let _ = process.kill_with(Signal::Term);
        }
    }
    println!(
        "==> stopped {} leftover dev process(es) from an earlier run",
        ours.len()
    );
    wait_for_exit(&ours);
    Ok(())
}

/// Poll until every signalled pid is gone, or the deadline passes.
///
/// A survivor is reported, not an error: the run is still worth finishing, and
/// the developer needs to know why the agent may lose the singleton lock.
fn wait_for_exit(pids: &[sysinfo::Pid]) {
    let mut system = System::new();
    let started = std::time::Instant::now();
    while started.elapsed() < EXIT_DEADLINE {
        system.refresh_processes(ProcessesToUpdate::Some(pids), true);
        if pids.iter().all(|pid| system.process(*pid).is_none()) {
            return;
        }
        std::thread::sleep(EXIT_POLL);
    }
    println!(
        "    warning: a leftover dev process is still running; the new agent may lose the lock"
    );
}
