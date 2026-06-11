//! Replace a running agent that speaks an older IPC protocol.
//!
//! The self-restart watcher (see [`crate::self_restart`]) keeps a *future*
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
//! SIGTERM, not a polite RPC: past protocols have no quit method. If the old
//! agent ran under launchd, dying by signal is a non-successful exit, so
//! launchd respawns it — from the bundle path, i.e. as the *new* binary —
//! and whichever copy loses the ensuing lock race exits cleanly. Either way
//! exactly one up-to-date agent survives.

#[cfg(unix)]
use std::time::Duration;

use openlogi_core::single_instance::InstanceGuard;
#[cfg(unix)]
use openlogi_core::single_instance::{self, InstanceError};
use tracing::info;
#[cfg(unix)]
use tracing::warn;

/// How long to wait for the protocol handshake against the lock holder. The
/// agent answers from memory; a holder that can't answer in this window is
/// wedged in a way we can't reason about, so leave it alone.
#[cfg(unix)]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait for the singleton lock after terminating the stale
/// holder (20 × 200 ms). SIGTERM delivery and process teardown are fast; the
/// budget mostly covers a slow exit under load.
#[cfg(unix)]
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

#[cfg(unix)]
fn replace_stale() -> Option<InstanceGuard> {
    use openlogi_agent_core::ipc::{AgentClient, PROTOCOL_VERSION};
    use tarpc::{client, context};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let holder_version = rt.block_on(async {
        let handshake = async {
            let stream = openlogi_agent_core::transport::connect().await.ok()?;
            let transport = openlogi_agent_core::transport::wrap(stream);
            let client = AgentClient::new(client::Config::default(), transport).spawn();
            client.protocol_version(context::current()).await.ok()
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
            .await
            .ok()
            .flatten()
    })?;
    drop(rt);

    if holder_version >= PROTOCOL_VERSION {
        // We are the duplicate (or the stale one — the GUI handles that
        // direction by telling the user to relaunch).
        return None;
    }
    info!(
        holder = holder_version,
        ours = PROTOCOL_VERSION,
        "lock holder speaks an older protocol — taking over"
    );

    let pids = agent_pids();
    if pids.is_empty() {
        // The holder answered the handshake a moment ago but no process is
        // findable now: it exited on its own (or pgrep is missing). Nothing
        // to signal either way — let the lock retry below decide whether the
        // lock is actually free, instead of exiting as a duplicate.
        info!("stale agent already gone — trying for its lock");
    } else {
        let mut signalled = false;
        for pid in pids {
            // `kill(1)` over raw libc: no unsafe, and the pid came from pgrep
            // moments ago — at worst the signal misses a just-exited process.
            let done = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .is_ok_and(|s| s.success());
            if done {
                info!(pid, "sent SIGTERM to the stale agent");
                signalled = true;
            } else {
                warn!(pid, "could not signal the stale agent");
            }
        }
        if !signalled {
            return None;
        }
    }

    let (attempts, delay) = LOCK_RETRY;
    for _ in 0..attempts {
        match single_instance::acquire("agent.lock") {
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

/// Pids of every other `openlogi-agent` process owned by this user. The
/// binary is named `openlogi-agent` in every install layout (cargo target
/// dir, the `OpenLogiAgent.app` login-item helper), and `pgrep -x` matches
/// the process name exactly; our own pid is excluded — `pkill` would signal
/// us too.
#[cfg(unix)]
fn agent_pids() -> Vec<u32> {
    let output = match std::process::Command::new("pgrep")
        .args(["-x", "openlogi-agent"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            warn!(error = %e, "pgrep unavailable — cannot locate the stale agent");
            return Vec::new();
        }
    };
    let own = std::process::id();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .collect()
}

/// No Windows release has ever shipped (or auto-started) the agent, so there
/// is no pre-watcher population to migrate; from the first shipped build
/// onward, `self_restart` exits on update and the GUI's spawn retry starts
/// the new binary.
#[cfg(windows)]
fn replace_stale() -> Option<InstanceGuard> {
    None
}
