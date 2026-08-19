//! COM apartment and Media Foundation platform lifetimes, as guards.
//!
//! Both Windows backends have to initialize COM — DirectShow enumeration for
//! controls, Media Foundation for capture — and both used to do it and never
//! tear it down, so every enumeration and every preview session left a count
//! behind on a thread that then exited. Initialization is paired here instead,
//! by types whose drop is the teardown.
//!
//! The two halves count differently, which is why they are two guards:
//!
//! - COM is per **thread**. Every successful `CoInitializeEx` — including one
//!   that returns `S_FALSE` because the thread was already initialized — owes a
//!   `CoUninitialize` on that same thread, and a *failed* one owes nothing.
//! - Media Foundation is not documented as per-thread at all: `MFShutdown` is
//!   specified only as "call this function once for every call to `MFStartup`",
//!   with no statement that one thread's shutdown leaves another thread's
//!   platform running. So the platform is refcounted *here* — started for the
//!   first live guard, shut down when the last one goes — and no capture
//!   session can pull Media Foundation out from under another.

#![expect(
    unsafe_code,
    reason = "COM apartment and Media Foundation platform initialization"
)]

use std::marker::PhantomData;
use std::sync::{Mutex, PoisonError};

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_LITE, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// A COM apartment entered on the current thread, left on drop.
///
/// Every COM interface pointer obtained under a guard must be released **before**
/// the guard drops. Rust drops locals in reverse declaration order, so the guard
/// is declared first in its scope; anything that outlives it borrows it, which
/// turns that ordering into a compile error rather than a convention.
pub(crate) struct ComApartment {
    /// Whether *this* guard's call incremented the thread's initialization
    /// count. `RPC_E_CHANGED_MODE` means the thread already sits in an
    /// incompatible apartment: both backends work under either model, so the
    /// existing one is borrowed — but never torn down, since nothing was added
    /// to its count.
    owned: bool,
    /// `CoUninitialize` has to run on the thread that initialized, so the guard
    /// cannot travel to another one.
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    /// Enter the multithreaded apartment on this thread.
    pub(crate) fn enter() -> Self {
        // SAFETY: no reserved parameter, and a single concurrency flag. The
        // returned HRESULT is a success code for both `S_OK` and `S_FALSE`,
        // which are exactly the two outcomes that owe a `CoUninitialize`.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            owned: hr.is_ok(),
            _not_send: PhantomData,
        }
    }

    /// Start the Media Foundation platform for as long as the returned guard
    /// lives — and no longer than this apartment, which the borrow enforces:
    /// Media Foundation's objects are COM, so the apartment has to outlive the
    /// platform, and dropping them the other way round will not compile.
    ///
    /// # Errors
    /// Whatever `MFStartup` reports, when this is the guard that starts the
    /// platform.
    pub(crate) fn start_media_foundation(&self) -> windows::core::Result<MediaFoundation<'_>> {
        let mut live = live_guards();
        if *live == 0 {
            // SAFETY: called on an application thread that has just entered an
            // apartment — never from a Media Foundation work queue, the one
            // thread kind the platform calls forbid. `MFSTARTUP_LITE` skips the
            // sockets library, which no local capture path needs.
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }?;
        }
        *live += 1;
        Ok(MediaFoundation { _com: self })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        // SAFETY: balances, exactly once and on the initializing thread, the
        // successful `CoInitializeEx` in `enter`. Every interface created under
        // this guard has already been released — callers hold it in the
        // outermost binding of their scope, and anything that outlives it
        // borrows it.
        unsafe { CoUninitialize() };
    }
}

/// The Media Foundation platform, started for as long as this guard lives.
///
/// `!Send` follows from the borrowed [`ComApartment`], which is itself pinned to
/// its thread.
pub(crate) struct MediaFoundation<'a> {
    _com: &'a ComApartment,
}

impl Drop for MediaFoundation<'_> {
    fn drop(&mut self) {
        let mut live = live_guards();
        *live -= 1;
        if *live != 0 {
            return;
        }
        // SAFETY: this is the last live guard, so the shutdown balances the one
        // `MFStartup` above, and every Media Foundation interface any of them
        // created has been released first — each capture session owns its whole
        // object graph in a scope that closes before its guard drops.
        let _ = unsafe { MFShutdown() };
    }
}

/// How many [`MediaFoundation`] guards are alive process-wide.
fn live_guards() -> std::sync::MutexGuard<'static, usize> {
    static LIVE: Mutex<usize> = Mutex::new(0);
    // Poisoning is impossible — no holder panics — but recover rather than
    // unwrap, so a panic elsewhere could not strand the platform.
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}
