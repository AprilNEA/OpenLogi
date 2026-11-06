//! Named-pipe endpoint: one pipe per user and profile, ACLed to that user and
//! `SYSTEM`, which clients open with an impersonation ceiling and refuse
//! unless that user owns it.
//!
//! `\\.\pipe\` is one namespace for the whole machine, so every local
//! principal can see — and try to create — any pipe name. A fixed name with
//! the default descriptor left three holes on a shared machine:
//!
//! - **Squatting.** Whoever creates the name first owns it, and the name is
//!   no secret for carrying the user's SID. The agent now creates its first
//!   instance with `FILE_FLAG_FIRST_PIPE_INSTANCE` (an existing pipe fails
//!   the bind instead of being joined) and treats that failure as fatal
//!   rather than running on without an endpoint. Clients, for their part,
//!   check who owns the instance they opened: the agent stamps its own SID on
//!   the pipe, and giving an object an owner one does not hold takes
//!   `SeRestorePrivilege`, so a squatter's pipe is refused instead of spoken
//!   to.
//! - **Impersonation.** A pipe server may `ImpersonateNamedPipeClient` up to
//!   the level the client allowed, and a client that opens the pipe without
//!   `SECURITY_SQOS_PRESENT` allows full impersonation. Clients now open with
//!   `SECURITY_IDENTIFICATION`: the server can learn who connected, never act
//!   as them.
//! - **Reach.** The default pipe DACL grants `Everyone` read access, enough
//!   for another user to open instances and tie them up. The listener's DACL
//!   now names the owning user and `SYSTEM` only.

#![expect(
    unsafe_code,
    reason = "process-token, SID, security-info, and CreateFileW calls have no safe wrapper in interprocess or windows-sys"
)]

use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::ptr;
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, ToNsName as _};
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
use interprocess::os::windows::named_pipe::local_socket::tokio::Stream as NamedPipeStream;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use openlogi_core::paths::Profile;
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::core::PWSTR;

/// How long [`connect`] keeps retrying a pipe whose instances are all taken.
///
/// `interprocess` creates the next instance only after a client has taken the
/// current one, so clients arriving together (GUI, overlay, and CLI at login)
/// can find none free and get `ERROR_PIPE_BUSY` for a scheduler tick or two.
/// One second is far beyond that window and inside the client's handshake
/// timeout, so a pipe that stays busy fails the connect rather than hanging it.
const BUSY_RETRY: Duration = Duration::from_secs(1);

pub(super) fn endpoint_name() -> io::Result<Name<'static>> {
    pipe_name(&current_user_sid()?).to_ns_name::<GenericNamespaced>()
}

pub(super) fn bind() -> io::Result<Listener> {
    let sid = current_user_sid()?;
    bind_named(&pipe_name(&sid), &sid)
}

pub(super) async fn connect() -> io::Result<Stream> {
    let sid = current_user_sid()?;
    connect_named(&pipe_name(&sid), &sid).await
}

/// The pipe's name within `\\.\pipe\`: the agent's, for this user and this
/// profile alone. The profile stem is the one the XDG directories use, so a
/// dev agent cannot claim the shipped app's endpoint on Windows any more than
/// on Unix.
fn pipe_name(sid: &str) -> String {
    format!("{}-agent-{sid}", Profile::current().app_dir())
}

/// Listen on `name` as `sid`: the pipe is owned by `sid` and reachable by
/// `sid` and `SYSTEM` alone. [`bind`] fixes the name; tests pick their own so
/// they never contend with a running agent.
fn bind_named(name: &str, sid: &str) -> io::Result<Listener> {
    let descriptor = SecurityDescriptor::deserialize(&owner_and_system_sddl(sid)?)?;
    ListenerOptions::new()
        .name(name.to_ns_name::<GenericNamespaced>()?)
        .security_descriptor(descriptor)
        .create_tokio()
}

/// Open `name` as a client of `sid`: with the impersonation ceiling, and only
/// if `sid` owns the pipe. [`connect`] fixes both; tests pick their own.
async fn connect_named(name: &str, sid: &str) -> io::Result<Stream> {
    let path = pipe_path(name)?;
    let sid = sid.to_owned();
    let handle = tokio::task::spawn_blocking(move || {
        let handle = open_pipe(&path)?;
        check_owner(&handle, &sid)?;
        Ok::<_, io::Error>(handle)
    })
    .await
    .map_err(io::Error::other)??;
    // The handle-to-stream conversion lives on the named-pipe flavour, not
    // on the cross-platform enum that wraps it.
    Ok(Stream::from(NamedPipeStream::try_from(handle)?))
}

/// The full `\\.\pipe\…` path `CreateFileW` and `WaitNamedPipeW` take.
fn pipe_path(name: &str) -> io::Result<U16CString> {
    U16CString::from_str(format!(r"\\.\pipe\{name}")).map_err(io::Error::other)
}

/// Owner `sid`, and a protected DACL granting full access to `SYSTEM` and
/// `sid` and nothing to anyone else. The owner is set explicitly because
/// clients check it ([`check_owner`]) and an elevated token would otherwise
/// default it to `Administrators`.
fn owner_and_system_sddl(sid: &str) -> io::Result<U16CString> {
    U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;{sid})")).map_err(io::Error::other)
}

/// Open the pipe the way every client of the agent must: duplex, overlapped
/// (tokio drives the handle), and with the impersonation ceiling at
/// `SecurityIdentification`, so the server — whoever it turns out to be — can
/// learn who connected but never act as them.
///
/// `ERROR_PIPE_BUSY` means every instance is taken for the moment (see
/// [`BUSY_RETRY`]); `WaitNamedPipeW` parks the caller until one frees up.
/// Every other failure — no agent listening, access denied — is final.
fn open_pipe(path: &U16CStr) -> io::Result<OwnedHandle> {
    let deadline = Instant::now() + BUSY_RETRY;
    loop {
        // SAFETY: `path` is NUL-terminated and outlives the call; no security
        // attributes or template handle are passed.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: `handle` is a freshly opened handle this process owns
            // and nothing else closes.
            return Ok(unsafe { OwnedHandle::from_raw_handle(handle) });
        }
        let err = io::Error::last_os_error();
        if !is_win32(&err, ERROR_PIPE_BUSY) {
            return Err(err);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "every instance of the agent's pipe stayed busy",
            ));
        }
        // `WaitNamedPipeW` reads a timeout of 0 as "the server's default", so
        // the wait is at least a millisecond and never the forever sentinel;
        // a wait that fails (timed out, or the pipe vanished) just makes the
        // next `CreateFileW` decide.
        let timeout = u32::try_from(remaining.as_millis()).unwrap_or(1).max(1);
        // SAFETY: `path` is NUL-terminated and outlives the call.
        unsafe { WaitNamedPipeW(path.as_ptr(), timeout) };
    }
}

/// Refuse a pipe that `sid` does not own. The name is public, so whoever
/// created the pipe first answers on it; the owner is the one fact about it
/// a squatter cannot forge (see the module docs).
fn check_owner(handle: &OwnedHandle, sid: &str) -> io::Result<()> {
    let owner = pipe_owner_sid(handle)?;
    if owner == sid {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the agent's pipe is owned by {owner}, not by the current user"),
    ))
}

/// The SID that owns the pipe instance behind `handle`, as a string.
fn pipe_owner_sid(handle: &OwnedHandle) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is an open pipe handle with `READ_CONTROL` (part of
    // `GENERIC_READ`); `owner` and `descriptor` outlive the call, and the
    // group, DACL, and SACL out-pointers may be null when not requested.
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error(status));
    }
    // `owner` points into `descriptor`, so it is read before the free below.
    let sid = if owner.is_null() {
        Err(io::Error::other(
            "the pipe's security descriptor names no owner",
        ))
    } else {
        sid_to_string(owner)
    };
    // SAFETY: on success `descriptor` came from `LocalAlloc` inside
    // `GetSecurityInfo`, and neither it nor `owner` is used after this.
    unsafe { LocalFree(descriptor) };
    sid
}

/// The caller's user SID as a string (`S-1-5-21-…`), read from the process
/// token.
fn current_user_sid() -> io::Result<String> {
    let token = open_process_token()?;
    let buffer = token_user(&token)?;
    // SAFETY: `token_user` returned a buffer the kernel filled with at least
    // `size_of::<TOKEN_USER>()` bytes; `u64` alignment satisfies the
    // pointer-sized alignment of `TOKEN_USER`, which is plain old data.
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read() };
    // `user.User.Sid` points into `buffer`, which stays alive past this call.
    sid_to_string(user.User.Sid)
}

/// The current process's token, opened for querying.
fn open_process_token() -> io::Result<OwnedHandle> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` is a pseudo-handle that needs no closing;
    // `token` outlives the call and receives a real handle on success.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a freshly opened handle this process owns and
    // nothing else closes.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// The token's `TOKEN_USER` block — the struct plus the SID it points into —
/// in a buffer sized by the kernel.
fn token_user(token: &OwnedHandle) -> io::Result<Vec<u64>> {
    let mut needed: u32 = 0;
    // SAFETY: a null buffer of length 0 is the documented size probe; `needed`
    // outlives the call.
    let probed = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    if probed == 0 {
        let err = io::Error::last_os_error();
        if !is_win32(&err, ERROR_INSUFFICIENT_BUFFER) {
            return Err(err);
        }
    }
    let len = usize::try_from(needed).map_err(io::Error::other)?;
    if len < size_of::<TOKEN_USER>() {
        return Err(io::Error::other(
            "GetTokenInformation reported a TOKEN_USER smaller than the struct",
        ));
    }
    let mut buffer = vec![0u64; len.div_ceil(size_of::<u64>())];
    let mut written: u32 = 0;
    // SAFETY: `buffer` is writable for `needed` bytes (rounded up to whole
    // `u64`s) and `written` outlives the call.
    let filled = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut written,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// `sid` in its `S-1-…` string form, via the system's own formatter.
fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut text: PWSTR = ptr::null_mut();
    // SAFETY: `sid` points at a SID in memory the caller still holds; `text`
    // outlives the call.
    let converted = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `text` is a NUL-terminated UTF-16 string from
    // `LocalAlloc`, which this function frees exactly once below.
    let string = unsafe { U16CStr::from_ptr_str(text) }.to_string_lossy();
    // SAFETY: `text` came from `LocalAlloc` and is not used after this.
    unsafe { LocalFree(text.cast()) };
    Ok(string)
}

/// Whether `err` is the Win32 error `code` (`io::Error` keeps it as `i32`).
fn is_win32(err: &io::Error, code: u32) -> bool {
    i32::try_from(code).is_ok_and(|code| err.raw_os_error() == Some(code))
}

/// An `io::Error` for a Win32 error `code` a call returned by value rather
/// than left as the thread's last error.
fn win32_error(code: u32) -> io::Error {
    i32::try_from(code).map_or_else(
        |_| io::Error::other(format!("Win32 error {code}")),
        io::Error::from_raw_os_error,
    )
}

#[cfg(test)]
mod tests {
    use interprocess::local_socket::traits::tokio::Listener as _;

    use super::*;

    /// A pipe name of this test's own, so the tests never contend with an
    /// agent running on the machine.
    fn test_pipe_name(test: &str) -> String {
        format!("openlogi-test-{test}-{}", std::process::id())
    }

    #[test]
    fn current_user_sid_is_a_sid_string() {
        let sid = current_user_sid().expect("the process token is readable");
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn owner_and_system_sddl_parses() {
        let sid = current_user_sid().expect("the process token is readable");
        let sddl = owner_and_system_sddl(&sid).expect("a SID has no NUL");
        SecurityDescriptor::deserialize(&sddl).expect("valid SDDL");
    }

    /// The owner reaches their own pipe through the identification-level
    /// open — the DACL, the SQOS flags, and the owner check must not lock the
    /// legitimate client out.
    #[tokio::test]
    async fn owner_connects_to_own_pipe() {
        let sid = current_user_sid().expect("the process token is readable");
        let name = test_pipe_name("owner-connects");
        let listener = bind_named(&name, &sid).expect("bind the per-user pipe");
        let accepted = tokio::spawn(async move { listener.accept().await });
        let _client = connect_named(&name, &sid)
            .await
            .expect("connect to the per-user pipe");
        accepted
            .await
            .expect("accept task joins")
            .expect("the listener accepts the owner");
    }

    /// A pipe owned by anyone else is refused: the name is public, so the
    /// owner is the only proof of who answers on it.
    #[tokio::test]
    async fn connect_refuses_a_pipe_owned_by_another_sid() {
        let sid = current_user_sid().expect("the process token is readable");
        let name = test_pipe_name("foreign-owner");
        let _listener = bind_named(&name, &sid).expect("bind the per-user pipe");
        // The NULL SID: no process token's user, so never this test's.
        let err = connect_named(&name, "S-1-0-0")
            .await
            .expect_err("a pipe with a foreign owner is refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    }
}
