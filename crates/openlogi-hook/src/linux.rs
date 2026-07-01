//! Linux `evdev` + `uinput` implementation of the OS-level mouse hook.
//!
//! Each physical mouse found under `/dev/input/` is grabbed exclusively;
//! a paired `uinput` virtual device re-injects events the callback marks
//! [`crate::EventDisposition::PassThrough`]. Events marked
//! [`crate::EventDisposition::Suppress`] are consumed and never reach the desktop.
//!
//! # Permissions
//!
//! The process needs read access to `/dev/input/eventN` (typically the `input`
//! group) and write access to `/dev/uinput` (the `input` or `uinput` group, or
//! a `udev` rule granting access). Without those, `start()` returns
//! [`crate::HookError::Linux`].

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use evdev::uinput::VirtualDevice;
use evdev::{Device, EventSummary, KeyCode, RelativeAxisCode};
use tracing::{debug, error, warn};
use x11rb::connection::Connection as _;
use x11rb::properties::WmClass;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use crate::{ButtonId, EventDisposition, HookError, HookEvent, MouseEvent};

/// Name stamped on every uinput pass-through device; used to skip those
/// devices during enumeration so we don't hook our own virtual mice.
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

pub(crate) fn start(
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

pub(crate) fn stop(inner: HookInner) {
    shutdown(&inner.stop, &inner.stop_pipes, inner.threads);
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
        .filter(|(_, d)| d.name().unwrap_or("") != VIRTUAL_DEVICE_NAME)
        .filter(|(_, d)| {
            d.supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::BTN_LEFT))
        })
        .collect()
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

fn scroll(delta_x: f32, delta_y: f32) -> MouseEvent {
    // evdev delivers the wheel and the trackpad as distinct devices, so a wheel
    // event is always a mouse wheel — never a trackpad gesture.
    MouseEvent::Scroll {
        delta_x,
        delta_y,
        from_trackpad: false,
        device: None,
    }
}

fn translate(event: &evdev::InputEvent, hires_scroll: bool) -> Option<MouseEvent> {
    match event.destructure() {
        EventSummary::Key(_, key, value) => {
            let id = key_to_button(key)?;
            Some(MouseEvent::Button {
                id,
                pressed: value != 0,
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
                #[allow(clippy::cast_precision_loss)]
                // scroll deltas fit comfortably in f32 mantissa
                let v = value as f32;
                if hires_scroll {
                    match axis {
                        RelativeAxisCode::REL_WHEEL_HI_RES => {
                            Some(scroll(0.0, v / HIRES_UNITS_PER_TICK))
                        }
                        RelativeAxisCode::REL_HWHEEL_HI_RES => {
                            Some(scroll(v / HIRES_UNITS_PER_TICK, 0.0))
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

// All params are owned: path/cb/stop/stop_rx are moved into the thread and must not be refs.
#[allow(clippy::needless_pass_by_value)]
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
                    EventDisposition::PassThrough => pending.push(event),
                    EventDisposition::Suppress => {}
                }
            }
        }
    }

    debug!("hook stopped on {}", path.display());
    // Dropping `device` releases the exclusive grab, restoring normal input delivery.
}

// ── frontmost_bundle_id ──────────────────────────────────────────────────────

struct X11State {
    conn: RustConnection,
    root: Window,
    net_active_window: Atom,
}

static X11_STATE: LazyLock<Option<X11State>> = LazyLock::new(|| {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|e| debug!("X11 not available, frontmost_bundle_id will return None: {e}"))
        .ok()?;
    let root = conn.setup().roots[screen_num].root;
    let net_active_window = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    Some(X11State {
        conn,
        root,
        net_active_window,
    })
});

/// Return the X11 `WM_CLASS` class component of the currently active window,
/// e.g. `"Firefox"` or `"Code"`.
///
/// Returns `None` when there is no active window, when the X11 display is
/// unavailable (Wayland-only session without XWayland), or on read error.
/// Native Wayland windows are not visible through this path.
pub(crate) fn frontmost_bundle_id() -> Option<String> {
    let state = X11_STATE.as_ref()?;

    // _NET_ACTIVE_WINDOW on the root window holds the focused window's XID.
    let window: Window = state
        .conn
        .get_property(
            false,
            state.root,
            state.net_active_window,
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
    let wm = WmClass::get(&state.conn, window)
        .ok()?
        .reply_unchecked()
        .ok()??;
    std::str::from_utf8(wm.class())
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
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
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::LeftClick,
                pressed: true
            })
        ));
    }

    #[test]
    fn translate_btn_left_up_returns_button_released() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 0);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::LeftClick,
                pressed: false
            })
        ));
    }

    #[test]
    fn translate_btn_back_returns_back() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_BACK.0, 1);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Back,
                pressed: true
            })
        ));
    }

    #[test]
    fn translate_btn_side_returns_back() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_SIDE.0, 1);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Back,
                pressed: true
            })
        ));
    }

    #[test]
    fn translate_btn_forward_returns_forward() {
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_FORWARD.0, 1);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Button {
                id: ButtonId::Forward,
                pressed: true
            })
        ));
    }

    // ── movement ─────────────────────────────────────────────────────────────

    #[test]
    fn translate_rel_x_returns_horizontal_move() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, 7);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Moved {
                delta_x: 7,
                delta_y: 0
            })
        ));
    }

    #[test]
    fn translate_rel_y_returns_vertical_move() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, -4);
        assert!(matches!(
            translate(&event, false),
            Some(MouseEvent::Moved {
                delta_x: 0,
                delta_y: -4
            })
        ));
    }

    // ── scroll — standard ────────────────────────────────────────────────────

    #[test]
    fn translate_rel_wheel_returns_scroll_y() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, 3);
        let result = translate(&event, false);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta_x, delta_y, .. })
                if delta_x.abs() < f32::EPSILON && (delta_y - 3.0).abs() < f32::EPSILON),
            "expected Scroll {{ delta_x: 0.0, delta_y: 3.0 }}, got {result:?}"
        );
    }

    #[test]
    fn translate_rel_hwheel_returns_scroll_x() {
        let event = InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL.0, -2);
        let result = translate(&event, false);
        assert!(
            matches!(result, Some(MouseEvent::Scroll { delta_x, delta_y, .. })
                if (delta_x - -2.0).abs() < f32::EPSILON && delta_y.abs() < f32::EPSILON),
            "expected Scroll {{ delta_x: -2.0, delta_y: 0.0 }}, got {result:?}"
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
            matches!(result, Some(MouseEvent::Scroll { delta_x, delta_y, .. })
                if delta_x.abs() < f32::EPSILON && (delta_y - 0.5).abs() < f32::EPSILON),
            "expected Scroll {{ delta_x: 0.0, delta_y: 0.5 }}, got {result:?}"
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
            matches!(result, Some(MouseEvent::Scroll { delta_x, delta_y, .. })
                if (delta_x - -1.0).abs() < f32::EPSILON && delta_y.abs() < f32::EPSILON),
            "expected Scroll {{ delta_x: -1.0, delta_y: 0.0 }}, got {result:?}"
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
}
