//! How long the keyboards have gone without a key press, for lighting that
//! changes while the user steps away.

/// Seconds since the last key press on any keyboard, or `None` where the
/// operating system does not say (everywhere but macOS, for now).
#[must_use]
pub fn seconds_since_key_press() -> Option<f64> {
    imp::seconds_since_key_press()
}

#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "CoreGraphics' idle query is a C function")]
mod imp {
    /// `kCGEventSourceStateCombinedSessionState`: every source in the login
    /// session, so any keyboard counts.
    const COMBINED_SESSION_STATE: i32 = 0;
    /// `kCGEventKeyDown`.
    const KEY_DOWN: u32 = 10;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }

    pub(super) fn seconds_since_key_press() -> Option<f64> {
        // SAFETY: a plain C call with by-value arguments; CoreGraphics allows
        // it from any thread and needs no permission for it.
        let seconds =
            unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, KEY_DOWN) };
        seconds.is_finite().then(|| seconds.max(0.))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn seconds_since_key_press() -> Option<f64> {
        None
    }
}
