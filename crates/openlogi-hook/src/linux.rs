//! Linux `evdev` + `uinput` implementation of the OS-level mouse hook.
//!
//! Each physical mouse — a relative pointer with buttons — found under
//! `/dev/input/` is grabbed exclusively; a paired `uinput` virtual device
//! re-injects events the callback marks
//! [`crate::EventDisposition::PassThrough`]. Events marked
//! [`crate::EventDisposition::Suppress`] are consumed and never reach the desktop.
//!
//! Touch-driven devices (touchpads, touchscreens) and pointing sticks are
//! never grabbed, even though they advertise mouse buttons: the virtual
//! device mirrors only keys and relative axes, so a grab would swallow their
//! multitouch `EV_ABS` stream (and the input properties libinput keys
//! behavior on) with no way to re-inject it — killing the built-in pointer.
//!
//! # Permissions
//!
//! The process needs read access to `/dev/input/eventN` (typically the `input`
//! group) and write access to `/dev/uinput` (the `input` or `uinput` group, or
//! a `udev` rule granting access). Without those, `start()` returns
//! [`crate::HookError::Linux`].
#![expect(
    unsafe_code,
    reason = "the wake pipe and the Wayland toplevel listener call libc directly"
)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsoluteAxisCode, AttributeSetRef, Device, EventSummary, KeyCode, PropType, RelativeAxisCode,
};
use tracing::{debug, error, warn};
use x11rb::connection::Connection as _;
use x11rb::properties::WmClass;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use crate::{
    ButtonId, CursorPosition, EventDisposition, ForegroundApp, HookBackend, HookError, HookEvent,
    LOGITECH_VENDOR_ID, MouseEvent,
};

/// Prefix carried by every uinput device OpenLogi creates — the hook's
/// pass-through mice ([`VIRTUAL_DEVICE_NAME`]) and openlogi-inject's
/// "OpenLogi action injector" (which also advertises mouse buttons).
/// Enumeration refuses anything with this prefix so the hook can never grab
/// one of our own virtual devices.
const OPENLOGI_DEVICE_PREFIX: &str = "OpenLogi ";

/// Name stamped on every uinput pass-through device.
const VIRTUAL_DEVICE_NAME: &str = "OpenLogi virtual mouse";

/// Hi-res scroll resolution: 120 units per standard wheel tick, matching the
/// Linux kernel's `REL_WHEEL_HI_RES` convention and Windows HID semantics.
const HIRES_UNITS_PER_TICK: f32 = 120.0;

pub(crate) struct HookInner {
    stop: Arc<AtomicBool>,
    /// One pipe write-end per device thread; writing wakes the blocking poll.
    stop_pipes: Vec<OwnedFd>,
    threads: Vec<thread::JoinHandle<()>>,
}

/// The Linux backend: one `evdev` reader thread per mouse, re-injecting
/// pass-through events through a `uinput` device.
pub(crate) struct Backend;

impl HookBackend for Backend {
    type Running = HookInner;

    fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<HookInner, HookError> {
        let devices = find_mouse_devices();
        if devices.is_empty() {
            return Err(HookError::NoDeviceFound);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync> = Arc::new(cb);
        let mut threads: Vec<thread::JoinHandle<()>> = Vec::with_capacity(devices.len());
        let mut stop_pipes: Vec<OwnedFd> = Vec::with_capacity(devices.len());

        let result = (|| -> io::Result<()> {
            for (path, device) in devices {
                let virtual_device = build_virtual_device(&device)?;
                let (rx, tx) = create_pipe()?;
                let stop_clone = Arc::clone(&stop);
                let cb_clone = Arc::clone(&cb);
                let handle = thread::Builder::new()
                    .name(format!("openlogi-hook:{}", path.display()))
                    .spawn(move || {
                        device_thread(path, device, virtual_device, cb_clone, stop_clone, rx);
                    })?;
                threads.push(handle);
                stop_pipes.push(tx);
            }
            Ok(())
        })();

        if let Err(e) = result {
            shutdown(&stop, &stop_pipes, threads);
            return Err(HookError::Linux(e));
        }

        Ok(HookInner {
            stop,
            stop_pipes,
            threads,
        })
    }

    fn stop(inner: HookInner) {
        shutdown(&inner.stop, &inner.stop_pipes, inner.threads);
    }

    /// Return the currently frontmost application, or `None` when unavailable.
    /// Dispatches to the backend chosen at startup.
    ///
    /// On an X11 session the identifier is the `WM_CLASS` class component (e.g.
    /// "Firefox"). On a Wayland session the wlr-foreign-toplevel or gnome-shell
    /// backend is used when available; XWayland windows fall back to the X11
    /// backend. No Linux source reports an application name separate from its
    /// identifier, so the two are the same string here.
    fn frontmost_app() -> Option<ForegroundApp> {
        FRONTMOST_SOURCE
            .frontmost_app_id()
            .map(ForegroundApp::unnamed)
    }

    /// Read the global cursor position through X11 when an X server is available.
    /// Native Wayland intentionally has no equivalent global-coordinate API.
    fn cursor_position() -> Option<CursorPosition> {
        let source = X11Source::connect()?;
        let reply = source.conn.query_pointer(source.root).ok()?.reply().ok()?;
        Some(CursorPosition {
            x: f64::from(reply.root_x),
            y: f64::from(reply.root_y),
        })
    }
}

fn shutdown(stop: &AtomicBool, pipes: &[OwnedFd], threads: Vec<thread::JoinHandle<()>>) {
    stop.store(true, Ordering::Relaxed);
    for fd in pipes {
        signal_pipe(fd);
    }
    for handle in threads {
        if let Err(e) = handle.join() {
            error!("hook thread panicked on shutdown: {e:?}");
        }
    }
}

/// Write one wake-up byte to a pipe, retrying on EINTR.
fn signal_pipe(fd: &OwnedFd) {
    loop {
        // SAFETY: fd is a valid open pipe write end; writing one byte is safe.
        let ret = unsafe { libc::write(fd.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
        if ret >= 0 {
            return;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        error!("failed to signal hook thread pipe ({err}): hook thread may not wake");
        return;
    }
}

fn create_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: fds is a valid two-element array; pipe2() fills it with two new fds on success.
    // O_CLOEXEC prevents the fds from being inherited by forked children — without it a child
    // holding the write-end would prevent the hook thread's read-end from ever seeing EOF,
    // blocking clean shutdown.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2() succeeded, so both fds are valid open file descriptors we own.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn find_mouse_devices() -> Vec<(std::path::PathBuf, Device)> {
    evdev::enumerate()
        // Only Logitech mice: the hook exists to remap devices OpenLogi
        // manages, and grabbing an unrelated vendor's mouse would route its
        // events through the virtual device and apply foreign remaps to it.
        // `input_id().vendor()` is u16; crate::LOGITECH_VENDOR_ID is u32.
        .filter(|(path, d)| {
            let ours = u32::from(d.input_id().vendor()) == LOGITECH_VENDOR_ID;
            if !ours
                && d.supported_keys()
                    .is_some_and(|keys| keys.contains(KeyCode::BTN_LEFT))
            {
                debug!(
                    "not hooking {} ({}): not a Logitech device",
                    path.display(),
                    d.name().unwrap_or("unnamed"),
                );
            }
            ours
        })
        .filter(|(path, d)| {
            let vendor = u32::from(d.input_id().vendor());
            let hookable = is_hookable_mouse(
                d.name(),
                d.supported_keys(),
                d.supported_relative_axes(),
                d.supported_absolute_axes(),
                d.properties(),
                vendor,
            );
            if !hookable
                && d.supported_keys()
                    .is_some_and(|keys| keys.contains(KeyCode::BTN_LEFT))
            {
                debug!(
                    "not hooking {} ({}): has mouse buttons but is not a managed Logitech relative-pointer mouse",
                    path.display(),
                    d.name().unwrap_or("unnamed"),
                );
            }
            hookable
        })
        .collect()
}

/// Decide whether an input device is a physical mouse the hook may grab.
///
/// Grabbing is only correct for devices whose full event stream the paired
/// virtual device can re-inject, i.e. relative pointers. Touch-driven
/// devices (touchpads, touchscreens) speak multitouch `EV_ABS`, which
/// [`build_virtual_device`] does not mirror — grabbing one swallows its
/// events and kills the pointer. Pointing sticks are relative pointers but
/// are excluded too: libinput derives their on-button scrolling from the
/// `POINTING_STICK` input property, which a re-injected uinput stream loses,
/// and built-in sticks are never OpenLogi's target hardware.
///
/// Only Logitech devices are grabbed: OpenLogi remaps Logitech mice, and an
/// exclusive grab on any other vendor's pointer (or a misclassified built-in
/// trackpad) would leave that device dead for the life of the agent (#484).
fn is_hookable_mouse(
    name: Option<&str>,
    keys: Option<&AttributeSetRef<KeyCode>>,
    rel_axes: Option<&AttributeSetRef<RelativeAxisCode>>,
    abs_axes: Option<&AttributeSetRef<AbsoluteAxisCode>>,
    props: &AttributeSetRef<PropType>,
    vendor_id: u32,
) -> bool {
    // Never hook one of our own uinput devices (an unnamed device is fine —
    // ours are always named).
    if name.is_some_and(|n| n.starts_with(OPENLOGI_DEVICE_PREFIX)) {
        return false;
    }
    // OpenLogi only remaps Logitech hardware — never grab foreign vendors.
    if vendor_id != LOGITECH_VENDOR_ID {
        return false;
    }
    // A mouse clicks and moves relatively; nothing else qualifies. This alone
    // rejects pure-ABS touchpads, keyboards with stray button bits, and
    // wheel-only devices like the action injector.
    let clicks = keys.is_some_and(|k| k.contains(KeyCode::BTN_LEFT));
    let moves = rel_axes.is_some_and(|r| {
        r.contains(RelativeAxisCode::REL_X) && r.contains(RelativeAxisCode::REL_Y)
    });
    if !clicks || !moves {
        return false;
    }
    // Combo devices that qualify as relative pointers but also expose a touch
    // surface must stay un-grabbed: their touch stream cannot be re-injected.
    let touches = keys
        .is_some_and(|k| k.contains(KeyCode::BTN_TOUCH) || k.contains(KeyCode::BTN_TOOL_FINGER))
        || abs_axes.is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_MT_POSITION_X))
        || props.contains(PropType::BUTTONPAD)
        || props.contains(PropType::SEMI_MT)
        || props.contains(PropType::DIRECT);
    !touches && !props.contains(PropType::POINTING_STICK)
}

fn build_virtual_device(device: &Device) -> io::Result<evdev::uinput::VirtualDevice> {
    let builder = VirtualDevice::builder()?.name(VIRTUAL_DEVICE_NAME);

    let builder = if let Some(keys) = device.supported_keys() {
        builder.with_keys(keys)?
    } else {
        builder
    };

    let builder = if let Some(axes) = device.supported_relative_axes() {
        builder.with_relative_axes(axes)?
    } else {
        builder
    };

    builder.build()
}

/// Block until `device_fd` has data or `stop_fd` is readable.
///
/// Returns `true` when the device is ready to read, `false` on stop signal or
/// unrecoverable poll error.
fn wait_readable(device_fd: i32, stop_fd: i32) -> bool {
    const ERR_FLAGS: libc::c_short = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
    let mut fds = [
        libc::pollfd {
            fd: device_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: fds is a valid two-element pollfd array.
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue; // interrupted by signal — retry
            }
            error!("poll() failed: {err}");
            return false;
        }
        // An error/hangup on either fd (e.g. the grabbed device was unplugged →
        // POLLHUP) leaves it permanently "ready", so without this check neither
        // POLLIN branch fires and the loop spins at 100% CPU. Treat it as a stop
        // so the caller exits the thread and releases the grab.
        if fds[0].revents & ERR_FLAGS != 0 {
            warn!("hooked device closed or errored; stopping its thread");
            return false;
        }
        if fds[1].revents & ERR_FLAGS != 0 {
            return false; // stop pipe closed → shut down
        }
        if fds[1].revents & libc::POLLIN != 0 {
            return false; // stop signal
        }
        if fds[0].revents & libc::POLLIN != 0 {
            return true; // device has data
        }
    }
}

fn scroll(delta_x: f64, delta_y: f64) -> MouseEvent {
    // evdev delivers the wheel and the trackpad as distinct devices, so a wheel
    // event is always a mouse wheel — never a trackpad gesture.
    MouseEvent::Scroll {
        delta: crate::ScrollDelta::wheel_ticks(delta_x, delta_y),
        from_trackpad: false,
        device: None,
    }
}

fn translate(event: &evdev::InputEvent, hires_scroll: bool) -> Option<MouseEvent> {
    match event.destructure() {
        EventSummary::Key(_, key, value) => {
            let id = key_to_button(key)?;
            // Device is already Logitech-only (see `is_hookable_mouse`); the
            // agent runtime treats `device: None` as remappable on Linux.
            Some(MouseEvent::Button {
                id,
                pressed: value != 0,
                device: None,
            })
        }
        EventSummary::RelativeAxis(_, axis, value) => match axis {
            // Pointer movement feeds gesture-button swipe detection. Emitted as a
            // `Moved` and always passed through, so the cursor keeps moving while
            // a held gesture button accumulates the swipe (the B2 cursor-drift
            // design).
            RelativeAxisCode::REL_X => Some(MouseEvent::Moved {
                delta_x: value,
                delta_y: 0,
            }),
            RelativeAxisCode::REL_Y => Some(MouseEvent::Moved {
                delta_x: 0,
                delta_y: value,
            }),
            _ => {
                let v = f64::from(value);
                if hires_scroll {
                    match axis {
                        RelativeAxisCode::REL_WHEEL_HI_RES => {
                            Some(scroll(0.0, v / f64::from(HIRES_UNITS_PER_TICK)))
                        }
                        RelativeAxisCode::REL_HWHEEL_HI_RES => {
                            Some(scroll(v / f64::from(HIRES_UNITS_PER_TICK), 0.0))
                        }
                        // Low-res ticks are redundant when hi-res is active.
                        _ => None,
                    }
                } else {
                    match axis {
                        RelativeAxisCode::REL_WHEEL => Some(scroll(0.0, v)),
                        RelativeAxisCode::REL_HWHEEL => Some(scroll(v, 0.0)),
                        _ => None,
                    }
                }
            }
        },
        _ => None,
    }
}

fn key_to_button(key: KeyCode) -> Option<ButtonId> {
    match key {
        KeyCode::BTN_LEFT => Some(ButtonId::LeftClick),
        KeyCode::BTN_RIGHT => Some(ButtonId::RightClick),
        KeyCode::BTN_MIDDLE => Some(ButtonId::MiddleClick),
        // BTN_BACK/BTN_SIDE both appear as the back thumb button across mice.
        KeyCode::BTN_BACK | KeyCode::BTN_SIDE => Some(ButtonId::Back),
        // BTN_FORWARD/BTN_EXTRA both appear as the forward thumb button.
        KeyCode::BTN_FORWARD | KeyCode::BTN_EXTRA => Some(ButtonId::Forward),
        // BTN_TASK is the closest generic match for a mode/DPI toggle button.
        KeyCode::BTN_TASK => Some(ButtonId::DpiToggle),
        _ => None,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "path/cb/stop/stop_rx are moved into the spawned thread and must not be refs"
)]
fn device_thread(
    path: std::path::PathBuf,
    mut device: Device,
    mut virtual_device: VirtualDevice,
    cb: Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync>,
    stop: Arc<AtomicBool>,
    stop_rx: OwnedFd,
) {
    if let Err(e) = device.grab() {
        // Without the exclusive grab the desktop still receives the physical
        // events, so reading and re-injecting them here would duplicate every
        // one. Skip this device instead — it stays usable, just un-hooked.
        warn!(
            "failed to grab {} exclusively: {e} — skipping (left un-hooked)",
            path.display()
        );
        return;
    }

    let hires_scroll = device
        .supported_relative_axes()
        .is_some_and(|axes| axes.contains(RelativeAxisCode::REL_WHEEL_HI_RES));

    let device_fd = device.as_raw_fd();
    let stop_fd = stop_rx.as_raw_fd();
    // Events that will be re-injected at the next SYN_REPORT.
    let mut pending: Vec<evdev::InputEvent> = Vec::new();

    debug!("hook started on {}", path.display());

    'read: loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if !wait_readable(device_fd, stop_fd) {
            break;
        }

        let events = match device.fetch_events() {
            Ok(iter) => iter,
            Err(e) => {
                error!("read error on {}: {e}", path.display());
                break;
            }
        };

        for event in events {
            if let EventSummary::Synchronization(..) = event.destructure() {
                // Flush the report. `emit()` appends its own SYN_REPORT, so the
                // incoming sync event is dropped rather than re-emitted — pushing
                // it would send a redundant second SYN_REPORT.
                if !pending.is_empty() {
                    if let Err(e) = virtual_device.emit(&pending) {
                        // The physical device is grabbed, so these pass-through
                        // events can't reach the desktop any other way. A uinput
                        // emit failure means the virtual device is broken, so
                        // stop here — dropping the grab restores normal input —
                        // rather than silently dropping events on every report.
                        error!(
                            "uinput emit failed on {}: {e} — stopping hook for this device",
                            path.display()
                        );
                        break 'read;
                    }
                    pending.clear();
                }
            } else {
                let disposition = match translate(&event, hires_scroll) {
                    Some(me) => cb(HookEvent::Mouse(me)),
                    // Low-res companions (REL_WHEEL/REL_HWHEEL) must be suppressed when hi-res
                    // is active — passing them through would double the scroll distance.
                    None if hires_scroll
                        && matches!(
                            event.destructure(),
                            EventSummary::RelativeAxis(
                                _,
                                RelativeAxisCode::REL_WHEEL | RelativeAxisCode::REL_HWHEEL,
                                _
                            )
                        ) =>
                    {
                        EventDisposition::Suppress
                    }
                    None => EventDisposition::PassThrough,
                };
                match disposition {
                    // No evdev rewrite path for `InvertScroll` yet; pass it
                    // through rather than silently eating the scroll.
                    EventDisposition::PassThrough | EventDisposition::InvertScroll => {
                        pending.push(event);
                    }
                    EventDisposition::Suppress => {}
                }
            }
        }
    }

    debug!("hook stopped on {}", path.display());
    // Dropping `device` releases the exclusive grab, restoring normal input delivery.
}

// ── frontmost_app_id ─────────────────────────────────────────────────────────

// The frontmost-app reader is backend-driven so that Wayland support can be
// added without touching callers. Exactly one backend is selected at startup
// from the session environment (see `detect_frontmost_source`) and cached in
// `FRONTMOST_SOURCE` for the process lifetime. The X11, wlr-foreign-toplevel,
// and gnome-shell backends are all available; see `wayland_candidates`.

mod gnome_shell;
mod wlr_foreign_toplevel;

/// A backend that reports which application is currently frontmost.
///
/// Implementations are display-server / desktop specific. The string returned
/// by `frontmost_app_id` is compared against per-app profile keys by exact
/// match (`openlogi_core::Config::effective_bindings`), so its exact form
/// matters and is backend-specific. The X11 and gnome-shell backends both
/// return the `WM_CLASS` class component (e.g. "Firefox"); the wlr backend
/// returns the xdg-shell `app_id` (e.g. "org.mozilla.firefox"). These two
/// namespaces do not map onto each other by any simple string rule, so a
/// per-app profile created under wlroots will not match under GNOME/X11 and
/// vice versa. This is a known limitation: reconciling it needs a canonical-id
/// scheme or per-profile aliases rather than naive normalization, and is
/// deliberately out of scope for the backends themselves.
trait FrontmostSource: Send + Sync {
    /// Opaque identifier of the frontmost application, or `None` when there is
    /// no frontmost window or it cannot be read.
    fn frontmost_app_id(&self) -> Option<String>;

    /// Short backend identifier, for diagnostics / logging only.
    fn name(&self) -> &'static str;
}

/// Frontmost backend backed by X11 `_NET_ACTIVE_WINDOW` + `WM_CLASS`.
///
/// Works on an X11 session, and on a Wayland session for XWayland windows;
/// native Wayland windows are invisible through this path and yield `None`.
struct X11Source {
    conn: RustConnection,
    root: Window,
    net_active_window: Atom,
}

impl X11Source {
    /// Connect to the X server and resolve the `_NET_ACTIVE_WINDOW` atom.
    /// Returns `None` when no X display is reachable (a Wayland session without
    /// XWayland, or `$DISPLAY` unset).
    fn connect() -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| debug!("X11 not available, frontmost will return None: {e}"))
            .ok()?;
        let root = conn.setup().roots[screen_num].root;
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            conn,
            root,
            net_active_window,
        })
    }
}

impl FrontmostSource for X11Source {
    fn frontmost_app_id(&self) -> Option<String> {
        // _NET_ACTIVE_WINDOW on the root window holds the focused window's XID.
        let window: Window = self
            .conn
            .get_property(
                false,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()?;
        if window == 0 {
            return None;
        }

        // WM_CLASS is instance_name\0class_name\0; the class component is more
        // stable across window instances and is what profiles should key on
        // (e.g. "Firefox", not "Navigator").
        let wm = WmClass::get(&self.conn, window)
            .ok()?
            .reply_unchecked()
            .ok()??;
        std::str::from_utf8(wm.class())
            .ok()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    fn name(&self) -> &'static str {
        "x11"
    }
}

/// Fallback used when no backend is available (e.g. a pure Wayland session
/// before any Wayland backend lands). Always reports `None`, so per-app
/// profile switching simply no-ops rather than erroring.
struct NullSource;

impl FrontmostSource for NullSource {
    fn frontmost_app_id(&self) -> Option<String> {
        None
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

/// Coarse classification of the graphical session, used to order the frontmost
/// backend candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

/// Classify the session from the environment. `XDG_SESSION_TYPE` is
/// authoritative when set to `x11` or `wayland`; otherwise fall back to the
/// presence of `WAYLAND_DISPLAY` / `DISPLAY`.
fn detect_session_kind() -> SessionKind {
    if let Ok(kind) = std::env::var("XDG_SESSION_TYPE") {
        match kind.as_str() {
            "wayland" => return SessionKind::Wayland,
            "x11" => return SessionKind::X11,
            _ => {}
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        SessionKind::Wayland
    } else if std::env::var_os("DISPLAY").is_some() {
        SessionKind::X11
    } else {
        SessionKind::Unknown
    }
}

/// A backend constructor: returns the backend if it can initialize on this
/// system, or `None` to fall through to the next candidate.
type Candidate = fn() -> Option<Box<dyn FrontmostSource>>;

fn x11_candidate() -> Option<Box<dyn FrontmostSource>> {
    X11Source::connect().map(|s| Box::new(s) as Box<dyn FrontmostSource>)
}

/// Wayland-native frontmost backends, in priority order: the wlroots
/// foreign-toplevel protocol (sway, Hyprland, river, …) and the GNOME Shell
/// D-Bus extension (Mutter). AT-SPI remains a future fallback. Compositors that
/// support none of these fall through to the X11/XWayland path (which resolves
/// XWayland windows, `None` for native Wayland apps).
fn wayland_candidates() -> Vec<Candidate> {
    vec![wlr_foreign_toplevel::candidate, gnome_shell::candidate]
}

/// Pick the frontmost backend for this session, trying each candidate in order
/// and keeping the first that initializes. Called once, lazily, per process.
fn detect_frontmost_source() -> Box<dyn FrontmostSource> {
    let session = detect_session_kind();
    debug!("frontmost: session kind = {session:?}");

    let mut candidates: Vec<Candidate> = match session {
        SessionKind::Wayland => wayland_candidates(),
        SessionKind::X11 | SessionKind::Unknown => Vec::new(),
    };
    // X11 / XWayland: the primary path on an X11 session and the universal
    // fallback everywhere else.
    candidates.push(x11_candidate);

    for candidate in candidates {
        if let Some(source) = candidate() {
            debug!("frontmost: using '{}' backend", source.name());
            // On Wayland, landing on the X11 backend means no native Wayland
            // frontmost source was available, so native Wayland windows will
            // report None (only XWayland windows resolve). Hint at the fix.
            if session == SessionKind::Wayland && source.name() == "x11" {
                debug!(
                    "frontmost: on Wayland but using the X11/XWayland backend; \
                     native Wayland windows will report None. Install the OpenLogi \
                     GNOME Shell extension (GNOME) or use a wlroots compositor."
                );
            }
            return source;
        }
    }

    debug!("frontmost: no usable backend; frontmost_app_id will return None");
    Box::new(NullSource)
}

static FRONTMOST_SOURCE: LazyLock<Box<dyn FrontmostSource>> =
    LazyLock::new(detect_frontmost_source);

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use evdev::{EventType, InputEvent, KeyCode, RelativeAxisCode};

    use super::*;

    // ── key_to_button ────────────────────────────────────────────────────────

    #[test]
    fn key_to_button_maps_standard_mouse_buttons() {
        let cases = [
            (KeyCode::BTN_LEFT, ButtonId::LeftClick),
            (KeyCode::BTN_RIGHT, ButtonId::RightClick),
            (KeyCode::BTN_MIDDLE, ButtonId::MiddleClick),
            (KeyCode::BTN_BACK, ButtonId::Back),
            (KeyCode::BTN_SIDE, ButtonId::Back),
            (KeyCode::BTN_FORWARD, ButtonId::Forward),
            (KeyCode::BTN_EXTRA, ButtonId::Forward),
            (KeyCode::BTN_TASK, ButtonId::DpiToggle),
        ];
        for (key, expected) in cases {
            assert_eq!(
                key_to_button(key),
                Some(expected),
                "key_to_button({key:?}) should be {expected:?}"
            );
        }
    }

    #[test]
    fn key_to_button_returns_none_for_non_mouse_keys() {
        assert_eq!(key_to_button(KeyCode::KEY_A), None);
        assert_eq!(key_to_button(KeyCode::KEY_LEFTSHIFT), None);
    }

    // ── translate ────────────────────────────────────────────────────────────

    #[test]
    fn translate_btn_left_down_returns_button_pressed() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 1);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::LeftClick,
                pressed: true,
                device: None,
            })
        );
    }

    #[test]
    fn translate_btn_left_up_returns_button_released() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 0);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::LeftClick,
                pressed: false,
                device: None,
            })
        );
    }

    #[test]
    fn translate_btn_back_returns_back() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_BACK.0, 1);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Back,
                pressed: true,
                device: None,
            })
        );
    }

    #[test]
    fn translate_btn_side_returns_back() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_SIDE.0, 1);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Back,
                pressed: true,
                device: None,
            })
        );
    }

    #[test]
    fn translate_btn_forward_returns_forward() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_FORWARD.0, 1);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Forward,
                pressed: true,
                device: None,
            })
        );
    }

    // ── movement ─────────────────────────────────────────────────────────────

    #[test]
    fn translate_rel_x_returns_horizontal_move() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, 7);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Moved {
                delta_x: 7,
                delta_y: 0
            })
        );
    }

    #[test]
    fn translate_rel_y_returns_vertical_move() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, -4);
        assert_matches!(
            translate(&event, false),
            Some(MouseEvent::Moved {
                delta_x: 0,
                delta_y: -4
            })
        );
    }

    // ── scroll — standard ────────────────────────────────────────────────────

    #[test]
    fn translate_rel_wheel_returns_scroll_y() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, 3);
        let result = translate(&event, false);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta, .. })
                if delta == crate::ScrollDelta::wheel_ticks(0.0, 3.0)),
            "expected a three-tick vertical scroll, got {result:?}"
        );
    }

    #[test]
    fn translate_rel_hwheel_returns_scroll_x() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL.0, -2);
        let result = translate(&event, false);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta, .. })
                if delta == crate::ScrollDelta::wheel_ticks(-2.0, 0.0)),
            "expected a negative two-tick horizontal scroll, got {result:?}"
        );
    }

    // ── scroll — hi-res ──────────────────────────────────────────────────────

    #[test]
    fn translate_hires_wheel_returns_fractional_scroll_y() {
        // 60 hi-res units = 0.5 standard ticks
        let event = InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_WHEEL_HI_RES.0,
            60,
        );
        let result = translate(&event, true);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta, .. })
                if delta == crate::ScrollDelta::wheel_ticks(0.0, 0.5)),
            "expected a half-tick vertical scroll, got {result:?}"
        );
    }

    #[test]
    fn translate_hires_hwheel_returns_fractional_scroll_x() {
        let event = InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_HWHEEL_HI_RES.0,
            -120,
        );
        let result = translate(&event, true);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta, .. })
                if delta == crate::ScrollDelta::wheel_ticks(-1.0, 0.0)),
            "expected a negative one-tick horizontal scroll, got {result:?}"
        );
    }

    #[test]
    fn translate_low_res_wheel_skipped_when_hires_active() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, 1);
        assert!(translate(&event, true).is_none());
    }

    #[test]
    fn translate_low_res_hwheel_skipped_when_hires_active() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL.0, 1);
        assert!(translate(&event, true).is_none());
    }

    #[test]
    fn translate_non_mouse_key_returns_none() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::KEY_A.0, 1);
        assert!(translate(&event, false).is_none());
    }

    #[test]
    fn translate_sync_event_returns_none() {
        let event = InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        assert!(translate(&event, false).is_none());
    }

    // ── is_hookable_mouse ────────────────────────────────────────────────────

    use evdev::{AbsoluteAxisCode, AttributeSet, PropType};

    /// Capability profile fed to [`is_hookable_mouse`]. [`Caps::mouse`] models a
    /// plain relative mouse; tests mutate it toward the device they model.
    struct Caps {
        name: Option<&'static str>,
        keys: Option<AttributeSet<KeyCode>>,
        rel: Option<AttributeSet<RelativeAxisCode>>,
        abs: Option<AttributeSet<AbsoluteAxisCode>>,
        props: AttributeSet<PropType>,
        vendor_id: u32,
    }

    impl Caps {
        fn mouse() -> Self {
            Self {
                name: Some("Logitech MX Master 3S"),
                keys: Some(
                    [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE]
                        .into_iter()
                        .collect(),
                ),
                rel: Some(
                    [
                        RelativeAxisCode::REL_X,
                        RelativeAxisCode::REL_Y,
                        RelativeAxisCode::REL_WHEEL,
                    ]
                    .into_iter()
                    .collect(),
                ),
                abs: None,
                props: AttributeSet::new(),
                vendor_id: LOGITECH_VENDOR_ID,
            }
        }

        fn is_hookable(&self) -> bool {
            is_hookable_mouse(
                self.name,
                self.keys.as_deref(),
                self.rel.as_deref(),
                self.abs.as_deref(),
                &self.props,
                self.vendor_id,
            )
        }
    }

    #[test]
    fn plain_mouse_is_hookable() {
        assert!(Caps::mouse().is_hookable());
    }

    #[test]
    fn unnamed_mouse_is_hookable() {
        let mut caps = Caps::mouse();
        caps.name = None;
        assert!(
            caps.is_hookable(),
            "a missing name must not exclude a device"
        );
    }

    #[test]
    fn non_logitech_mouse_is_not_hookable() {
        let mut caps = Caps::mouse();
        caps.name = Some("Apple SPI Trackpad");
        caps.vendor_id = 0x05ac;
        assert!(
            !caps.is_hookable(),
            "foreign vendors must never be exclusively grabbed"
        );
        caps.name = Some("Generic USB Mouse");
        caps.vendor_id = 0x1234;
        assert!(!caps.is_hookable());
    }

    #[test]
    fn touchpad_is_not_hookable() {
        // A libinput touchpad: clicks via BTN_LEFT but moves via multitouch
        // EV_ABS, no relative axes — the device class the old BTN_LEFT-only
        // filter wrongly grabbed, killing the built-in touchpad.
        let mut caps = Caps::mouse();
        caps.name = Some("ELAN0670:00 04F3:3150 Touchpad");
        caps.keys = Some(
            [
                KeyCode::BTN_LEFT,
                KeyCode::BTN_TOUCH,
                KeyCode::BTN_TOOL_FINGER,
            ]
            .into_iter()
            .collect(),
        );
        caps.rel = None;
        caps.abs = Some([AbsoluteAxisCode::ABS_MT_POSITION_X].into_iter().collect());
        caps.props = [PropType::POINTER, PropType::BUTTONPAD]
            .into_iter()
            .collect();
        assert!(!caps.is_hookable());
    }

    #[test]
    fn touch_keys_exclude_a_relative_pointer() {
        for touch_key in [KeyCode::BTN_TOUCH, KeyCode::BTN_TOOL_FINGER] {
            let mut caps = Caps::mouse();
            caps.keys = Some(
                [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, touch_key]
                    .into_iter()
                    .collect(),
            );
            assert!(
                !caps.is_hookable(),
                "{touch_key:?} should mark the device as touch-driven"
            );
        }
    }

    #[test]
    fn multitouch_abs_axis_excludes_a_relative_pointer() {
        let mut caps = Caps::mouse();
        caps.abs = Some([AbsoluteAxisCode::ABS_MT_POSITION_X].into_iter().collect());
        assert!(!caps.is_hookable());
    }

    #[test]
    fn touch_props_exclude_a_relative_pointer() {
        for prop in [PropType::BUTTONPAD, PropType::SEMI_MT, PropType::DIRECT] {
            let mut caps = Caps::mouse();
            caps.props = [prop].into_iter().collect();
            assert!(
                !caps.is_hookable(),
                "{prop:?} should mark the device as touch-driven"
            );
        }
    }

    #[test]
    fn pointing_stick_is_not_hookable() {
        let mut caps = Caps::mouse();
        caps.name = Some("TPPS/2 Elan TrackPoint");
        caps.props = [PropType::POINTER, PropType::POINTING_STICK]
            .into_iter()
            .collect();
        assert!(!caps.is_hookable());
    }

    #[test]
    fn device_without_relative_motion_is_not_hookable() {
        // Buttons but no REL_X/REL_Y: keyboards with stray button bits and
        // wheel-only virtual devices (the action injector's shape).
        let mut caps = Caps::mouse();
        caps.rel = None;
        assert!(!caps.is_hookable());

        let mut caps = Caps::mouse();
        caps.rel = Some([RelativeAxisCode::REL_WHEEL].into_iter().collect());
        assert!(!caps.is_hookable());
    }

    #[test]
    fn device_without_buttons_is_not_hookable() {
        let mut caps = Caps::mouse();
        caps.keys = Some(
            [KeyCode::KEY_A, KeyCode::KEY_LEFTSHIFT]
                .into_iter()
                .collect(),
        );
        assert!(!caps.is_hookable());
    }

    #[test]
    fn own_virtual_devices_are_not_hookable() {
        // Both uinput devices OpenLogi creates carry the prefix; even with
        // fully mouse-like capabilities they must never be grabbed.
        for name in ["OpenLogi virtual mouse", "OpenLogi action injector"] {
            let mut caps = Caps::mouse();
            caps.name = Some(name);
            assert!(!caps.is_hookable(), "{name:?} must be excluded by prefix");
        }
    }

    #[test]
    fn virtual_device_name_carries_exclusion_prefix() {
        assert!(
            VIRTUAL_DEVICE_NAME.starts_with(OPENLOGI_DEVICE_PREFIX),
            "renaming the virtual device away from the prefix would let the \
             hook grab its own pass-through mice"
        );
    }

    #[test]
    fn stray_abs_axis_does_not_exclude_a_mouse() {
        // Some mice expose odd ABS codes (e.g. ABS_MISC for tilt); only the
        // multitouch position axis marks a touch surface.
        let mut caps = Caps::mouse();
        caps.abs = Some([AbsoluteAxisCode::ABS_MISC].into_iter().collect());
        assert!(caps.is_hookable());
    }
}
