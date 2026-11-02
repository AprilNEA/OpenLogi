//! Replace a running agent that speaks an older IPC protocol.
//!
//! The binary watcher (see [`crate::binary_watch`]) keeps a *future*
//! stale agent from outliving an update — but the watcher only exists in
//! binaries that ship it, so the first protocol bump still strands every
//! user whose pre-watcher agent is running: it never exits, launchd only
//! acts on exit, it holds the singleton lock so every freshly-spawned new
//! agent loses and quits, and the new GUI refuses the old protocol — parking
//! the user on the connecting screen until the next login.
//!
//! The escape hatch runs in the *new* agent: when it loses the singleton
//! lock, it connects to the IPC socket as a client and asks the lock holder
//! for its protocol version. If the holder is provably older, the holder is
//! a leftover from before the update — terminate it and take the lock. The
//! `protocol_version` handshake is wire-stable across versions (method 0,
//! plain `u32`), so this works against any past agent. A holder that is the
//! same version or newer means *we* are the duplicate (or the stale one),
//! and we exit as before.
//!
//! SIGTERM, not a polite RPC: past protocols have no quit method. A holder
//! too old to handle the signal dies by it, which under launchd is a
//! non-successful exit, so launchd respawns it — from the bundle path, i.e.
//! as the *new* binary — and whichever copy loses the ensuing lock race exits
//! cleanly. A holder new enough to handle SIGTERM releases its event tap and
//! exits 0, which launchd leaves alone, so the lock falls to us. Either way
//! exactly one up-to-date agent survives.
//!
//! Windows has no signals, so the holder is terminated outright
//! (`TerminateProcess`). Nothing respawns it there — the Run key fires once
//! per login — and the OS drops its file lock and named-pipe instance with
//! the process, so the lock retry below picks the lock up and `server::run`
//! binds the pipe fresh.

use std::time::Duration;

use openlogi_core::single_instance::{self, InstanceError, InstanceGuard, Role};
use tracing::{info, warn};

/// How long to wait for the singleton lock after terminating the stale
/// holder (20 × 200 ms). SIGTERM delivery and process teardown are fast; the
/// budget mostly covers a slow exit under load.
const LOCK_RETRY: (u32, Duration) = (20, Duration::from_millis(200));

/// Try to replace the agent currently holding `agent.lock`, returning the
/// acquired lock guard on success. `None` means the holder stays (it is
/// current or newer, unreachable, or couldn't be terminated) and the caller
/// should exit as a duplicate.
pub fn try_replace_stale() -> Option<InstanceGuard> {
    if cfg!(debug_assertions) {
        // A dev agent losing the lock to the user's production agent is the
        // *expected* dev workflow; a debug build must never displace it.
        info!("debug build — leaving the running agent in place");
        return None;
    }
    replace_stale()
}

fn replace_stale() -> Option<InstanceGuard> {
    use openlogi_ipc::client::{self, ProtocolSkew};
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let rt = openlogi_core::worker::runtime().ok()?;
    // The probe carries the shared handshake deadline: a holder that cannot
    // answer within it is wedged in a way we cannot reason about, so it is
    // left alone like an unreachable one.
    let holder_version = rt.block_on(client::probe_version()).ok()?;
    drop(rt);

    // Only an older holder is ours to replace. The same version makes us the
    // duplicate; a newer one makes us the stale side, which the GUI handles by
    // telling the user to relaunch.
    let Err(skew @ ProtocolSkew::AgentOlder { .. }) = ProtocolSkew::check(holder_version) else {
        return None;
    };
    info!(%skew, "lock holder speaks an older protocol — taking over");

    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own_pid = Pid::from_u32(std::process::id());
    // sysinfo names a Windows process by its image file, `.exe` included.
    let agent_name = format!(
        "{}{}",
        openlogi_core::brand::Helper::Agent.executable(),
        std::env::consts::EXE_SUFFIX
    );
    let stale_agents = system
        .processes_by_exact_name(std::ffi::OsStr::new(&agent_name))
        .filter(|process| process.pid() != own_pid);

    let mut found = false;
    let mut terminated = false;
    for process in stale_agents {
        found = true;
        let pid = process.pid().as_u32();
        if terminate(process) {
            info!(pid, "terminated the stale agent");
            terminated = true;
        } else {
            warn!(pid, "could not terminate the stale agent");
        }
    }

    if !found {
        // The holder answered the handshake a moment ago but no process is
        // findable now: it exited on its own. Nothing to terminate either way —
        // let the lock retry below decide whether the lock is actually free,
        // instead of exiting as a duplicate.
        info!("stale agent already gone — trying for its lock");
    } else if !terminated {
        return None;
    }

    let (attempts, delay) = LOCK_RETRY;
    for _ in 0..attempts {
        match single_instance::acquire(Role::Agent) {
            Ok(guard) => return Some(guard),
            Err(InstanceError::AlreadyRunning { .. }) => std::thread::sleep(delay),
            Err(e) => {
                warn!(error = %e, "single-instance retry failed during takeover");
                return None;
            }
        }
    }
    warn!("stale agent did not release the lock — giving up the takeover");
    None
}

/// Ask `process` to exit: SIGTERM on Unix, so a holder new enough to handle
/// it shuts down cleanly; `TerminateProcess` on Windows, which has no signals
/// (sysinfo answers `kill_with(Signal::Term)` with `None` there).
fn terminate(process: &sysinfo::Process) -> bool {
    if cfg!(windows) {
        process.kill()
    } else {
        process.kill_with(sysinfo::Signal::Term) == Some(true)
    }
}
