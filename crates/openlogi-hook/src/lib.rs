//! OS-level mouse-event hook for OpenLogi.
//!
//! | Platform | Implementation |
//! |----------|---------------|
//! | macOS    | `CGEventTap` (same primitive used by Logi Options+) |
//! | Linux    | `evdev` grab + `uinput` re-injection |
//! | Windows  | stub — returns [`HookError::Unsupported`] |
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
    /// Pointer movement, in device units. Emitted so a held gesture button can
    /// accumulate a swipe; the callback passes these through (the cursor keeps
    /// moving) and only reads them while a gesture button is down.
    Moved {
        /// Positive = right, negative = left.
        delta_x: i32,
        /// Positive = down, negative = up.
        delta_y: i32,
    },
    /// The OS interrupted event capture (on macOS, the tap was disabled by a
    /// timeout or by competing user input). Any in-progress gesture hold must be
    /// cancelled: a button-up dropped during the gap would otherwise leave a
    /// stale hold that the next stray pointer move turns into a phantom swipe.
    /// Carries no data and is always passed through.
    CaptureInterrupted,
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
    /// This platform has no hook implementation yet (Windows).
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
    /// No mouse device was found under `/dev/input`. Either no pointing device
    /// is connected, or the process lacks read permission on the device nodes
    /// (add the user to the `input` group, or add a `udev` rule).
    #[cfg(target_os = "linux")]
    #[error(
        "no mouse device found under /dev/input; \
         ensure a pointing device is connected and the process has read permission \
         (add user to the `input` group or add a udev rule)"
    )]
    NoDeviceFound,
    /// A Linux-specific I/O error occurred while setting up or running the hook.
    #[cfg(target_os = "linux")]
    #[error("Linux input error: {0}")]
    Linux(#[source] std::io::Error),
}

/// A running OS-level mouse hook. Call [`Hook::stop`] to tear down.
///
/// On macOS a dedicated thread runs a `CFRunLoop` draining a `CGEventTap`.
/// On Linux one thread per physical mouse device reads `evdev` events and
/// re-injects pass-through events via a `uinput` virtual device.
/// Call `stop` (or let the value drop) to shut down all threads and release
/// grabbed devices.
pub struct Hook {
    #[cfg(target_os = "macos")]
    inner: Option<macos::HookInner>,
    #[cfg(target_os = "linux")]
    inner: Option<linux::HookInner>,
    /// Makes `Hook` uninhabited on unsupported targets so [`Hook::start`] can
    /// only ever return `Err` there and the type can never be constructed.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    never: std::convert::Infallible,
}

impl Drop for Hook {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.take() {
            macos::stop(inner);
        }
        #[cfg(target_os = "linux")]
        if let Some(inner) = self.inner.take() {
            linux::stop(inner);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        // Unreachable: `never: Infallible` makes `Hook` uninhabited here.
        {}
    }
}

impl Hook {
    /// Install the mouse hook and start delivering events to `cb`.
    ///
    /// The callback runs on a private background thread for every mouse button
    /// or scroll event. It must return [`EventDisposition`] quickly — blocking
    /// it stalls input delivery system-wide.
    ///
    /// On macOS, returns [`HookError::AccessibilityDenied`] when Accessibility
    /// permission has not been granted. On Linux, returns
    /// [`HookError::NoDeviceFound`] when no mouse device is accessible.
    /// On Windows, always returns [`HookError::Unsupported`].
    pub fn start(
        cb: impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        #[cfg(target_os = "macos")]
        {
            macos::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(target_os = "linux")]
        {
            linux::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = cb;
            Err(HookError::Unsupported)
        }
    }

    /// Stop the hook and release OS resources.
    ///
    /// Signals background threads to exit and blocks until they join. Calling
    /// this explicitly is preferred over relying on `Drop` when errors in
    /// cleanup should be visible. `Drop` calls this automatically.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
        allow(
            unused_mut,
            reason = "`mut self` is only consumed by macOS and Linux teardown paths"
        )
    )]
    pub fn stop(mut self) {
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.take() {
            macos::stop(inner);
        }
        #[cfg(target_os = "linux")]
        if let Some(inner) = self.inner.take() {
            linux::stop(inner);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        match self.never {}
    }

    /// Returns `true` when the process has the permissions required to install
    /// the hook.
    ///
    /// On macOS, checks the Accessibility entitlement. On Linux and Windows
    /// this always returns `true`; those platforms enforce permissions at a
    /// lower layer (device node ownership / group membership).
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

/// Return an opaque string identifying the currently frontmost application.
///
/// On macOS this is the bundle identifier, e.g. `"com.microsoft.VSCode"`.
/// On Linux (X11 / XWayland) this is the `WM_CLASS` class component,
/// e.g. `"Code"` or `"Firefox"`. Pure Wayland windows (not running under
/// XWayland) are not visible through this path and return `None`.
///
/// `None` when no app is frontmost, when reading fails, or on unsupported
/// platforms. Costs one X11 round-trip on Linux, four `objc_msgSend`s on
/// macOS — well under a millisecond at the 1 Hz polling cadence in
/// `openlogi-gui::app_watcher`.
#[must_use]
pub fn frontmost_bundle_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_bundle_id()
    }
    #[cfg(target_os = "linux")]
    {
        linux::frontmost_bundle_id()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(test)]
mod tests;
