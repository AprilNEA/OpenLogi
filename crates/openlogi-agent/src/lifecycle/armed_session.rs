//! Which login session last armed the agent, so a launchd respawn inside it
//! re-arms instead of going dormant.
//!
//! The dormancy gate cannot tell *why* launchd started it: the service plist
//! carries one trigger for login and crash alike (`SuccessfulExit` implies
//! `RunAtLoad`). With `launch_at_login` off, a crash respawn — the hook
//! watchdog's `exit(78)` on a sleep transition, a panic — used to wait out the
//! dormant window and `exit(0)`, which launchd never respawns: remaps stayed
//! dead until the user opened the GUI by hand.
//!
//! The question the gate needs answered is "was an agent armed earlier in this
//! login session and not asked to leave?". Arming records the session here,
//! every final exit erases it, and an exit that hands over to a successor
//! (binary update, Input Monitoring relaunch) leaves it. So a start that finds
//! its own session recorded follows an unclean exit or a handover, and a start
//! in a new session — a login — finds nothing or a stale record and stays
//! dormant.
//!
//! A session is the kernel boot plus the audit session: an audit session id is
//! only unique within one boot, and the boot alone would re-arm a login the
//! user never asked for after logging out and back in.

#![expect(
    unsafe_code,
    reason = "two out-pointer reads: sysctlbyname and SessionGetInfo"
)]

use std::io;
use std::path::PathBuf;

use objc2_security::{SessionAttributeBits, SessionGetInfo, callerSecuritySession};
use tracing::{debug, warn};

/// Whether this start follows an agent armed earlier in the same login
/// session that did not leave through a final exit.
pub(super) fn rearm() -> bool {
    let (Some(record), Some(session)) = (Record::open(), LoginSession::current()) else {
        return false;
    };
    record.holds(&session)
}

/// Record the current login session as armed. A failure costs only crash
/// recovery, so it is logged, not propagated.
pub(super) fn record() {
    let (Some(record), Some(session)) = (Record::open(), LoginSession::current()) else {
        return;
    };
    if let Err(error) = record.write(&session) {
        warn!(%error, path = %record.path.display(), "could not record the armed session — a crash respawn will stay dormant");
    }
}

/// Forget the armed session: this exit was asked for, and the next start must
/// not mistake it for a crash. Also the tray-Quit fallback's job when the
/// lifecycle that would normally do it is already gone.
pub(crate) fn clear() {
    if let Some(record) = Record::open() {
        record.clear();
    }
}

/// The login session a process runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LoginSession {
    boot: String,
    audit_session: u32,
}

impl LoginSession {
    /// Read the current session, or `None` if either half is unavailable —
    /// then no respawn can be recognized and the gate falls back to dormancy.
    fn current() -> Option<Self> {
        let boot = boot_session_uuid()
            .inspect_err(|error| warn!(%error, "could not read the boot session"))
            .ok()?;
        let audit_session = audit_session_id()
            .inspect_err(|status| warn!(status, "could not read the audit session"))
            .ok()?;
        Some(Self {
            boot,
            audit_session,
        })
    }

    fn encode(&self) -> String {
        format!("{} {}\n", self.boot, self.audit_session)
    }

    fn decode(text: &str) -> Option<Self> {
        let (boot, audit_session) = text.trim_end().split_once(' ')?;
        Some(Self {
            boot: boot.to_owned(),
            audit_session: audit_session.parse().ok()?,
        })
    }
}

/// The on-disk record of the armed session. It lives in the runtime dir,
/// next to the agent's socket, so each profile keeps its own.
struct Record {
    path: PathBuf,
}

impl Record {
    fn open() -> Option<Self> {
        let dir = openlogi_core::paths::runtime_dir()
            .inspect_err(|error| warn!(%error, "no runtime dir for the armed-session record"))
            .ok()?;
        Some(Self {
            path: dir.join("armed-session"),
        })
    }

    fn holds(&self, session: &LoginSession) -> bool {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => LoginSession::decode(&text).is_some_and(|recorded| recorded == *session),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                warn!(%error, path = %self.path.display(), "could not read the armed-session record");
                false
            }
        }
    }

    /// Written beside the record and renamed into place, so a kill mid-write
    /// leaves the previous record rather than a truncated one. The singleton
    /// lock makes this process the only writer.
    fn write(&self, session: &LoginSession) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let staged = self.path.with_extension("tmp");
        std::fs::write(&staged, session.encode())?;
        std::fs::rename(&staged, &self.path)
    }

    fn clear(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => debug!(path = %self.path.display(), "cleared the armed-session record"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(%error, path = %self.path.display(), "could not clear the armed-session record");
            }
        }
    }
}

/// `kern.bootsessionuuid`: fresh on every boot, unlike the audit session id.
fn boot_session_uuid() -> io::Result<String> {
    // A UUID string and its NUL take 37 bytes; leave headroom.
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: the name is a NUL-terminated literal; `buf` and `len` describe a
    // live writable buffer, which the kernel fills up to `len` before storing
    // the byte count back into `len`. Nothing is written (null new value, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    buf.get(..len)
        .and_then(|bytes| bytes.split(|&b| b == 0).next())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("kern.bootsessionuuid is not a UTF-8 string"))
}

/// The audit session this process belongs to — the login that launchd's
/// `gui/<uid>` domain serves.
fn audit_session_id() -> Result<u32, i32> {
    let mut id = 0;
    // SAFETY: `callerSecuritySession` asks about this process's own session;
    // `id` is a live out-pointer, and the attribute out-pointer may be null.
    let status = unsafe {
        SessionGetInfo(
            callerSecuritySession,
            &raw mut id,
            std::ptr::null_mut::<SessionAttributeBits>(),
        )
    };
    if status == 0 { Ok(id) } else { Err(status) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: &str = "C3E655FA-3500-45F4-BAFE-01DA6C16EF21";

    fn session(boot: &str, audit_session: u32) -> LoginSession {
        LoginSession {
            boot: boot.to_owned(),
            audit_session,
        }
    }

    #[test]
    fn only_the_recorded_session_is_rearmed() {
        let dir = tempfile::tempdir().unwrap();
        let record = Record {
            path: dir.path().join("nested").join("armed-session"),
        };
        let here = session(BOOT, 100_023);
        assert!(!record.holds(&here), "nothing recorded yet");

        record.write(&here).unwrap();
        assert!(record.holds(&here), "a respawn in the same login re-arms");
        assert!(
            !record.path.with_extension("tmp").exists(),
            "the staged copy is renamed into place, not left behind"
        );
        assert!(
            !record.holds(&session(BOOT, 100_024)),
            "a new login in the same boot stays dormant"
        );
        assert!(
            !record.holds(&session("0D8F2B6A-1111-4C0E-9A7E-3B5C0A1D2E3F", 100_023)),
            "an audit id reused after a reboot stays dormant"
        );

        record.clear();
        assert!(!record.holds(&here), "a final exit forgets the session");
        record.clear();
    }

    #[test]
    fn a_malformed_record_is_never_armed() {
        let dir = tempfile::tempdir().unwrap();
        let record = Record {
            path: dir.path().join("armed-session"),
        };
        let here = session(BOOT, 100_023);
        for garbage in ["", "\n", "no-space", "C3E655FA not-a-number\n"] {
            std::fs::write(&record.path, garbage).unwrap();
            assert!(!record.holds(&here), "{garbage:?} is not a session");
        }
    }

    #[test]
    fn the_current_session_is_readable_and_round_trips() {
        let first = LoginSession::current().expect("a macOS process has a login session");
        assert_eq!(first.boot.len(), 36, "a UUID string: {:?}", first.boot);
        assert_eq!(LoginSession::current(), Some(first.clone()));
        assert_eq!(LoginSession::decode(&first.encode()), Some(first));
    }
}
