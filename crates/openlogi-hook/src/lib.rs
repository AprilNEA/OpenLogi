//! OS-level mouse-event hook for OpenLogi.
//!
//! | Platform | Implementation |
//! |----------|---------------|
//! | macOS    | `CGEventTap` (same primitive used by Logi Options+) |
//! | Linux    | `evdev` grab + `uinput` re-injection |
//! | Windows  | `WH_MOUSE_LL` low-level mouse hook (motion is edge-clamped) |
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

use std::cfg_select;

pub use openlogi_core::binding::ButtonId;

/// Logitech's USB/Bluetooth vendor id (`0x046D`), widened from
/// [`openlogi_core::hid::LOGITECH_VENDOR_ID`] because the hook's identity
/// sources (IOKit, evdev) hand it back as a `u32`.
pub const LOGITECH_VENDOR_ID: u32 = openlogi_core::hid::LOGITECH_VENDOR_ID as u32;

/// Cursor position in the operating system's global screen coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorPosition {
    /// Horizontal screen coordinate.
    pub x: f64,
    /// Vertical screen coordinate.
    pub y: f64,
}

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

impl EventDevice {
    /// Whether this looks like a trackpad/touchpad (must never be remapped).
    #[must_use]
    pub fn is_trackpad_like(&self) -> bool {
        self.product_name.as_deref().is_some_and(|n| {
            let n = n.to_ascii_lowercase();
            n.contains("trackpad") || n.contains("touchpad") || n.contains("touch pad")
        })
    }

    /// Whether this is a Logitech product OpenLogi may remap buttons for.
    #[must_use]
    pub fn is_logitech(&self) -> bool {
        if self.vendor_id == Some(LOGITECH_VENDOR_ID) {
            return true;
        }
        self.product_name.as_deref().is_some_and(|n| {
            let n = n.to_ascii_lowercase();
            n.contains("logitech") || n.starts_with("logi ")
        })
    }
}

/// Whether the OS hook may suppress/remap a button event from this source.
///
/// Fail-closed on macOS-style attribution: only a known Logitech non-trackpad
/// source is remappable. Unknown / non-Logitech / trackpad sources always pass
/// through so a wedged remap policy can never brick the system pointer.
#[must_use]
pub fn source_is_remappable(device: Option<&EventDevice>) -> bool {
    match device {
        Some(d) if d.is_trackpad_like() => false,
        Some(d) => d.is_logitech(),
        None => false,
    }
}

/// Which modifier keys were held when a key event fired. Mirrors the
/// detectable macOS modifier flags. Note `Fn` is deliberately absent — it is
/// firmware-internal and never reported on non-function-row keys (see the
/// function-key-remapper spec, Appendix A).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent modifier flags from OS event bits"
)]
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
    /// Mouse button / scroll / move event.
    Mouse(MouseEvent),
    /// Keyboard event (function-key remapper path).
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
        /// Best-effort physical source. `None` when the platform cannot
        /// attribute the event (Windows today) or it was synthetic.
        device: Option<EventDevice>,
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

/// Where in the event stream a tap is inserted (macOS `CGEventTapLocation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapLocation {
    /// `kCGHIDEventTap` — the lowest level, ahead of the window server. An
    /// *active* tap here gates raw device input for the whole system, so a slow
    /// or wedged owner adds latency to every event. This is where OpenLogi (and
    /// Logi Options+) install.
    Hid,
    /// `kCGSessionEventTap` — scoped to the current login session.
    Session,
    /// `kCGAnnotatedSessionEventTap` — session tap that also sees annotations.
    AnnotatedSession,
    /// A location value newer than this enum knows about.
    Other(u32),
}

/// A live event tap installed somewhere in the system, as reported by
/// [`Hook::list_event_taps`]. Read-only diagnostic snapshot — enumerating taps
/// needs no Accessibility grant and any process in the session sees them all.
///
/// The per-tap latency figures `CGEventTapInformation` carries are deliberately
/// omitted: empirically they hold uninitialised sentinel values that change
/// between samples, so they are not a trustworthy lag signal.
#[derive(Clone, Debug)]
pub struct EventTapInfo {
    /// The system-assigned tap identifier.
    pub tap_id: u32,
    /// Where the tap sits in the event stream.
    pub location: TapLocation,
    /// `true` for an *active* tap (`kCGEventTapOptionDefault`) that can modify
    /// or suppress events; `false` for a passive *listen-only* tap, which
    /// physically cannot stall input.
    pub active: bool,
    /// Whether the tap is currently enabled (servicing events).
    pub enabled: bool,
    /// PID of the process that installed the tap.
    pub owner_pid: i32,
    /// Best-effort executable file name of the owner, or `None` if the process
    /// has exited or its path is unreadable.
    pub owner_name: Option<String>,
    /// PID of the single process whose events this tap intercepts, or `None`
    /// for a global tap (one that sees every process's events).
    pub target_pid: Option<i32>,
}

impl EventTapInfo {
    /// `true` when this tap sits *active* at the [`TapLocation::Hid`] level and
    /// is enabled — the one configuration that inserts the owner into the path
    /// of every event and can therefore add latency system-wide. Listen-only,
    /// disabled, or session-level taps cannot stall input this way.
    #[must_use]
    pub fn gates_input(&self) -> bool {
        self.active && self.enabled && self.location == TapLocation::Hid
    }

    /// If this tap's owner is a known third-party input driver that competes
    /// with OpenLogi for the mouse stream, return its product name — used to
    /// warn the user about a likely pointer-lag cause.
    ///
    /// Matches on the owner executable name only; callers should combine it with
    /// [`Self::gates_input`] so a competitor's *inactive* helper isn't flagged.
    #[must_use]
    pub fn known_input_conflict(&self) -> Option<&'static str> {
        // (lower-cased executable-name substring, product display name). Brand
        // names are not localised; only the surrounding warning copy is.
        const KNOWN: &[(&str, &str)] = &[
            ("logioptionsplus", "Logi Options+"),
            ("logioptions", "Logitech Options"),
            ("logimgr", "Logitech Options"),
            ("lccdaemon", "Logitech Control Center"),
            ("steermouse", "SteerMouse"),
            ("bettermouse", "BetterMouse"),
            ("usboverdrive", "USB Overdrive"),
            ("mac mouse fix", "Mac Mouse Fix"),
            ("linearmouse", "LinearMouse"),
            ("smoothscroll", "SmoothScroll"),
        ];
        let name = self.owner_name.as_deref()?.to_ascii_lowercase();
        KNOWN
            .iter()
            .find(|(needle, _)| name.contains(needle))
            .map(|&(_, label)| label)
    }
}

/// Errors that [`Hook::start`] and related functions can produce.
///
/// The same shape on every target: a platform-conditional enum would compile on
/// the maintainer's macOS and break an exhaustive `match` on Linux, and one
/// unreachable variant costs nothing.
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
    #[error(
        "no mouse device found under /dev/input; \
         ensure a pointing device is connected and the process has read permission \
         (add user to the `input` group or add a udev rule)"
    )]
    NoDeviceFound,
    /// A Linux-specific I/O error occurred while setting up or running the hook.
    #[error("Linux input error: {0}")]
    Linux(#[source] std::io::Error),
    /// `SetWindowsHookExW` failed, or the hook thread could not be started.
    #[error("Windows mouse hook setup failed: {0}")]
    WindowsHook(String),
}

/// Everything one operating system has to provide for [`Hook`] to work.
///
/// Exactly one backend is compiled in — see the `Backend` alias below — so this
/// is a compile-time contract, not runtime polymorphism. It earns its place by
/// keeping the crate's per-OS `cfg` down to that single site, and by making the
/// platform surface a list the compiler checks instead of a naming convention.
/// Everything only some platforms can answer carries its do-nothing default
/// here, so a backend implements exactly what it has.
trait HookBackend {
    /// Whatever the platform holds on to while the hook runs; handed back to
    /// [`Self::stop`] to tear it down.
    type Running;

    /// Install the hook. [`Hook::start`] documents the contract owed to callers.
    fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self::Running, HookError>;

    /// Stop the hook and join its threads.
    fn stop(running: Self::Running);

    /// See [`Hook::has_accessibility`]. Platforms that gate the hook below the
    /// privacy layer answer `true`.
    fn has_accessibility() -> bool {
        true
    }

    /// See [`Hook::prompt_accessibility`]. Nothing to prompt for by default.
    fn prompt_accessibility() {}

    /// See [`Hook::list_event_taps`]. Empty where the OS keeps no tap registry.
    fn list_event_taps() -> Vec<EventTapInfo> {
        Vec::new()
    }

    /// See [`crate::frontmost_bundle_id`].
    fn frontmost_app() -> Option<String> {
        None
    }

    /// See [`crate::cursor_position`].
    fn cursor_position() -> Option<CursorPosition> {
        None
    }
}

/// The backend for a platform with no hook: every default, and a
/// [`HookBackend::start`] that can only fail. Compiled only where it is the
/// one selected below, so it never sits unused in a real build.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
struct Unsupported;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
impl HookBackend for Unsupported {
    /// Uninhabited, so [`Hook`] can never hold a running hook here.
    type Running = std::convert::Infallible;

    fn start(
        _cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self::Running, HookError> {
        Err(HookError::Unsupported)
    }

    fn stop(running: Self::Running) {
        match running {}
    }
}

// The backend this build talks to — the crate's one platform switch.
cfg_select! {
    target_os = "macos" => { type Backend = macos::Backend; }
    target_os = "linux" => { type Backend = linux::Backend; }
    target_os = "windows" => { type Backend = windows::Backend; }
    _ => { type Backend = Unsupported; }
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
    inner: Option<<Backend as HookBackend>::Running>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Hook {
    /// Install the input hook and start delivering events to `cb`.
    ///
    /// The callback runs on a private background thread for every mouse
    /// button, scroll, or (macOS / Windows) keyboard event. It must return
    /// [`EventDisposition`] quickly — blocking it stalls input delivery
    /// system-wide.
    ///
    /// On macOS, returns [`HookError::AccessibilityDenied`] when Accessibility
    /// permission has not been granted. On Linux, returns
    /// `HookError::NoDeviceFound` when no mouse device is accessible (key
    /// events are not yet captured there). On Windows, installs `WH_MOUSE_LL`
    /// and `WH_KEYBOARD_LL` low-level hooks.
    pub fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<Self, HookError> {
        Backend::start(cb).map(|inner| Self { inner: Some(inner) })
    }

    /// Stop the hook and release OS resources.
    ///
    /// Signals background threads to exit and blocks until they join. Calling
    /// this explicitly is preferred over relying on `Drop` when errors in
    /// cleanup should be visible. `Drop` calls this automatically.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Tear down the platform hook if it is still running. Idempotent: the
    /// first call takes `inner`, so the `Drop` after an explicit [`Self::stop`]
    /// is a no-op.
    fn shutdown(&mut self) {
        if let Some(inner) = self.inner.take() {
            Backend::stop(inner);
        }
    }

    /// Returns `true` when the process has the permissions required to install
    /// the hook.
    ///
    /// On macOS this is a live capability check, not just a read of the
    /// Accessibility trust flag: that flag keeps reporting `true` after the
    /// user deletes the app's row from System Settings, so it is paired with a
    /// throwaway event tap that only succeeds while the grant really stands.
    /// Poll it for as long as a hook is installed — an active tap that outlives
    /// its permission wedges system input until reboot. On Linux and Windows
    /// this always returns `true`; those platforms enforce permissions at a
    /// lower layer (device-node ownership / group membership on Linux; the
    /// Windows low-level hook needs no separate privacy grant).
    #[must_use]
    pub fn has_accessibility() -> bool {
        Backend::has_accessibility()
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
        Backend::prompt_accessibility();
    }

    /// Enumerate every event tap currently installed in this login session.
    ///
    /// A read-only diagnostic snapshot for spotting input contention — e.g. a
    /// competing app holding an *active* [`TapLocation::Hid`] tap (the classic
    /// "another driver is also intercepting the mouse" cause of pointer lag),
    /// or OpenLogi's own tap being unexpectedly disabled. Needs no Accessibility
    /// grant; the call sees every process's taps regardless of who asks.
    ///
    /// Returns an empty vector on non-macOS targets, which have no equivalent
    /// global tap registry.
    #[must_use]
    pub fn list_event_taps() -> Vec<EventTapInfo> {
        Backend::list_event_taps()
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
/// `openlogi-desktop::app_watcher`.
#[must_use]
pub fn frontmost_bundle_id() -> Option<String> {
    Backend::frontmost_app()
}

/// Return the current global cursor position without installing an input hook.
///
/// Returns `None` on unsupported platforms and on native Wayland, where the
/// compositor deliberately does not expose global pointer coordinates.
#[must_use]
pub fn cursor_position() -> Option<CursorPosition> {
    Backend::cursor_position()
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(test)]
mod tests;
