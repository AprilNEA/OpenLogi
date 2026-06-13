//! Spawn the background agent so macOS evaluates its privacy (TCC) permissions
//! against the agent's *own* code signature, not the GUI's.
//!
//! `std::process::Command` launches a child that inherits the parent's TCC
//! "responsibility": macOS attributes the spawned agent's Accessibility and
//! Input-Monitoring checks to the GUI bundle (`org.openlogi.openlogi`), not to
//! the agent bundle (`org.openlogi.agent`) where the user actually granted
//! them. A GUI-spawned agent then reports those permissions as missing even
//! though System Settings shows OpenLogi enabled — it draws the "Accessibility
//! required" screen and opens no devices (issue #214).
//!
//! `responsibility_spawnattrs_setdisclaim` makes the child disclaim that
//! inheritance, so TCC judges it on its own identity — the same identity a
//! launchd-started agent already gets. With both the GUI-spawned and the
//! launchd-respawned agent resolving to `org.openlogi.agent`, the post-update
//! takeover race can no longer strand the user on the wrong identity. The
//! symbol is a private libSystem export (used by Chromium, Sparkle, et al.)
//! stable since macOS 10.14.

#![expect(
    unsafe_code,
    reason = "posix_spawn + the private responsibility_spawnattrs_setdisclaim FFI"
)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use libc::{c_char, c_int, pid_t, posix_spawnattr_t};

unsafe extern "C" {
    /// Private libSystem call: with `disclaim` non-zero, a child spawned under
    /// `*attrs` does not inherit the caller's TCC responsibility and is judged
    /// against its own code signature. The Apple `spawn_private.h` signature
    /// takes the attribute **by pointer** (`posix_spawnattr_t *`); passing it by
    /// value returns `EINVAL` on current macOS, i.e. the disclaim silently never
    /// happens.
    fn responsibility_spawnattrs_setdisclaim(
        attrs: *mut posix_spawnattr_t,
        disclaim: c_int,
    ) -> c_int;
}

/// Launch `program` (no arguments, inheriting the current environment) as a
/// detached child whose macOS privacy permissions are evaluated against its own
/// bundle identity rather than this process's.
///
/// # Errors
/// Returns the underlying `posix_spawn` / attribute error, or `InvalidInput` if
/// the path contains an interior NUL. A failed *disclaim* is logged, not
/// returned — the agent still launches, just (as before) under this process's
/// identity.
pub fn spawn_disclaiming_responsibility(program: &Path) -> io::Result<()> {
    spawn_inner(program).map(|_pid| ())
}

fn spawn_inner(program: &Path) -> io::Result<pid_t> {
    let path = CString::new(program.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent path contains an interior NUL byte",
        )
    })?;
    // posix_spawn wants NUL-terminated, `*mut`-typed argv/envp. The inner
    // pointers are never written through; the `*mut` is a C-ABI relic.
    let argv = [path.as_ptr().cast_mut(), ptr::null_mut::<c_char>()];
    // SAFETY: `_NSGetEnviron` returns a valid, non-null pointer to the live
    // `environ` for the whole process lifetime; we only read it here.
    let envp: *const *mut c_char = unsafe { *libc::_NSGetEnviron() };

    let mut attr: posix_spawnattr_t = ptr::null_mut();
    // SAFETY: `&raw mut attr` is a valid out-pointer; `posix_spawnattr_init`
    // initializes the attribute handle into it.
    let rc = unsafe { libc::posix_spawnattr_init(&raw mut attr) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }

    // SAFETY: `attr` was just initialized; `&raw mut attr` is the
    // `posix_spawnattr_t *` the call expects; it only flips the disclaim flag.
    let disclaim_rc = unsafe { responsibility_spawnattrs_setdisclaim(&raw mut attr, 1) };
    if disclaim_rc != 0 {
        // Non-fatal: a running agent under the wrong TCC identity still beats no
        // agent at all. Surface it so a dropped symbol shows up in the logs.
        tracing::warn!(
            code = disclaim_rc,
            "could not disclaim agent responsibility — its TCC identity may be wrong"
        );
    }

    let mut pid: pid_t = 0;
    // SAFETY: `pid` is writable; `path`, `argv`, and `envp` are valid and
    // NUL-terminated and outlive the call; `&attr` points at the initialized
    // attribute; no file actions are requested.
    let spawn_rc = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            path.as_ptr(),
            ptr::null(),
            &raw const attr,
            argv.as_ptr(),
            envp,
        )
    };

    // SAFETY: `attr` was initialized by `posix_spawnattr_init` above (the
    // init-failure path returned early, before this), so destroying it here is
    // valid and happens exactly once — on both the spawn-success and -failure
    // paths. POSIX forbids destroying an attr whose init failed, which is why
    // the early return must *not* reach this.
    unsafe { libc::posix_spawnattr_destroy(&raw mut attr) };

    if spawn_rc == 0 {
        Ok(pid)
    } else {
        Err(io::Error::from_raw_os_error(spawn_rc))
    }
}

#[cfg(test)]
mod tests {
    use super::spawn_inner;
    use std::path::Path;

    /// Exercises the whole posix_spawn plumbing (argv/envp/attr) against a real
    /// binary — a botched NUL terminator or envp cast would fail the spawn here.
    /// `/usr/bin/true` exits 0 at once; we reap it so the test leaves no zombie.
    #[test]
    fn spawns_and_reaps_a_real_binary() {
        let pid = match spawn_inner(Path::new("/usr/bin/true")) {
            Ok(pid) => pid,
            Err(e) => panic!("posix_spawn of /usr/bin/true failed: {e}"),
        };
        let mut status: i32 = 0;
        // SAFETY: reaping the child we just spawned, by its own pid.
        let reaped = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert_eq!(reaped, pid, "waitpid should reap exactly the spawned child");
    }
}
