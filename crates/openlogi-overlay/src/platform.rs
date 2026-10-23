//! Native window policy for the standalone overlay: the Actions Ring and its
//! notices.

#[cfg(not(target_os = "windows"))]
mod placement;
// Keep Windows geometry tests runnable on the host; only native.rs needs Win32.
#[cfg(any(target_os = "windows", test))]
mod windows;

#[cfg(not(target_os = "windows"))]
pub(crate) use placement::{NoticePlacement, RingPlacement};
#[cfg(target_os = "windows")]
pub(crate) use windows::{NoticePlacement, RingPlacement};

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, warn};

/// Open the first of `alternatives` that shows a window, returning the first
/// window it opened.
///
/// Each alternative is a whole set of windows, best first. Within a set, one
/// window that fails to open must not sink the others; a set that opens none
/// gives way to the next. A refusal the compositor repeats for the rest of the
/// process ends its set at once.
pub(crate) fn open_first<W, H>(
    alternatives: Vec<Vec<W>>,
    open: impl FnMut(W) -> anyhow::Result<H>,
) -> Option<H> {
    open_first_with(alternatives, open, refused_for_good)
}

fn open_first_with<W, H>(
    alternatives: Vec<Vec<W>>,
    mut open: impl FnMut(W) -> anyhow::Result<H>,
    refused_for_good: impl Fn(&anyhow::Error) -> bool,
) -> Option<H> {
    let last = alternatives.len().saturating_sub(1);
    for (index, windows) in alternatives.into_iter().enumerate() {
        let mut first = None;
        for window in windows {
            match open(window) {
                Ok(handle) => {
                    first.get_or_insert(handle);
                }
                Err(error) if refused_for_good(&error) => {
                    debug!(%error, "the compositor refuses this kind of overlay window");
                    break;
                }
                Err(error) if index == last => warn!(%error, "could not open an overlay window"),
                Err(error) => debug!(%error, "could not open a preferred overlay window"),
            }
        }
        if first.is_some() {
            return first;
        }
    }
    None
}

/// Set once the compositor has refused a layer-shell surface.
///
/// GPUI binds the compositor's globals once, when it connects, so a refusal
/// holds for the life of the process. Asking again would only be refused
/// again, and each refused request leaves behind the `wl_surface` GPUI
/// created for it.
#[cfg(target_os = "linux")]
static LAYER_SHELL_REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether `error` is the compositor refusing a layer-shell surface, which
/// it then does for the rest of the process. A refusal is remembered.
#[cfg(target_os = "linux")]
fn refused_for_good(error: &anyhow::Error) -> bool {
    let refused = error
        .downcast_ref::<gpui::layer_shell::LayerShellNotSupportedError>()
        .is_some();
    if refused {
        LAYER_SHELL_REFUSED.store(true, Ordering::Relaxed);
    }
    refused
}

/// Only a Wayland compositor refuses a kind of window for good.
#[cfg(not(target_os = "linux"))]
const fn refused_for_good(_error: &anyhow::Error) -> bool {
    false
}

/// Whether the compositor has refused a layer-shell surface, so asking for
/// another is pointless.
#[cfg(target_os = "linux")]
pub(crate) fn layer_shell_refused() -> bool {
    LAYER_SHELL_REFUSED.load(Ordering::Relaxed)
}

/// Keep the overlay out of the Dock and app switcher.
#[cfg(target_os = "macos")]
pub fn configure_application() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(marker) = MainThreadMarker::new() {
        NSApplication::sharedApplication(marker)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Make the transparent ring panel borderless and remove its native shadow.
#[cfg(target_os = "macos")]
pub fn configure_windows() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowStyleMask};

    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker).windows() {
            window.setStyleMask(NSWindowStyleMask::NonactivatingPanel);
            window.setHasShadow(false);
        }
    }
}

/// No native application policy is required away from macOS.
#[cfg(not(target_os = "macos"))]
pub fn configure_application() {}

/// Linux needs no additional native window configuration here.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn configure_windows() {}

/// Owner of the native click-away event monitor; dropping it removes the
/// monitor. Create and drop on the main thread.
#[cfg(target_os = "macos")]
pub struct ClickAwayMonitor(objc2::rc::Retained<objc2::runtime::AnyObject>);

#[cfg(target_os = "macos")]
impl Drop for ClickAwayMonitor {
    #[expect(
        unsafe_code,
        reason = "NSEvent::removeMonitor is plain AppKit FFI; the token is exactly what addGlobalMonitor returned"
    )]
    fn drop(&mut self) {
        // SAFETY: `self.0` is the monitor token returned by
        // `addGlobalMonitorForEventsMatchingMask_handler`, removed only once.
        unsafe { objc2_app_kit::NSEvent::removeMonitor(&self.0) };
    }
}

/// Invoke `on_mouse_down` for every mouse-down that macOS delivers to *other*
/// applications, for as long as the returned monitor is held.
///
/// Global `NSEvent` monitors never see events routed to this process's own
/// windows and cannot consume the events they observe — together exactly the
/// ring's click-away contract: clicks on the ring keep hitting the GPUI
/// handlers they always did, while a click anywhere else can dismiss the ring
/// without being swallowed. Must be called on the main thread (returns `None`
/// off it); the handler runs on the main run loop.
#[cfg(target_os = "macos")]
pub fn watch_clicks_outside(on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask};

    MainThreadMarker::new()?;
    let handler: block2::RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>)> =
        block2::RcBlock::new(move |_event| on_mouse_down());
    NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &handler,
    )
    .map(ClickAwayMonitor)
}

/// Away from macOS no global click monitor is available; the ring keeps its
/// in-window dismissal paths (center ×, slot activation, timeout).
#[cfg(not(target_os = "macos"))]
pub struct ClickAwayMonitor(());

#[cfg(not(target_os = "macos"))]
pub fn watch_clicks_outside(_on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    None
}

/// Whether the host OS asks for reduced UI motion. The overlay calls this on
/// every ring open. macOS and Windows read the live setting; Linux answers from
/// its last background read without blocking (see `gnome_motion`).
#[must_use]
pub fn prefers_reduced_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(target_os = "windows")]
    {
        client_area_animation_enabled().is_some_and(|enabled| !enabled)
    }
    #[cfg(target_os = "linux")]
    {
        gnome_motion::refresh();
        gnome_motion::reduced()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "SystemParametersInfoW is plain Win32 FFI; std and windows-sys offer no safe reader for SPI_GETCLIENTAREAANIMATION"
)]
fn client_area_animation_enabled() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    let mut enabled = 0i32;
    // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one `BOOL` when the call succeeds.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    if ok == 0 {
        return None;
    }
    Some(enabled != 0)
}

/// GNOME's `enable-animations`, read by `gsettings` off the UI thread. Each
/// call to [`prefers_reduced_motion`] starts one background read (at most one
/// at a time) and answers from the last value read, so a changed setting
/// applies from the next open and a slow `gsettings` never stalls a window.
/// No answer yet, no GNOME, or no `gsettings`: motion stays on.
#[cfg(target_os = "linux")]
mod gnome_motion {
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    const UNKNOWN: u8 = 0;
    const ANIMATIONS_ON: u8 = 1;
    const ANIMATIONS_OFF: u8 = 2;

    static LAST: AtomicU8 = AtomicU8::new(UNKNOWN);
    static READING: AtomicBool = AtomicBool::new(false);

    pub(super) fn reduced() -> bool {
        LAST.load(Ordering::Relaxed) == ANIMATIONS_OFF
    }

    pub(super) fn refresh() {
        if READING.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("gnome-motion".into())
            .spawn(|| {
                let mut command = Command::new("gsettings");
                command.args(["get", "org.gnome.desktop.interface", "enable-animations"]);
                if let Some(enabled) =
                    super::read_bool_command(command, super::MOTION_QUERY_TIMEOUT)
                {
                    let value = if enabled {
                        ANIMATIONS_ON
                    } else {
                        ANIMATIONS_OFF
                    };
                    LAST.store(value, Ordering::Relaxed);
                }
                READING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            READING.store(false, Ordering::Release);
        }
    }
}

/// How long a background `gsettings` read may run before it is killed.
#[cfg(any(target_os = "linux", all(test, unix)))]
const MOTION_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Run `command` and parse its stdout as a `gsettings` boolean, killing it if
/// it has not exited within `timeout`. Blocks the calling (background) thread.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn read_bool_command(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Option<bool> {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut stdout = String::new();
                child.stdout.take()?.read_to_string(&mut stdout).ok()?;
                return parse_gsettings_bool(&stdout);
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `gsettings get` prints a GVariant boolean; anything else is no signal.
#[cfg(any(target_os = "linux", test))]
fn parse_gsettings_bool(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod reduced_motion_tests {
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::parse_gsettings_bool;
    #[cfg(unix)]
    use super::{MOTION_QUERY_TIMEOUT, read_bool_command};

    #[test]
    fn gsettings_booleans_parse_and_anything_else_is_no_signal() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool("false\n"), Some(false));
        assert_eq!(parse_gsettings_bool(""), None);
        assert_eq!(parse_gsettings_bool("No such schema\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_motion_query_returns_its_parsed_answer() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo false"]);
        assert_eq!(
            read_bool_command(command, Duration::from_secs(5)),
            Some(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_motion_query_is_killed_at_its_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5; echo false"]);
        let started = Instant::now();
        assert_eq!(read_bool_command(command, MOTION_QUERY_TIMEOUT), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

/// One display's global geometry, in the same top-left-origin global point
/// space that `openlogi_hook::cursor_position()` reports.
#[cfg(not(target_os = "windows"))]
pub struct CursorDisplay {
    /// Native display id; on macOS the `CGDirectDisplayID`, numerically equal
    /// to GPUI's `DisplayId` for the same display.
    pub id: u64,
    /// Global origin (top-left corner) of the display, in points.
    pub origin: (f64, f64),
    /// Display size in points.
    pub size: (f64, f64),
}

/// Find the display whose global bounds contain the point `(x, y)`.
///
/// GPUI's `PlatformDisplay::bounds()` zeroes every display's origin (window
/// bounds are display-relative), so mapping a global cursor position to its
/// display has to go through CoreGraphics.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CGGetActiveDisplayList/CGDisplayBounds are plain C FFI; GPUI exposes no global display bounds"
)]
pub fn display_containing(x: f64, y: f64) -> Option<CursorDisplay> {
    use core_graphics::display::{CGDisplayBounds, CGGetActiveDisplayList};

    const MAX_DISPLAYS: u32 = 32;
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: the list write is bounded by the capacity we pass; `count`
    // reports how many entries were actually filled.
    let result = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if result != 0 {
        return None;
    }
    ids.iter().take(count as usize).find_map(|&id| {
        // SAFETY: side-effect-free C getter on an id from the active list.
        let bounds = unsafe { CGDisplayBounds(id) };
        let contains = x >= bounds.origin.x
            && x < bounds.origin.x + bounds.size.width
            && y >= bounds.origin.y
            && y < bounds.origin.y + bounds.size.height;
        contains.then(|| CursorDisplay {
            id: u64::from(id),
            origin: (bounds.origin.x, bounds.origin.y),
            size: (bounds.size.width, bounds.size.height),
        })
    })
}

/// On Linux the GPUI display list already carries global origins, so there is
/// nothing to resolve natively.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn display_containing(_x: f64, _y: f64) -> Option<CursorDisplay> {
    None
}

/// The part of `display` that windows may use — without the menu bar and the
/// Dock — in the display's own top-left-origin points.
///
/// AppKit reports this per `NSScreen`, in its own bottom-left-origin global
/// space; [`placement::usable_in_display`] finds the screen that is `display`
/// and translates.
#[cfg(target_os = "macos")]
pub fn usable_area(display: &CursorDisplay) -> Option<gpui::Bounds<gpui::Pixels>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let screens = NSScreen::screens(MainThreadMarker::new()?);
    let primary_height = screens.firstObject()?.frame().size.height;
    screens.to_vec().iter().find_map(|screen| {
        let (frame, visible) = (screen.frame(), screen.visibleFrame());
        placement::usable_in_display(
            placement::CocoaRect {
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
            },
            placement::CocoaRect {
                x: visible.origin.x,
                y: visible.origin.y,
                width: visible.size.width,
                height: visible.size.height,
            },
            primary_height,
            display.origin,
        )
    })
}

/// Linux resolves no native display, so there is no usable area to report.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn usable_area(_display: &CursorDisplay) -> Option<gpui::Bounds<gpui::Pixels>> {
    None
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A window that opens as `name`, or fails to with `reason`.
    type Attempt = (&'static str, Result<(), &'static str>);

    /// Open `alternatives`, recording each window tried; `refusal` is the one
    /// failure the compositor repeats for good.
    fn open(
        alternatives: Vec<Vec<Attempt>>,
        refusal: &'static str,
    ) -> (Option<&'static str>, Vec<&'static str>) {
        let tried = RefCell::new(Vec::new());
        let opened = open_first_with(
            alternatives,
            |(name, outcome)| {
                tried.borrow_mut().push(name);
                outcome.map(|()| name).map_err(anyhow::Error::msg)
            },
            |error| error.to_string() == refusal,
        );
        (opened, tried.into_inner())
    }

    #[test]
    fn the_first_set_that_opens_a_window_is_the_only_one_opened() {
        let (opened, tried) = open(
            vec![
                vec![("left", Ok(())), ("right", Ok(()))],
                vec![("fallback", Ok(()))],
            ],
            "refused",
        );
        assert_eq!(opened, Some("left"));
        assert_eq!(tried, ["left", "right"]);
    }

    #[test]
    fn one_window_that_fails_does_not_sink_the_rest_of_its_set() {
        let (opened, tried) = open(
            vec![
                vec![("left", Err("lost its output")), ("right", Ok(()))],
                vec![("fallback", Ok(()))],
            ],
            "refused",
        );
        assert_eq!(opened, Some("right"));
        assert_eq!(tried, ["left", "right"]);
    }

    #[test]
    fn a_set_that_opens_nothing_gives_way_to_the_next() {
        let (opened, tried) = open(
            vec![
                vec![("left", Err("failed")), ("right", Err("failed"))],
                vec![("fallback", Ok(()))],
            ],
            "refused",
        );
        assert_eq!(opened, Some("fallback"));
        assert_eq!(tried, ["left", "right", "fallback"]);
    }

    #[test]
    fn a_refusal_for_good_ends_its_set_at_once() {
        let (opened, tried) = open(
            vec![
                vec![("left", Err("refused")), ("right", Ok(()))],
                vec![("fallback", Ok(()))],
            ],
            "refused",
        );
        assert_eq!(opened, Some("fallback"));
        assert_eq!(
            tried,
            ["left", "fallback"],
            "the rest of the set is not asked for"
        );
    }

    #[test]
    fn nothing_opens_when_every_set_fails() {
        let (opened, tried) = open(
            vec![
                vec![("left", Err("refused"))],
                vec![("fallback", Err("failed"))],
            ],
            "refused",
        );
        assert_eq!(opened, None);
        assert_eq!(tried, ["left", "fallback"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_layer_shell_refusal_ends_its_set_and_is_remembered() {
        assert!(!refused_for_good(&anyhow::anyhow!("lost its output")));
        let mut tried = Vec::new();
        let opened = open_first(vec![vec!["left", "right"], vec!["fallback"]], |name| {
            tried.push(name);
            if name == "fallback" {
                Ok(name)
            } else {
                Err(anyhow::Error::from(
                    gpui::layer_shell::LayerShellNotSupportedError,
                ))
            }
        });
        assert_eq!(opened, Some("fallback"));
        assert_eq!(tried, ["left", "fallback"]);
        assert!(layer_shell_refused());
    }
}
