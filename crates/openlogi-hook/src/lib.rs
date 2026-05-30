//! OS-level mouse-event hook for OpenLogi.
//!
//! On macOS the hook is implemented with `CGEventTap` (the same primitive used
//! by Logi Options+ and external-reference). Linux and Windows return
//! [`HookError::Unsupported`] from [`Hook::start`] — stubs that let the
//! workspace compile on all platforms without feature-gating callers.
//!
//! # Usage
//!
//! ```no_run
//! use openlogi_hook::{Hook, MouseEvent, EventDisposition};
//!
//! if !Hook::has_accessibility() {
//!     eprintln!("grant Accessibility access first");
//!     return;
//! }
//!
//! let hook = Hook::start(|event| {
//!     println!("{event:?}");
//!     EventDisposition::PassThrough
//! }).unwrap();
//!
//! // … later, on shutdown:
//! hook.stop();
//! ```

pub use openlogi_core::binding::ButtonId;

/// An event captured at the OS layer.
#[derive(Clone, Debug)]
pub enum MouseEvent {
    /// A mouse button was pressed or released.
    Button {
        /// Which button.
        id: ButtonId,
        /// `true` = button down; `false` = button up.
        pressed: bool,
    },
    /// A scroll-wheel tick (or continuous momentum scroll).
    Scroll {
        /// Positive = right, negative = left.
        delta_x: f32,
        /// Positive = down, negative = up.
        delta_y: f32,
    },
}

/// What the hook callback wants the OS to do with the captured event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventDisposition {
    /// Let the event reach its original target unchanged.
    PassThrough,
    /// Drop the event; the target application never sees it.
    Suppress,
}

/// Errors that [`Hook::start`] and related functions can produce.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// This platform has no hook implementation yet (Linux, Windows).
    #[error("mouse event hook is not supported on this platform")]
    Unsupported,
    /// macOS Accessibility permission has not been granted to this process.
    #[error(
        "macOS Accessibility permission is required to capture mouse events; \
         grant it in System Settings → Privacy & Security → Accessibility"
    )]
    AccessibilityDenied,
    /// `CGEventTapCreate` returned null, or the run loop source could not be
    /// created. The inner string carries the context.
    #[error("CGEventTap setup failed: {0}")]
    MacOsTap(String),
}

/// A running OS-level mouse hook. Call [`Hook::stop`] to tear down.
///
/// Internally on macOS, a dedicated `std::thread` runs a `CFRunLoop` that
/// drains the `CGEventTap` queue. `stop` signals that run loop and joins the
/// thread so the process exits cleanly. Dropping a `Hook` without calling
/// `stop` has the same effect via `Drop`.
pub struct Hook {
    #[cfg(target_os = "macos")]
    inner: Option<macos::HookInner>,
    /// Makes `Hook` uninhabited on non-macOS targets, so [`Hook::start`] can
    /// only ever return `Err` there and the type can never be constructed.
    #[cfg(not(target_os = "macos"))]
    never: std::convert::Infallible,
}

impl Drop for Hook {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.take() {
            macos::stop(inner);
        }
        #[cfg(not(target_os = "macos"))]
        // Unreachable: `never: Infallible` makes `Hook` uninhabited here.
        {}
    }
}

impl Hook {
    /// Install the mouse hook and start delivering events to `cb`.
    ///
    /// The callback runs on a private background thread (not the GPUI thread)
    /// for every mouse button or scroll event at the OS HID tap. It must
    /// return [`EventDisposition`] quickly — blocking it stalls input delivery
    /// system-wide.
    ///
    /// On macOS, returns [`HookError::AccessibilityDenied`] when the process
    /// has not been granted Accessibility permission. On Linux and Windows,
    /// returns [`HookError::Unsupported`].
    pub fn start(
        cb: impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        #[cfg(target_os = "macos")]
        {
            macos::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cb;
            Err(HookError::Unsupported)
        }
    }

    /// Stop the hook and release OS resources.
    ///
    /// Signals the background run loop to exit and blocks until the thread
    /// joins. Calling this explicitly is preferred over relying on `Drop` when
    /// errors in cleanup should be visible. `Drop` calls this automatically.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            unused_mut,
            reason = "`mut self` is only consumed by the macOS teardown path"
        )
    )]
    pub fn stop(mut self) {
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.take() {
            macos::stop(inner);
        }
        #[cfg(not(target_os = "macos"))]
        match self.never {}
    }

    /// Returns `true` when the process has the macOS Accessibility entitlement
    /// required to install an active `CGEventTap`.
    ///
    /// On Linux and Windows this always returns `true`; those platforms handle
    /// permissions at a higher layer.
    #[must_use]
    pub fn has_accessibility() -> bool {
        #[cfg(target_os = "macos")]
        {
            macos::has_accessibility()
        }
        #[cfg(not(target_os = "macos"))]
        {
            true
        }
    }

    /// Show the macOS Accessibility permission dialog and register this
    /// process in System Settings → Privacy & Security → Accessibility.
    ///
    /// Unlike [`Self::has_accessibility`], this passes the
    /// `kAXTrustedCheckOptionPrompt` option, so macOS surfaces the native
    /// "open System Settings" dialog the first time and lists the app there
    /// (otherwise the user would have to add the binary by hand). Called for
    /// its side effect; the resulting trust state is observed separately via
    /// [`Self::has_accessibility`]. No-op on non-macOS.
    pub fn prompt_accessibility() {
        #[cfg(target_os = "macos")]
        {
            macos::prompt_accessibility();
        }
    }
}

/// Return the macOS bundle identifier of the currently frontmost application,
/// e.g. `"com.microsoft.VSCode"`. `None` when no app is frontmost, when
/// reading the value fails, or on any non-macOS platform (P1.4).
///
/// Costs four `objc_msgSend`s plus a UTF-8 copy — well under a millisecond
/// at the 1 Hz polling cadence in `openlogi-gui::app_watcher`.
#[must_use]
pub fn frontmost_bundle_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_bundle_id()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(test)]
mod tests;
