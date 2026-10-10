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

mod foreground;
pub(crate) mod pointer;

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsoluteAxisCode, AttributeSetRef, Device, EventSummary, EventType, InputEvent, KeyCode,
    PropType, RelativeAxisCode,
};
use tracing::{debug, error, warn};
use x11rb::protocol::xproto::ConnectionExt as _;

use crate::{
    ButtonId, CursorPosition, EventDisposition, ForegroundApp, HookBackend, HookError, HookEvent,
    LOGITECH_VENDOR_ID, MouseEvent,
};
use foreground::{FRONTMOST, X11Source};
pub(crate) use foreground::{
    ForegroundApplicationObserver, watch_frontmost_application_activations,
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
        FRONTMOST.frontmost_app()
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
    // Mirror the physical device's bus/vendor/product/version rather than
    // uinput's default sample ID (0x1234/0x5678): libinput and desktop
    // environments key per-device settings — pointer acceleration, scroll
    // behavior, GNOME's remembered mouse speed — off the reported vendor and
    // product ID. Leaving the sample ID in place made every re-injected mouse
    // look identical and unrecognized, so those settings silently fell back
    // to their defaults instead of the ones configured for this hardware
    // (issue #1075).
    let builder = VirtualDevice::builder()?
        .name(VIRTUAL_DEVICE_NAME)
        .input_id(device.input_id());

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

/// Negate a vertical wheel event when software scroll inversion is on.
///
/// Applied at re-emission, on the original event, so the value written back is
/// in the same evdev units the device produced — no conversion, no rounding.
/// Both wheel axes carry vertical motion (`REL_WHEEL_HI_RES` on a hi-res
/// wheel, `REL_WHEEL` otherwise); on a hi-res wheel the `REL_WHEEL`
/// companion is negated as it is released, so both stay in agreement. Horizontal scrolling is left
/// alone: the setting reverses the wheel, not the thumb wheel.
fn invert_wheel(event: InputEvent, inverted: bool) -> InputEvent {
    if !inverted {
        return event;
    }
    match event.destructure() {
        EventSummary::RelativeAxis(
            _,
            axis @ (RelativeAxisCode::REL_WHEEL | RelativeAxisCode::REL_WHEEL_HI_RES),
            value,
        ) => InputEvent::new(EventType::RELATIVE.0, axis.0, value.saturating_neg()),
        _ => event,
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

/// Low-res wheel events (`REL_WHEEL`/`REL_HWHEEL`) held back for one report.
///
/// A hi-res device emits both a hi-res and a low-res event per notch. libinput
/// ignores the low-res one when the hi-res one is present, but desktops that
/// read the *discrete* axis value (mutter 42) derive it from the low-res count,
/// so a pass-through that drops it scrolls nothing. The low-res event is
/// therefore forwarded exactly when the hi-res event on the same axis was; when
/// the hi-res one was suppressed, the action injector re-emits both and a
/// forwarded companion would double the scroll distance.
#[derive(Default)]
struct LowResCompanions {
    held: Vec<evdev::InputEvent>,
    vertical_forwarded: bool,
    horizontal_forwarded: bool,
}

impl LowResCompanions {
    fn is_low_res(axis: RelativeAxisCode) -> bool {
        matches!(
            axis,
            RelativeAxisCode::REL_WHEEL | RelativeAxisCode::REL_HWHEEL
        )
    }

    fn hold(&mut self, event: evdev::InputEvent) {
        self.held.push(event);
    }

    /// Record that the hi-res event for `axis` is being passed through.
    fn hires_forwarded(&mut self, axis: RelativeAxisCode) {
        match axis {
            RelativeAxisCode::REL_WHEEL_HI_RES => self.vertical_forwarded = true,
            RelativeAxisCode::REL_HWHEEL_HI_RES => self.horizontal_forwarded = true,
            _ => {}
        }
    }

    /// Move the companions whose hi-res event was forwarded into `pending` and
    /// reset for the next report. `inverted` applies software scroll inversion
    /// to the low-res vertical companion too, so it agrees with its hi-res
    /// event for clients that read the discrete axis.
    fn release_into(&mut self, pending: &mut Vec<evdev::InputEvent>, inverted: bool) {
        for event in self.held.drain(..) {
            let forwarded = match RelativeAxisCode(event.code()) {
                RelativeAxisCode::REL_WHEEL => self.vertical_forwarded,
                RelativeAxisCode::REL_HWHEEL => self.horizontal_forwarded,
                _ => false,
            };
            if forwarded {
                pending.push(invert_wheel(event, inverted));
            }
        }
        self.vertical_forwarded = false;
        self.horizontal_forwarded = false;
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
    let mut companions = LowResCompanions::default();
    // Software scroll inversion, read once per report so a hi-res event and its
    // low-res companion never disagree if the setting flips mid-report.
    let mut report_inverted: Option<bool> = None;

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
                let inverted = report_inverted
                    .take()
                    .unwrap_or_else(crate::scroll_inversion);
                companions.release_into(&mut pending, inverted);
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
                if hires_scroll
                    && let EventSummary::RelativeAxis(_, axis, _) = event.destructure()
                    && LowResCompanions::is_low_res(axis)
                {
                    // Redundant for the callback; whether it reaches the desktop
                    // depends on what happens to the hi-res event in this report.
                    companions.hold(event);
                    continue;
                }
                let disposition = translate(&event, hires_scroll)
                    .map_or(EventDisposition::PassThrough, |me| cb(HookEvent::Mouse(me)));
                if disposition == EventDisposition::PassThrough
                    && let EventSummary::RelativeAxis(_, axis, _) = event.destructure()
                {
                    companions.hires_forwarded(axis);
                }
                match disposition {
                    EventDisposition::PassThrough => {
                        let inverted = *report_inverted.get_or_insert_with(crate::scroll_inversion);
                        pending.push(invert_wheel(event, inverted));
                    }
                    EventDisposition::Suppress => {}
                }
            }
        }
    }

    debug!("hook stopped on {}", path.display());
    // Dropping `device` releases the exclusive grab, restoring normal input delivery.
}

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

    // ── LowResCompanions ─────────────────────────────────────────────────────

    fn rel(axis: RelativeAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, axis.0, value)
    }

    fn pending_codes(pending: &[InputEvent]) -> Vec<u16> {
        pending.iter().map(InputEvent::code).collect()
    }

    #[test]
    fn low_res_wheel_forwarded_when_hires_forwarded() {
        let mut companions = LowResCompanions::default();
        let mut pending = Vec::new();
        companions.hold(rel(RelativeAxisCode::REL_WHEEL, -1));
        companions.hires_forwarded(RelativeAxisCode::REL_WHEEL_HI_RES);
        companions.release_into(&mut pending, false);
        assert_eq!(pending_codes(&pending), [RelativeAxisCode::REL_WHEEL.0]);
    }

    #[test]
    fn low_res_wheel_dropped_when_hires_suppressed() {
        let mut companions = LowResCompanions::default();
        let mut pending = Vec::new();
        companions.hold(rel(RelativeAxisCode::REL_WHEEL, -1));
        companions.release_into(&mut pending, false);
        assert!(pending.is_empty());
    }

    #[test]
    fn low_res_axes_are_matched_independently() {
        let mut companions = LowResCompanions::default();
        let mut pending = Vec::new();
        companions.hold(rel(RelativeAxisCode::REL_WHEEL, 1));
        companions.hold(rel(RelativeAxisCode::REL_HWHEEL, 1));
        companions.hires_forwarded(RelativeAxisCode::REL_HWHEEL_HI_RES);
        companions.release_into(&mut pending, false);
        assert_eq!(pending_codes(&pending), [RelativeAxisCode::REL_HWHEEL.0]);
    }

    #[test]
    fn forwarded_state_does_not_leak_into_next_report() {
        let mut companions = LowResCompanions::default();
        let mut pending = Vec::new();
        companions.hires_forwarded(RelativeAxisCode::REL_WHEEL_HI_RES);
        companions.release_into(&mut pending, false);
        companions.hold(rel(RelativeAxisCode::REL_WHEEL, 1));
        companions.release_into(&mut pending, false);
        assert!(pending.is_empty());
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

#[cfg(test)]
mod scroll_inversion_tests {
    use super::*;

    fn rel(axis: RelativeAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::RELATIVE.0, axis.0, value)
    }

    fn value_of(event: InputEvent) -> i32 {
        event.value()
    }

    /// Both wheel axes are negated: a hi-res wheel reports `REL_WHEEL_HI_RES`
    /// and a detented one `REL_WHEEL`, and the Signature M650 this was
    /// verified against advertises both.
    #[test]
    fn inversion_negates_either_vertical_wheel_axis() {
        for axis in [
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
        ] {
            assert_eq!(value_of(invert_wheel(rel(axis, 120), true)), -120);
            assert_eq!(value_of(invert_wheel(rel(axis, -1), true)), 1);
        }
    }

    /// Off is a pass-through, byte for byte — the setting must cost nothing
    /// when it is not in use.
    #[test]
    fn inversion_off_leaves_the_wheel_alone() {
        let event = rel(RelativeAxisCode::REL_WHEEL, 120);
        assert_eq!(value_of(invert_wheel(event, false)), 120);
    }

    /// The setting reverses the wheel, not the thumb wheel or the pointer.
    #[test]
    fn inversion_leaves_horizontal_and_pointer_axes_alone() {
        for axis in [
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
        ] {
            assert_eq!(value_of(invert_wheel(rel(axis, 7), true)), 7);
        }
    }

    /// A forwarded low-res companion is negated with its hi-res event, so a
    /// client reading the discrete axis scrolls the same way as one reading
    /// the hi-res axis.
    #[test]
    fn inversion_negates_the_released_low_res_companion() {
        let mut companions = LowResCompanions::default();
        companions.hold(rel(RelativeAxisCode::REL_WHEEL, 1));
        companions.hires_forwarded(RelativeAxisCode::REL_WHEEL_HI_RES);
        let mut pending = Vec::new();
        companions.release_into(&mut pending, true);
        assert_eq!(pending.len(), 1);
        assert_eq!(value_of(pending[0]), -1);
    }

    /// `i32::MIN` has no positive counterpart; saturating keeps the scroll
    /// pinned at the extreme instead of wrapping it back to the same sign.
    #[test]
    fn inversion_saturates_instead_of_wrapping() {
        assert_eq!(
            value_of(invert_wheel(
                rel(RelativeAxisCode::REL_WHEEL, i32::MIN),
                true
            )),
            i32::MAX
        );
    }
}
