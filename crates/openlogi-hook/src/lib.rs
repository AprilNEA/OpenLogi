//! OS-level mouse-event hook for OpenLogi.
//!
//! | Platform | Implementation |
//! |----------|---------------|
//! | macOS    | `CGEventTap` (same primitive used by Logi Options+) |
//! | Linux    | `evdev` grab + `uinput` re-injection |
//! | Windows  | `WH_MOUSE_LL` low-level mouse hook |
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

/// Best-effort identity for the physical device that produced an OS event.
///
/// Platform hooks fill the stable fields they can read cheaply from the native
/// event. Consumers use this to apply host-side settings per device rather than
/// through the currently selected UI device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventDevice {
    /// USB/Bluetooth vendor id when the platform exposes it.
    pub vendor_id: Option<u32>,
    /// USB/Bluetooth/HID product id when the platform exposes it.
    pub product_id: Option<u32>,
    /// Human-readable product name, normalized by consumers before matching.
    pub product_name: Option<String>,
}

/// Which modifier keys were held when a key event fired. Mirrors the
/// detectable macOS modifier flags. Note `Fn` is deliberately absent — it is
/// firmware-internal and never reported on non-function-row keys (see the
/// function-key-remapper spec, Appendix A).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

/// A keyboard event observed by the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Platform virtual keycode (macOS: `kVK_*`, e.g. 122 = F1, 53 = Escape).
    pub keycode: u16,
    /// `true` = key down; `false` = key up.
    pub pressed: bool,
    /// Which modifiers were held.
    pub modifiers: KeyModifiers,
}

/// Anything the OS hook can observe. `Mouse` preserves the existing callback
/// payload; `Key` is the keyboard path added by the function-key remapper.
/// Wrapping both in a union means `Hook::start`'s callback widens once and
/// stays stable as further event classes arrive.
#[derive(Clone, Debug)]
pub enum HookEvent {
    Mouse(MouseEvent),
    Key(KeyEvent),
}

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
        /// `true` when the OS attributes this scroll to a trackpad / Magic Mouse
        /// gesture rather than a mouse wheel, so a consumer can transform the
        /// wheel while leaving native trackpad scrolling alone (issue #126).
        ///
        /// On macOS this is resolved from the `IOHIDEvent` sender's IOKit device
        /// identity, because Logitech free-spin wheels can carry the same phase
        /// flags as a trackpad. Sender-less events fall back to the phase fields.
        /// Always `false` on Linux/Windows, where the wheel and trackpad arrive
        /// as distinct event types rather than one flagged stream.
        from_trackpad: bool,
        /// Best-effort physical source of the scroll event. `None` means the
        /// platform could not attribute the event to a device, or the event was
        /// synthetic.
        device: Option<EventDevice>,
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
    /// This platform has no hook implementation (neither macOS, Linux, nor
    /// Windows).
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
    /// `SetWindowsHookExW` failed, or the hook thread could not be started.
    #[error("Windows mouse hook setup failed: {0}")]
    WindowsHook(String),
}

/// A running OS-level mouse hook. Call [`Hook::stop`] to tear down.
///
/// On macOS a dedicated thread runs a `CFRunLoop` draining a `CGEventTap`.
/// On Linux one thread per physical mouse device reads `evdev` events and
/// re-injects pass-through events via a `uinput` virtual device. On Windows a
/// dedicated thread owns a `WH_MOUSE_LL` hook and pumps its message loop.
/// Call `stop` (or let the value drop) to shut down all threads and release
/// grabbed devices.
pub struct Hook {
    #[cfg(target_os = "macos")]
    inner: Option<macos::HookInner>,
    #[cfg(target_os = "linux")]
    inner: Option<linux::HookInner>,
    #[cfg(target_os = "windows")]
    inner: Option<windows::HookInner>,
    /// Makes `Hook` uninhabited on unsupported targets so [`Hook::start`] can
    /// only ever return `Err` there and the type can never be constructed.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
        #[cfg(target_os = "windows")]
        if let Some(inner) = self.inner.take() {
            windows::stop(inner);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
    /// [`HookError::NoDeviceFound`] when no mouse device is accessible. On
    /// Windows, installs a `WH_MOUSE_LL` low-level mouse hook.
    pub fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        #[cfg(target_os = "macos")]
        {
            macos::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(target_os = "linux")]
        {
            linux::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(target_os = "windows")]
        {
            windows::start(cb).map(|inner| Self { inner: Some(inner) })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(
            unused_mut,
            reason = "`mut self` is only consumed by platform teardown paths"
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
        #[cfg(target_os = "windows")]
        if let Some(inner) = self.inner.take() {
            windows::stop(inner);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        match self.never {}
    }

    /// Returns `true` when the process has the permissions required to install
    /// the hook.
    ///
    /// On macOS, checks the Accessibility entitlement. On Linux and Windows
    /// this always returns `true`; those platforms enforce permissions at a
    /// lower layer (device-node ownership / group membership on Linux; the
    /// Windows low-level hook needs no separate privacy grant).
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
/// XWayland) are not visible through this path and return `None`. On Windows
/// this is the lower-cased executable path of the foreground process.
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
    #[cfg(target_os = "windows")]
    {
        windows::frontmost_process_path()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(test)]
mod tests;
