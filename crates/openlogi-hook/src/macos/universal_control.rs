//! Whether Universal Control has moved the pointer to another Mac or iPad.
//!
//! While it has, Universal Control filters this Mac's own HID reports and
//! forwards them to the other device. Events posted with `CGEventPost` never
//! pass through that filter, so a wheel or button the agent diverted and
//! re-posts acts on this Mac instead of the one the pointer is on.
//!
//! Universal Control publishes its input state as the state of a per-user
//! Darwin notification. It holds 0 while input is local, sets the pointer
//! bit when the pointer leaves, and adds the keyboard bit when typing
//! follows (measured on macOS 26: 0, then 2, then 3, then 0 on return). The
//! name is not in any SDK header. If it ever stops being published, the
//! state reads 0 and nothing changes.
//!
//! libSystem's `notify(3)` API has no objc2 or `libc` binding, so its
//! `extern` declaration lives here, next to its only caller.

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;

unsafe extern "C" {
    /// `uint32_t notify_register_check(const char *name, int *out_token)` —
    /// `notify.h`, macOS 10.3+.
    fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    /// `uint32_t notify_get_state(int token, uint64_t *state)` — `notify.h`,
    /// macOS 10.5+.
    fn notify_get_state(token: c_int, state: *mut u64) -> u32;
}

/// `NOTIFY_STATUS_OK` in `notify.h`.
const NOTIFY_STATUS_OK: u32 = 0;

/// The bit Universal Control sets while the pointer is on another device.
const POINTER_ELSEWHERE: u64 = 0x2;

/// Whether the pointer is on another device, or `None` when the state
/// cannot be read.
pub(crate) fn pointer_on_other_device() -> Option<bool> {
    let token = token()?;
    let mut state = 0;
    // SAFETY: `token` came from a successful `notify_register_check`, and
    // `state` is a valid out-pointer for the call.
    let status = unsafe { notify_get_state(token, &raw mut state) };
    (status == NOTIFY_STATUS_OK).then_some(state & POINTER_ELSEWHERE != 0)
}

/// The process's one registration for the input-state name. It is never
/// cancelled: the agent reads it for its whole life.
fn token() -> Option<c_int> {
    static TOKEN: OnceLock<Option<c_int>> = OnceLock::new();
    *TOKEN.get_or_init(|| {
        // SAFETY: `getuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let name = CString::new(format!(
            "user.uid.{uid}.com.apple.universalcontrol.inputstate"
        ))
        .ok()?;
        let mut token = 0;
        // SAFETY: `name` is a NUL-terminated string that outlives the call,
        // and `token` is a valid out-pointer.
        let status = unsafe { notify_register_check(name.as_ptr(), &raw mut token) };
        (status == NOTIFY_STATUS_OK).then_some(token)
    })
}
