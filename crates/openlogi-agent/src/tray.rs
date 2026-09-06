//! The agent's macOS AppKit loop, menu-bar item, and activity gate.
//!
//! The always-on agent hosts the menu bar (the GUI is on-demand). The item
//! carries GUI-directed actions ("Show Main Window", Settings, About, Check for
//! Updates) and "Quit OpenLogi"; the GitHub/help links live in the GUI's own
//! menu bar, not here. Clicks fire on the main thread's AppKit run loop.
//!
//! GUI-directed actions open [`DeeplinkCommand`] `openlogi://` URLs which macOS
//! delivers to the GUI via Apple Events — works for both cold start (app
//! launched then URL delivered) and warm reactivation (URL delivered to the
//! running app).
//!
//! macOS-only. AppKit objects are `Retained<T>` (no #99-style leaks); the run
//! loop owns the main thread for the agent's lifetime.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init, action targets, selector-based workspace notifications, the CoreGraphics display-list read, and the IOKit registry read of the system capability set"
)]

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::NSStatusItem;
#[cfg(test)]
use objc2_app_kit::NSWorkspaceDidWakeNotification;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSRunningApplication, NSWorkspace,
    NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification,
    NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceSessionDidResignActiveNotification,
    NSWorkspaceWillSleepNotification,
};
use objc2_core_foundation::{CFBoolean, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayIsAsleep, CGError, CGEventSource, CGEventSourceStateID,
    CGEventType, CGGetActiveDisplayList, CGSessionCopyCurrentDictionary,
};
use objc2_foundation::{NSNotification, NSString};
use objc2_io_kit::{
    IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingService,
    IOServiceMatching, kIOMainPortDefault, kIOPMSystemCapabilityGraphics,
};
use openlogi_core::brand::{self, DeeplinkCommand};
use openlogi_core::config::AppIcon;
use openlogi_hid::DeviceIoSignal;
use tracing::{debug, info, warn};

use crate::shutdown::{self, ShutdownRequestSender};
use crate::status_item;

/// The installed menu-bar item plus the action target its menu items weakly
/// reference — everything a later config reload needs to restyle the icon or
/// rebuild the menu in a new language.
struct TrayState {
    item: Retained<NSStatusItem>,
    target: Retained<MenuTarget>,
}

thread_local! {
    /// The installed tray state, kept where a later config reload can find it.
    /// A `thread_local` rather than a global: everything that touches it runs
    /// on the main thread, which is the same thread that installed it, so the
    /// affinity AppKit demands is the affinity the storage already has.
    static TRAY: RefCell<Option<TrayState>> = const { RefCell::new(None) };
    /// Where menu actions hand process termination to the async lifecycle.
    /// Kept separately because the AppKit loop still exists when the status
    /// item is hidden by preference.
    static SHUTDOWN_TX: RefCell<Option<ShutdownRequestSender>> = const { RefCell::new(None) };
}

/// The menu-bar glyph for `icon`: a monochrome template the system tints for
/// the current menu bar, not the app icon itself — which is why these are
/// hand-drawn silhouettes rather than renders of the Icon Composer documents.
const fn glyph(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::Openlogi => include_bytes!("../assets/tray-icon@2x.png"),
        AppIcon::Prism => include_bytes!("../assets/tray-icon-prism@2x.png"),
    }
}

/// Point the menu-bar item at `icon`'s glyph, so picking an app icon changes
/// every surface that shows one rather than all but this.
///
/// Callable from anywhere: the work hops to the main queue, where AppKit lives
/// and where the status item was installed. A no-op when the item is hidden or
/// the loop never started.
pub fn set_icon(icon: AppIcon) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        TRAY.with_borrow(|state| {
            if let Some(state) = state.as_ref() {
                status_item::set_png_icon(&state.item, mtm, glyph(icon), "OpenLogi");
            }
        });
    });
}

/// Rebuild the menu-bar menu with the current locale's titles, after a config
/// reload switched the interface language. Same shape as [`set_icon`]: the
/// work hops to the main queue, and a hidden item is a no-op.
pub fn relocalize() {
    DispatchQueue::main().exec_async(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        TRAY.with_borrow(|state| {
            if let Some(state) = state.as_ref() {
                // The status item retains the menu; the fresh one replaces the
                // old wholesale so titles, order, and key equivalents cannot
                // drift from `build_menu`.
                let menu = build_menu(mtm, &state.target);
                state.item.setMenu(Some(&menu));
            }
        });
    });
}

/// What the suspend sources hold, and since when.
///
/// The instant matters: a level read can only discharge a suspension by
/// proving something happened *after* the suspension was recorded.
struct Suspension {
    held: u8,
    since: Instant,
}

/// The activity state the workspace observers and the reconciler share.
///
/// One domain fact — may the agent touch hardware — with one transition
/// authority: every writer goes through [`ActivitySources::suspend_from`] /
/// [`ActivitySources::resume_from`], so the published [`DeviceIoSignal`] can
/// never drift from the recorded suspend sources.
struct ActivitySources {
    signal: DeviceIoSignal,
    suspension: Mutex<Suspension>,
    /// Signalled on every change to `suspension` so the reconciler parks
    /// rather than polls while the gate is open.
    reconcile: Condvar,
}

struct ActivityTargetIvars {
    sources: Arc<ActivitySources>,
}

const SYSTEM_SLEEP: u8 = 1 << 0;
const SCREEN_SLEEP: u8 = 1 << 1;
const SESSION_INACTIVE: u8 = 1 << 2;
const STARTUP: u8 = 1 << 3;

/// Log-facing names for the suspend sources. A "device I/O paused" line that
/// does not say which lifecycle event produced it cannot be diagnosed from a
/// user's log.
const SOURCE_NAMES: [(u8, &str); 4] = [
    (SYSTEM_SLEEP, "system-sleep"),
    (SCREEN_SLEEP, "screens-asleep"),
    (SESSION_INACTIVE, "session-inactive"),
    (STARTUP, "startup"),
];

/// A set of suspend sources, rendered as `screens-asleep+session-inactive`.
struct Sources(u8);

impl fmt::Display for Sources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("none");
        }
        let mut separator = "";
        for (source, name) in SOURCE_NAMES {
            if self.0 & source != 0 {
                f.write_str(separator)?;
                f.write_str(name)?;
                separator = "+";
            }
        }
        Ok(())
    }
}

/// The shortest interval macOS lets the display be configured to blank after
/// (`pmset displaysleep 1`). Any HID input wakes a sleeping display, so input
/// newer than this rules out every display-sleep *timeout* — which is all the
/// proof there is when nothing has reported the display asleep.
const DISPLAY_SLEEP_IDLE_FLOOR: Duration = Duration::from_secs(60);

/// How often the reconciler re-reads the levels while a suspension it could
/// discharge is outstanding.
///
/// Short, because the startup hold fails closed: until it is discharged the
/// agent does no device I/O at all, so a launch into a live session must
/// recover in seconds. It costs the missed-wake case nothing to be early — the
/// relative proof below only accepts input that arrived *after* the suspension
/// was recorded, so an early tick is a stricter test, not a racier one.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// Whether `held` is a suspension a level read may discharge.
///
/// `SYSTEM_SLEEP` deliberately is not: the process runs during a maintenance
/// DarkWake, and opening HID there is exactly what promoted an invisible wake
/// into a full display wake (#656). A system sleep is cleared only by the wake
/// notification that pairs with it.
const fn reconcilable(held: u8) -> bool {
    held != 0 && held & SYSTEM_SLEEP == 0
}

/// What power management's system capability set says about graphics.
///
/// The system runs code in three states, and only one of them can be showing
/// the user anything: full wake, a DarkWake (the CPU is up, the panels are
/// not), and sleep. The window-server levels below cannot tell the first two
/// apart — see [`system_graphics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SystemGraphics {
    /// The capability set carries `kIOPMSystemCapabilityGraphics`: this is a
    /// full wake, so the other levels are worth reading.
    Up,
    /// The capability set was read and the graphics bit is clear: a DarkWake.
    /// Nothing is on screen, whatever else the levels say.
    Down,
    /// The property could not be read, so it proves nothing either way and
    /// must not be allowed to hold the gate closed on its own.
    Unknown,
}

/// One reading of every level the owner can check its notification bookkeeping
/// against. Taken as a value so the decision below is pure and testable
/// without a display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivityLevels {
    /// `kCGSSessionOnConsoleKey`, trustworthy in both directions: it is the
    /// level whose edges `NSWorkspaceSessionDidBecomeActive` /
    /// `…DidResignActive` announce.
    on_console: bool,
    /// `CGDisplayIsAsleep` across the active display list — trustworthy only
    /// when it says *asleep*. See [`displays_report_asleep`].
    displays_report_asleep: bool,
    /// Whether the system as a whole has graphics. See [`system_graphics`].
    graphics: SystemGraphics,
    /// How long the HID system has been idle.
    idle: Duration,
}

/// Which of `held`'s sources `levels` prove are over, given how long `held` has
/// stood.
///
/// Never returns `SYSTEM_SLEEP`; see [`reconcilable`].
fn discharged_by(held: u8, held_for: Duration, levels: ActivityLevels) -> u8 {
    if !levels.on_console {
        // Another user owns the console. Nothing resumes into their session —
        // which is also what keeps this from fighting the input hook that
        // follows the same gate.
        return 0;
    }
    if levels.graphics == SystemGraphics::Down {
        // A DarkWake: the machine is running with the panels off, so *nothing*
        // is visible and no level below can prove otherwise. The console level
        // still reads true, the idle timer still counts from whatever the user
        // did before the lid shut, and — after a lid-close display
        // reconfiguration — `CGDisplayIsAsleep` reports the re-enumerated
        // display awake. This is the necessary condition all three of those
        // are missing.
        return 0;
    }
    // The console level is direct proof, so it discharges the session source on
    // its own — exactly as `SessionDidBecomeActive` does, and like that
    // notification it says nothing about the display.
    let mut cleared = SESSION_INACTIVE;
    if display_is_proven_awake(held, held_for, levels) {
        cleared |= SCREEN_SLEEP | STARTUP;
    }
    cleared & held
}

/// Whether the display can be *proved* on. There is no reading that proves it
/// off-and-on again, so this is deliberately one-directional: unproven keeps
/// the gate closed.
fn display_is_proven_awake(held: u8, held_for: Duration, levels: ActivityLevels) -> bool {
    if levels.displays_report_asleep {
        // The one direction `CGDisplayIsAsleep` is trustworthy in.
        return false;
    }
    if held & SCREEN_SLEEP == 0 {
        // Nothing has reported this display asleep, so the only thing that
        // could have blanked it is an idle timeout — and input newer than the
        // shortest timeout macOS allows rules that out. This is the startup
        // case: a relaunched agent has no notification history at all.
        return levels.idle < DISPLAY_SLEEP_IDLE_FLOOR;
    }
    // The display *was* reported asleep when this source was recorded, so the
    // idle floor proves nothing — a hot corner blanks the display a second
    // after the user's last keystroke. Only input that arrived after the
    // suspension can have woken it, because any HID input wakes a sleeping
    // display.
    levels.idle < held_for
}

/// What [`ActivityTarget::finish_startup`] left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupDisplay {
    /// The display was proved awake; the startup hold is lifted.
    Awake,
    /// The display could not be proved awake, so the startup hold still keeps
    /// the gate closed and the reconciler has to lift it later.
    Unproven,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `ActivityTarget`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = ActivityTargetIvars]
    #[name = "OpenLogiAgentWorkspaceActivityTarget"]
    struct ActivityTarget;

    impl ActivityTarget {
        #[unsafe(method(workspaceWillSleep:))]
        fn workspace_will_sleep(&self, _notification: &NSNotification) {
            self.suspend_from(SYSTEM_SLEEP);
        }

        #[unsafe(method(workspaceScreensDidSleep:))]
        fn workspace_screens_did_sleep(&self, _notification: &NSNotification) {
            self.suspend_from(SCREEN_SLEEP);
        }

        #[unsafe(method(workspaceSessionDidResignActive:))]
        fn workspace_session_did_resign_active(&self, _notification: &NSNotification) {
            self.suspend_from(SESSION_INACTIVE);
        }

        #[unsafe(method(workspaceScreensDidWake:))]
        fn workspace_screens_did_wake(&self, _notification: &NSNotification) {
            // A real screen wake is direct proof of a live display, so it also
            // discharges a startup hold no level read could.
            self.resume_from(SYSTEM_SLEEP | SCREEN_SLEEP | STARTUP);
        }

        #[unsafe(method(workspaceSessionDidBecomeActive:))]
        fn workspace_session_did_become_active(&self, _notification: &NSNotification) {
            self.resume_from(SYSTEM_SLEEP | SESSION_INACTIVE);
        }
    }
);

impl ActivityTarget {
    fn new(signal: DeviceIoSignal) -> Retained<Self> {
        // `main` closes the gate before spawning the core thread; repeat the
        // idempotent close here so the target's STARTUP source is self-contained
        // in tests and any future caller cannot accidentally start open.
        let _ = signal.suspend();
        let this = Self::alloc().set_ivars(ActivityTargetIvars {
            sources: Arc::new(ActivitySources {
                signal,
                suspension: Mutex::new(Suspension {
                    held: STARTUP,
                    since: Instant::now(),
                }),
                reconcile: Condvar::new(),
            }),
        });
        // SAFETY: `init` initializes our freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }

    /// The shared state behind the observers — also what the reconciler holds.
    fn sources(&self) -> &Arc<ActivitySources> {
        &self.ivars().sources
    }

    /// Release the startup hold if `levels` prove the display is on.
    ///
    /// An unproven display keeps the gate closed. That is the fail-safe
    /// direction — a relaunched agent that guesses "awake" starts probing HID
    /// behind a dark panel — and it is the direction the old
    /// `CGDisplayIsAsleep(CGMainDisplayID())` snapshot got wrong (#952).
    fn finish_startup(&self, levels: ActivityLevels) -> StartupDisplay {
        if self.sources().discharge(levels) & STARTUP == 0 {
            StartupDisplay::Unproven
        } else {
            StartupDisplay::Awake
        }
    }

    fn suspend_from(&self, source: u8) {
        self.sources().suspend_from(source);
    }

    fn resume_from(&self, sources: u8) {
        self.sources().resume_from(sources);
    }
}

impl ActivitySources {
    /// Record `source` and close the hardware gate.
    fn suspend_from(&self, source: u8) {
        let (changed, held) = {
            let mut suspension = self
                .suspension
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let was_allowed = suspension.held == 0;
            if source & !suspension.held != 0 {
                // A newly recorded source restarts the clock the relative proof
                // in `display_is_proven_awake` measures input against.
                suspension.since = Instant::now();
            }
            suspension.held |= source;
            (was_allowed && self.signal.suspend(), suspension.held)
        };
        self.reconcile.notify_all();
        if changed {
            info!(source = %Sources(source), "display/session suspended — pausing device I/O");
        } else {
            debug!(
                source = %Sources(source),
                held = %Sources(held),
                "additional display/session suspend source"
            );
        }
    }

    /// Clear `sources`; once nothing is left holding it, reopen the gate.
    fn resume_from(&self, sources: u8) {
        let (changed, cleared, held) = {
            let mut suspension = self
                .suspension
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let cleared = suspension.held & sources;
            let was_suspended = suspension.held != 0;
            suspension.held &= !sources;
            (
                was_suspended && suspension.held == 0 && self.signal.resume(),
                cleared,
                suspension.held,
            )
        };
        self.reconcile.notify_all();
        if changed {
            info!(cleared = %Sources(cleared), "display/session resumed — enabling device I/O");
        } else if cleared != 0 {
            debug!(
                cleared = %Sources(cleared),
                held = %Sources(held),
                "display/session partially resumed — device I/O still paused"
            );
        }
    }

    /// Discharge every held source `levels` prove is over, and report which.
    /// The one place a level read is allowed to move the gate.
    fn discharge(&self, levels: ActivityLevels) -> u8 {
        self.discharge_at(levels, Instant::now())
    }

    /// [`Self::discharge`] with an explicit `now`, so the time-dependent half
    /// of the decision is testable.
    fn discharge_at(&self, levels: ActivityLevels, now: Instant) -> u8 {
        let (held, held_for) = {
            let suspension = self
                .suspension
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                suspension.held,
                now.saturating_duration_since(suspension.since),
            )
        };
        let cleared = discharged_by(held, held_for, levels);
        if cleared != 0 {
            self.resume_from(cleared);
        }
        cleared
    }

    /// Start the process-lifetime reconciler thread.
    ///
    /// Owned by the production launch sequence rather than by
    /// [`install_activity_observer`], so the unit tests drive
    /// [`ActivitySources::discharge_at`] directly with no live thread racing
    /// them.
    fn start_reconciler(self: &Arc<Self>) {
        let sources = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("openlogi-activity-reconcile".into())
            .spawn(move || sources.reconcile_forever());
        if let Err(error) = spawned {
            warn!(
                %error,
                "could not start the display/session reconciler — a dropped wake notification or an unproven launch would pause device I/O until the agent restarts"
            );
        }
    }

    /// Park until a discharge-able suspension has stood for
    /// [`RECONCILE_INTERVAL`], then check it against the levels.
    ///
    /// Both an open gate and a system sleep park on the condvar, so this
    /// performs no timed work and issues no CoreGraphics call in either state.
    fn reconcile_forever(&self) -> ! {
        loop {
            let outstanding = {
                let mut suspension = self
                    .suspension
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                while !reconcilable(suspension.held) {
                    suspension = self
                        .reconcile
                        .wait(suspension)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                let (suspension, _) = self
                    .reconcile
                    .wait_timeout_while(suspension, RECONCILE_INTERVAL, |suspension| {
                        reconcilable(suspension.held)
                    })
                    .unwrap_or_else(PoisonError::into_inner);
                reconcilable(suspension.held)
            };
            if !outstanding {
                continue;
            }
            // Read the levels outside the lock: these are window-server round
            // trips, and nothing else may block on them.
            let levels = read_levels();
            let missed = self.discharge(levels) & (SCREEN_SLEEP | SESSION_INACTIVE);
            if missed != 0 {
                warn!(
                    cleared = %Sources(missed),
                    graphics = ?levels.graphics,
                    idle_secs = levels.idle.as_secs_f64(),
                    "the display/session levels disagreed with the last notification — a wake notification never arrived; reconciled"
                );
            }
        }
    }
}

/// Read every level once.
///
/// Window-server state and one IORegistry property: no HID, no Bluetooth,
/// nothing that could promote a maintenance DarkWake into a full display wake
/// (#656).
fn read_levels() -> ActivityLevels {
    ActivityLevels {
        on_console: session_is_on_console(),
        displays_report_asleep: displays_report_asleep(),
        graphics: system_graphics(),
        idle: seconds_since_last_input(),
    }
}

/// Whether the system is running with graphics, read from `IOPMrootDomain`'s
/// `System Capabilities` property.
///
/// The bit values are public — `<IOKit/pwr_mgt/IOPM.h>` declares
/// `kIOPMSystemCapabilityCPU/Graphics/Audio/Network`, and `objc2-io-kit`
/// generates them — but the registry key that carries the current set is not
/// in any SDK header. This is nonetheless an ordinary IORegistry read
/// (`IOServiceGetMatchingService` + `IORegistryEntryCreateCFProperty`, the same
/// pair `ioreg` uses), not a private SPI call, and it needs no entitlement.
/// A missing or unreadable key is therefore [`SystemGraphics::Unknown`] rather
/// than a hard "no": a macOS that renames it must degrade to the levels this
/// call supplements, never wedge the gate shut.
///
/// It is the only reading that separates a DarkWake from a full wake. `pmset`'s
/// own log draws the same line — a `DarkWake` line carries `[CDNP]` where a
/// `FullWake` carries `[CDNVA]`, the `V` being video.
fn system_graphics() -> SystemGraphics {
    // SAFETY: the class name is a NUL-terminated C string literal; the
    // dictionary comes back owned, and `IOServiceGetMatchingService` consumes
    // exactly the one reference passed to it.
    let Some(matching) = (unsafe { IOServiceMatching(c"IOPMrootDomain".as_ptr()) }) else {
        return SystemGraphics::Unknown;
    };
    // SAFETY: `CFMutableDictionary` is a `CFDictionary` subclass, which is the
    // type IOKit's matching API is declared against.
    let matching = unsafe { CFRetained::cast_unchecked(matching) };
    // SAFETY: `kIOMainPortDefault` is IOKit's process-lifetime default port
    // constant.
    let root = unsafe { IOServiceGetMatchingService(kIOMainPortDefault, Some(matching)) };
    if root == 0 {
        return SystemGraphics::Unknown;
    }
    let key = CFString::from_static_str("System Capabilities");
    // SAFETY: `root` is a live registry entry handle, the key is a CFString,
    // and the default allocator with no options is what the API documents.
    let value = unsafe { IORegistryEntryCreateCFProperty(root, Some(&key), None, 0) }
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64));
    IOObjectRelease(root);
    match value {
        Some(capabilities) => {
            if capabilities & i64::from(kIOPMSystemCapabilityGraphics) == 0 {
                SystemGraphics::Down
            } else {
                SystemGraphics::Up
            }
        }
        None => SystemGraphics::Unknown,
    }
}

/// Whether this GUI session currently owns the console.
///
/// `kCGSessionOnConsoleKey` (`<CoreGraphics/CGSession.h>`: "an indication of
/// whether the session is on a console") is the level whose edges
/// `NSWorkspaceSessionDidBecomeActive` / `…DidResignActive` announce, so a
/// fast-user-switched-away agent reads `false` here and never reconciles its
/// way back into another user's session. No session dictionary at all means no
/// Quartz GUI session, which is likewise not a state to resume into.
fn session_is_on_console() -> bool {
    let Some(session) = CGSessionCopyCurrentDictionary() else {
        return false;
    };
    // SAFETY: `CGSession.h` documents the session dictionary as a map from the
    // `kCGSession*Key` CFStrings to CoreFoundation values.
    let keyed = unsafe { session.cast_unchecked::<CFString, CFType>() };
    keyed
        .get(&CFString::from_static_str("kCGSSessionOnConsoleKey"))
        .as_deref()
        .and_then(CFType::downcast_ref::<CFBoolean>)
        .is_some_and(CFBoolean::value)
}

/// Whether every display this session drives reports itself asleep.
///
/// `CGDisplayIsAsleep` (`<CoreGraphics/CGDisplayConfiguration.h>`: "true if the
/// display is asleep (and is therefore not drawable)") is right about an
/// ordinary idle blank — a display that times out leaves the active list and
/// reports `true`. What it misses is a display **reconfiguration**: closing the
/// lid re-enumerates the external panel under a fresh `CGDirectDisplayID`, and
/// that new id reported `false` through every DarkWake of a 2.5 h clamshell
/// blank while `NSWorkspaceScreensDidSleep` had reported the transition
/// correctly (#952). Widening the read past `CGMainDisplayID` does not fix that
/// — with the lid shut the external panel is both the main and the only online
/// display — and there is nothing to read instead: on Apple Silicon
/// `IODisplayWrangler` carries no `IOPowerManagement` dictionary. That case is
/// what [`system_graphics`] covers.
///
/// So this is used in one direction only, as a fast definite "the user can see
/// nothing". An empty list (headless, or screen-shared) and a failed query both
/// prove nothing and read as `false`; the caller then has to find its proof in
/// the idle timer.
fn displays_report_asleep() -> bool {
    const MAX_DISPLAYS: u32 = 16;
    let mut displays: [CGDirectDisplayID; MAX_DISPLAYS as usize] = [0; MAX_DISPLAYS as usize];
    let mut count: u32 = 0;
    // SAFETY: the write is bounded by the capacity passed as `max_displays`,
    // and `count` reports how many entries CoreGraphics actually filled.
    let status =
        unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, displays.as_mut_ptr(), &raw mut count) };
    if status != CGError::Success {
        return false;
    }
    let count = usize::try_from(count).unwrap_or(0).min(displays.len());
    let active = &displays[..count];
    !active.is_empty() && active.iter().all(|&display| CGDisplayIsAsleep(display))
}

/// How long the HID system has been idle.
///
/// Reads the hardware event state rather than the combined session state, so
/// events another process synthesizes cannot stand in for a user being at the
/// machine.
fn seconds_since_last_input() -> Duration {
    // `kCGAnyInputEventType` is `(uint32_t)(~0)` in `CGEventTypes.h`; objc2
    // generates the `CGEventType` newtype but not that constant.
    const ANY_INPUT: CGEventType = CGEventType(u32::MAX);

    let seconds = CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::HIDSystemState,
        ANY_INPUT,
    );
    // A value CoreGraphics cannot express as a duration proves nothing, so it
    // reads as "idle forever" and leaves the gate closed.
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLogiAgentMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openOpenLogi:))]
        fn open_openlogi(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::Show);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::OpenSettings);
        }

        #[unsafe(method(openAbout:))]
        fn open_about(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::OpenAbout);
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::CheckForUpdates);
        }

        #[unsafe(method(quitOpenLogi:))]
        fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
            quit_agent();
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` initializes our freshly-allocated NSObject subclass and
        // returns it (the two-phase construction objc2's `define_class!` uses).
        unsafe { msg_send![super(this), init] }
    }
}

fn open_url(url: &str) {
    match opener::open(url) {
        Ok(()) => info!(url, "menu-bar — opening URL"),
        Err(e) => warn!(error = %e, url, "could not open URL from menu bar"),
    }
}

/// Route a GUI-directed [`DeeplinkCommand`] through the `openlogi://` scheme.
/// macOS launches the GUI (cold start) or hands the URL to the running app.
fn open_command(command: DeeplinkCommand) {
    open_url(&command.to_url());
}

/// Menu-bar Quit: take a running GUI with us, then hand process termination to
/// the lifecycle that owns firmware capture and the input hook.
///
/// Kept out of `define_class!` so the lint set actually sees the exit — clippy
/// does not look inside macro expansions.
fn quit_agent() -> ! {
    // Tell a *running* GUI to quit too, but don't let `open` cold-launch one
    // just to immediately quit it (it would flash a window — and on first run
    // the update-consent prompt — before exiting). The gate keeps the target
    // warm in the common case, so the blocking `.output()` (which guarantees
    // Apple-Event delivery) returns at once; a GUI that races to exit after the
    // check was quitting anyway.
    if gui_is_running() {
        let _ = std::process::Command::new("open")
            .arg(DeeplinkCommand::Quit.to_url())
            .output();
    }
    crate::overlay::evict_on_quit();
    info!("menu-bar Quit — requesting graceful agent shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0)
}

/// Whether an OpenLogi GUI process is currently running (prod or dev bundle).
/// Used to avoid cold-launching the GUI from the Quit handler just to quit it.
fn gui_is_running() -> bool {
    // Release and dev; the agent's own id is `brand::AGENT_ID`, so neither
    // matches the agent itself.
    let dev = brand::dev_id(brand::APP_ID);
    [brand::APP_ID, dev.as_str()].iter().any(|id| {
        let running =
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(id));
        !running.is_empty()
    })
}

/// Run the agent's AppKit main loop: an `Accessory` `NSApplication` (no Dock
/// icon) optionally hosting the menu-bar status item. Must be called on the
/// process's main thread; blocks for the agent's lifetime (the agent exits via
/// Quit).
///
/// `show_in_menu_bar` honors the user's preference: when `false`, the same
/// Accessory loop runs with no status item (the agent stays fully headless; the
/// tokio core still does all the work). The toggle takes effect on the agent's
/// next launch — a no-restart live toggle would need a main-thread hop from the
/// IPC reload path (deferred; it can't be verified headlessly).
/// `device_io_signal` closes the hardware gate while the display/session is
/// away and reopens it only once the user can be shown to be back — announced
/// by a wake notification, or, when one never arrives, proved from the
/// window-server levels by the reconciler this loop starts.
pub fn run_app_loop(
    show_in_menu_bar: bool,
    app_icon: AppIcon,
    device_io_signal: DeviceIoSignal,
    shutdown_tx: ShutdownRequestSender,
) -> ! {
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("agent AppKit loop not started off the main thread — exiting");
        let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
        shutdown::request_tray_quit(requests.as_ref(), 1);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let activity_target = install_activity_observer(device_io_signal);
    // Bind the status item (+ its target/menu) so they outlive `run()` — the
    // menu items only weakly reference the target. `None` when hidden.
    let _tray = show_in_menu_bar.then(|| install_status_item(mtm, app_icon));

    // AppKit documents that an app launched into an inactive session receives
    // `NSWorkspaceSessionDidResignActiveNotification` between its will- and
    // did-finish-launching notifications. Finish that lifecycle while STARTUP
    // still holds the hardware gate closed, then try to prove the display is on
    // before permitting the core's initial inventory scan.
    app.finishLaunching();
    let levels = read_levels();
    if activity_target.finish_startup(levels) == StartupDisplay::Unproven {
        info!(
            graphics = ?levels.graphics,
            displays_report_asleep = levels.displays_report_asleep,
            idle_secs = levels.idle.as_secs_f64(),
            "display state unproven at launch — device I/O stays paused until input or a screen wake proves it"
        );
    }
    // Only now, with the launch sequence's own attempt made, does the
    // reconciler start: it must never race `finish_startup` for the gate.
    activity_target.sources().start_reconciler();
    info!(show_in_menu_bar, "agent AppKit loop started");

    app.run();
    info!("agent AppKit loop ended — requesting graceful core shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0);
}

/// Observe display/session sleep and user-visible resume transitions. Generic
/// `NSWorkspaceDidWakeNotification` is deliberately not registered: macOS
/// emits it for maintenance DarkWake, where opening BLE HID is exactly what can
/// promote an otherwise invisible wake into a full display wake (#656).
///
/// These notifications are edges over window-server state, and the workspace
/// center guarantees neither delivery nor pairing: a screens-asleep or
/// session-inactive edge whose partner never arrives would otherwise pause
/// device I/O until the agent restarts, and a relaunched agent has no history
/// at all. `run_app_loop` therefore also starts
/// [`ActivitySources::start_reconciler`], which proves those levels back.
fn install_activity_observer(signal: DeviceIoSignal) -> Retained<ActivityTarget> {
    let target = ActivityTarget::new(signal);
    let workspace = NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let system_sleep = unsafe { NSWorkspaceWillSleepNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let screen_sleep = unsafe { NSWorkspaceScreensDidSleepNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let session_inactive = unsafe { NSWorkspaceSessionDidResignActiveNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let screen_wake = unsafe { NSWorkspaceScreensDidWakeNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let session_active = unsafe { NSWorkspaceSessionDidBecomeActiveNotification };
    // SAFETY: Every selector below has exactly one `NSNotification` argument,
    // and the caller retains the target for the AppKit loop's lifetime.
    unsafe {
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceWillSleep:),
            Some(system_sleep),
            Some(&workspace),
        );
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceScreensDidSleep:),
            Some(screen_sleep),
            Some(&workspace),
        );
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceSessionDidResignActive:),
            Some(session_inactive),
            Some(&workspace),
        );
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceScreensDidWake:),
            Some(screen_wake),
            Some(&workspace),
        );
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceSessionDidBecomeActive:),
            Some(session_active),
            Some(&workspace),
        );
    }
    target
}

/// Build and install the menu-bar status item, returning the objects that must
/// stay alive for the app's lifetime (the status item, the action target the
/// menu items weakly reference, and the menu itself).
fn install_status_item(
    mtm: MainThreadMarker,
    app_icon: AppIcon,
) -> (
    Retained<objc2_app_kit::NSStatusItem>,
    Retained<MenuTarget>,
    Retained<objc2_app_kit::NSMenu>,
) {
    let target = MenuTarget::new(mtm);
    let status_item = status_item::create_status_item();
    status_item::set_png_icon(&status_item, mtm, glyph(app_icon), "OpenLogi");
    TRAY.with_borrow_mut(|slot| {
        *slot = Some(TrayState {
            item: status_item.clone(),
            target: target.clone(),
        });
    });
    let menu = build_menu(mtm, &target);
    status_item.setMenu(Some(&menu));

    info!("menu-bar item installed");
    (status_item, target, menu)
}

/// Build the tray menu with the current locale's titles. The one constructor
/// for both the install and a [`relocalize`] rebuild, so the two cannot drift.
fn build_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<objc2_app_kit::NSMenu> {
    let menu = status_item::new_menu(mtm);

    let show = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.show_main_window"),
        sel!(openOpenLogi:),
        target,
        "m",
    );
    menu.addItem(&show);
    status_item::add_separator(&menu, mtm);

    let settings = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.settings_dialog"),
        sel!(openSettings:),
        target,
        ",",
    );
    menu.addItem(&settings);
    let about = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("about.about_openlogi"),
        sel!(openAbout:),
        target,
        "",
    );
    menu.addItem(&about);
    let updates = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("updates.check_for_updates_dialog"),
        sel!(checkForUpdates:),
        target,
        "u",
    );
    menu.addItem(&updates);
    status_item::add_separator(&menu, mtm);

    let quit = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.quit_openlogi"),
        sel!(quitOpenLogi:),
        target,
        "q",
    );
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("xmark.square"),
        Some(&NSString::from_str(&rust_i18n::t!("app.quit_openlogi"))),
    ) {
        image.setTemplate(true);
        quit.setImage(Some(&image));
    }
    menu.addItem(&quit);
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_hid::device_io_channel;

    // Both tests post to the process-wide NSWorkspace notification center.
    // Keep each observer's entire registration/posting/removal lifetime isolated
    // so one test's session-inactive event cannot suspend the other test's gate.
    static WORKSPACE_NOTIFICATIONS: Mutex<()> = Mutex::new(());

    /// The user is at the machine: on console, no display reports itself
    /// asleep, and input a moment ago.
    const PRESENT: ActivityLevels = ActivityLevels {
        on_console: true,
        displays_report_asleep: false,
        graphics: SystemGraphics::Up,
        idle: Duration::from_millis(200),
    };

    /// On console, but nothing has been touched for long enough that no level
    /// can rule display sleep out.
    const IDLE: ActivityLevels = ActivityLevels {
        on_console: true,
        displays_report_asleep: false,
        graphics: SystemGraphics::Up,
        idle: Duration::from_mins(30),
    };

    #[test]
    fn overlapping_suspend_sources_all_clear_before_device_io_resumes() {
        let _notifications = WORKSPACE_NOTIFICATIONS.lock().unwrap();
        let (signal, gate) = device_io_channel();
        let target = install_activity_observer(signal);
        assert_eq!(target.finish_startup(PRESENT), StartupDisplay::Awake);
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();

        // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
        let system_sleep = unsafe { NSWorkspaceWillSleepNotification };
        // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
        let screen_sleep = unsafe { NSWorkspaceScreensDidSleepNotification };
        // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
        let session_inactive = unsafe { NSWorkspaceSessionDidResignActiveNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(system_sleep, Some(&workspace)) };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(screen_sleep, Some(&workspace)) };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(session_inactive, Some(&workspace)) };
        assert!(!gate.allows_io());

        // `DidWake` is a maintenance/system wake and intentionally has no
        // observer, so posting it must leave the gate closed.
        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let darkwake = unsafe { NSWorkspaceDidWakeNotification };
        // SAFETY: `workspace` is live and notification delivery is synchronous.
        unsafe { center.postNotificationName_object(darkwake, Some(&workspace)) };
        assert!(!gate.allows_io());

        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let screen_wake = unsafe { NSWorkspaceScreensDidWakeNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(screen_wake, Some(&workspace)) };
        assert!(
            !gate.allows_io(),
            "screen wake must not override an inactive session",
        );

        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let session_active = unsafe { NSWorkspaceSessionDidBecomeActiveNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(session_active, Some(&workspace)) };
        assert!(gate.allows_io());

        // SAFETY: This is the same live target registered with `center` above.
        unsafe { center.removeObserver(&target) };
    }

    #[test]
    fn startup_stays_suspended_when_the_display_is_already_asleep() {
        let _notifications = WORKSPACE_NOTIFICATIONS.lock().unwrap();
        let (signal, gate) = device_io_channel();
        let target = install_activity_observer(signal);
        assert!(!gate.allows_io(), "startup must fail closed");

        assert_eq!(target.finish_startup(IDLE), StartupDisplay::Unproven);
        assert!(
            !gate.allows_io(),
            "an unproven display must retain the startup hold",
        );

        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        // A screen wake is direct proof of a live display, so it discharges the
        // startup hold no level read could — #952 relaunched the agent every few
        // minutes for the whole of display sleep, and each relaunch has to stay
        // paused until the display really comes back.
        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let screen_wake = unsafe { NSWorkspaceScreensDidWakeNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(screen_wake, Some(&workspace)) };
        assert!(gate.allows_io());

        // SAFETY: This is the same live target registered with `center` above.
        unsafe { center.removeObserver(&target) };
    }

    /// Every test below builds a bare [`ActivityTarget`] rather than calling
    /// [`install_activity_observer`], so it neither registers on the
    /// process-global workspace notification center nor is disturbed by what
    /// another test posts there.
    fn present_target() -> (Retained<ActivityTarget>, openlogi_hid::DeviceIoGate) {
        let (signal, gate) = device_io_channel();
        let target = ActivityTarget::new(signal);
        assert_eq!(target.finish_startup(PRESENT), StartupDisplay::Awake);
        assert!(gate.allows_io());
        (target, gate)
    }

    /// The reported failure: a screens-asleep edge arrives, its wake never
    /// does, and nothing else is ever delivered. Input that arrived *after* the
    /// suspension proves the display came back, because any HID input wakes a
    /// sleeping display.
    #[test]
    fn a_screen_sleep_whose_wake_never_arrives_is_reconciled_by_input_after_it() {
        let (target, gate) = present_target();
        let slept_at = Instant::now();
        target.suspend_from(SCREEN_SLEEP);
        assert!(!gate.allows_io());

        // Two seconds later, with the last input from before the display slept.
        let levels = ActivityLevels {
            idle: Duration::from_secs(10),
            ..PRESENT
        };
        target
            .sources()
            .discharge_at(levels, slept_at + Duration::from_secs(2));
        assert!(
            !gate.allows_io(),
            "input older than the suspension proves nothing",
        );

        // The user is working: input newer than the suspension itself.
        let levels = ActivityLevels {
            idle: Duration::from_millis(500),
            ..PRESENT
        };
        target
            .sources()
            .discharge_at(levels, slept_at + Duration::from_secs(2));
        assert!(gate.allows_io());
    }

    /// The trap an absolute idle floor falls into: a hot corner blanks the
    /// display a second after the last keystroke, so recent input is not proof
    /// that the display is on. Only input *after* the suspension is.
    #[test]
    fn a_forced_display_sleep_is_not_reconciled_by_input_that_preceded_it() {
        let (target, gate) = present_target();
        let slept_at = Instant::now();
        target.suspend_from(SCREEN_SLEEP);

        // Idle 6 s, suspension 5 s old: the input is one second older than the
        // display sleep — and well inside `DISPLAY_SLEEP_IDLE_FLOOR`.
        let levels = ActivityLevels {
            idle: Duration::from_secs(6),
            ..PRESENT
        };
        assert!(levels.idle < DISPLAY_SLEEP_IDLE_FLOOR);
        target
            .sources()
            .discharge_at(levels, slept_at + Duration::from_secs(5));
        assert!(
            !gate.allows_io(),
            "the idle floor must not discharge a display sleep that was reported",
        );
    }

    /// The same hazard on the session half: `SessionDidResignActive` without
    /// the `SessionDidBecomeActive` that should follow the unlock handoff. The
    /// console level is trustworthy in both directions, so it needs no input.
    #[test]
    fn a_session_resign_whose_activation_never_arrives_is_reconciled_from_the_console_level() {
        let (target, gate) = present_target();
        target.suspend_from(SESSION_INACTIVE);
        assert!(!gate.allows_io());

        let elsewhere = ActivityLevels {
            on_console: false,
            ..IDLE
        };
        target.sources().discharge(elsewhere);
        assert!(!gate.allows_io(), "another user still owns the console");

        // Back on console — and still idle, which must not matter here: no
        // display sleep was ever reported.
        target.sources().discharge(IDLE);
        assert!(
            gate.allows_io(),
            "an unpaired session-inactive edge must not outlive the console level that set it",
        );
    }

    /// #656 non-regression: the process runs during a maintenance DarkWake, so
    /// a level read must never be the thing that reopens the gate after a
    /// system sleep — however present the user looks.
    #[test]
    fn reconciliation_never_clears_a_system_sleep_suspension() {
        let (target, gate) = present_target();
        target.suspend_from(SYSTEM_SLEEP);
        target.suspend_from(SCREEN_SLEEP);

        assert!(!reconcilable(SYSTEM_SLEEP | SCREEN_SLEEP));
        target.sources().discharge(PRESENT);
        assert!(
            !gate.allows_io(),
            "only the paired wake notification may clear a system sleep",
        );

        // The ordinary, user-visible resume still works.
        target.resume_from(SYSTEM_SLEEP | SCREEN_SLEEP);
        assert!(gate.allows_io());
    }

    /// #952's relaunch loop: `CGDisplayIsAsleep` answered "awake" throughout a
    /// 2.5 h blank, so every relaunched agent resumed device I/O behind a dark
    /// panel. The hold now stays until something proves otherwise.
    #[test]
    fn an_unproven_startup_display_keeps_device_io_paused_until_input_proves_it() {
        let (signal, gate) = device_io_channel();
        let target = ActivityTarget::new(signal);
        assert!(!gate.allows_io(), "startup must fail closed");

        assert_eq!(target.finish_startup(IDLE), StartupDisplay::Unproven);
        assert!(!gate.allows_io());

        // Still nothing: a display that reports itself asleep is definite.
        let asleep = ActivityLevels {
            displays_report_asleep: true,
            ..PRESENT
        };
        target.sources().discharge(asleep);
        assert!(!gate.allows_io());

        // The user touches the machine, which is what wakes a sleeping display.
        target.sources().discharge(PRESENT);
        assert!(gate.allows_io());
    }

    #[test]
    fn an_unproven_startup_display_does_not_mask_a_second_suspend_source() {
        let (signal, gate) = device_io_channel();
        let target = ActivityTarget::new(signal);
        assert_eq!(target.finish_startup(IDLE), StartupDisplay::Unproven);

        target.suspend_from(SESSION_INACTIVE);
        target.resume_from(STARTUP);
        assert!(
            !gate.allows_io(),
            "an inactive session must outlive the startup hold",
        );

        target.resume_from(SESSION_INACTIVE);
        assert!(gate.allows_io());
    }

    #[test]
    fn only_input_newer_than_the_shortest_display_sleep_timeout_proves_a_launch_display() {
        let fresh = |idle| ActivityLevels { idle, ..PRESENT };
        let no_history = STARTUP;
        let irrelevant = Duration::ZERO;

        assert!(display_is_proven_awake(
            no_history,
            irrelevant,
            fresh(Duration::ZERO)
        ));
        assert!(display_is_proven_awake(
            no_history,
            irrelevant,
            fresh(Duration::from_secs(59))
        ));
        // `pmset displaysleep 1` is the shortest blank macOS allows, so idle
        // time at or past the floor can no longer rule display sleep out.
        assert!(!display_is_proven_awake(
            no_history,
            irrelevant,
            fresh(DISPLAY_SLEEP_IDLE_FLOOR)
        ));
        // #952's relaunches landed between ~1 and ~150 minutes into the blank.
        assert!(!display_is_proven_awake(
            no_history,
            irrelevant,
            fresh(Duration::from_mins(150))
        ));
        // An unreadable idle timer reads as "idle forever" — fail closed.
        assert!(!display_is_proven_awake(
            no_history,
            irrelevant,
            fresh(Duration::MAX)
        ));
    }

    #[test]
    fn a_display_that_reports_itself_asleep_is_never_proven_awake() {
        let asleep = ActivityLevels {
            displays_report_asleep: true,
            ..PRESENT
        };
        assert!(!display_is_proven_awake(STARTUP, Duration::ZERO, asleep));
        assert!(!display_is_proven_awake(
            SCREEN_SLEEP,
            Duration::from_secs(60),
            asleep
        ));
        assert_eq!(discharged_by(STARTUP, Duration::ZERO, asleep), 0);
    }

    /// The hole the levels above cannot see: a lid close puts the machine into
    /// a DarkWake seconds after the user was last at it, and the agent that
    /// relaunches there reads every window-server level as "the user is here" —
    /// on console, no display reporting itself asleep (the external panel
    /// re-enumerates under a fresh id and reports awake), and input well inside
    /// the idle floor. Only the capability set says otherwise.
    #[test]
    fn a_darkwake_proves_nothing_however_present_every_other_level_looks() {
        let darkwake = ActivityLevels {
            graphics: SystemGraphics::Down,
            idle: Duration::from_secs(27),
            ..PRESENT
        };
        assert!(darkwake.idle < DISPLAY_SLEEP_IDLE_FLOOR);

        for held in [
            STARTUP,
            SCREEN_SLEEP,
            SESSION_INACTIVE,
            STARTUP | SCREEN_SLEEP,
        ] {
            assert_eq!(
                discharged_by(held, Duration::from_secs(2), darkwake),
                0,
                "a DarkWake must discharge nothing",
            );
        }

        // The same launch in a full wake is the case the idle floor is for.
        let full_wake = ActivityLevels {
            graphics: SystemGraphics::Up,
            ..darkwake
        };
        assert_eq!(
            discharged_by(STARTUP, Duration::from_secs(2), full_wake),
            STARTUP,
        );
    }

    /// An unreadable capability set is not evidence of a DarkWake. If a future
    /// macOS drops the key, the gate has to keep working off the levels that
    /// remain rather than latching shut for the process's lifetime.
    #[test]
    fn an_unreadable_capability_set_neither_proves_nor_blocks_anything() {
        let unknown = |levels: ActivityLevels| ActivityLevels {
            graphics: SystemGraphics::Unknown,
            ..levels
        };

        assert_eq!(
            discharged_by(STARTUP, Duration::from_secs(2), unknown(PRESENT)),
            STARTUP,
            "recent input must still prove a launch display",
        );
        assert_eq!(
            discharged_by(STARTUP, Duration::from_secs(2), unknown(IDLE)),
            0,
            "and a long-idle launch must still prove nothing",
        );
    }

    #[test]
    fn nothing_is_discharged_while_another_user_owns_the_console() {
        let elsewhere = ActivityLevels {
            on_console: false,
            ..PRESENT
        };
        for held in [STARTUP, SCREEN_SLEEP, SESSION_INACTIVE] {
            assert_eq!(discharged_by(held, Duration::from_secs(60), elsewhere), 0);
        }
    }

    /// The startup path is the one that opened the gate 43 times in a lid-close
    /// relaunch loop, so pin it at the entry point rather than only at
    /// [`discharged_by`].
    #[test]
    fn a_launch_into_a_darkwake_keeps_the_startup_hold() {
        let (signal, gate) = device_io_channel();
        let target = ActivityTarget::new(signal);
        let darkwake = ActivityLevels {
            graphics: SystemGraphics::Down,
            idle: Duration::from_secs(27),
            ..PRESENT
        };

        assert_eq!(target.finish_startup(darkwake), StartupDisplay::Unproven);
        assert!(!gate.allows_io());

        // The full wake that follows is what lifts it.
        target.sources().discharge(PRESENT);
        assert!(gate.allows_io());
    }

    #[test]
    fn only_a_system_sleep_is_beyond_the_reach_of_a_level_read() {
        assert!(!reconcilable(0), "an open gate has nothing to reconcile");
        assert!(reconcilable(STARTUP));
        assert!(reconcilable(SCREEN_SLEEP));
        assert!(reconcilable(SESSION_INACTIVE));
        assert!(reconcilable(SCREEN_SLEEP | SESSION_INACTIVE));
        assert!(!reconcilable(SYSTEM_SLEEP));
        assert!(!reconcilable(SYSTEM_SLEEP | SCREEN_SLEEP));
        assert!(!reconcilable(SYSTEM_SLEEP | STARTUP));
    }

    #[test]
    fn suspend_sources_render_every_bit_for_the_log() {
        assert_eq!(Sources(0).to_string(), "none");
        assert_eq!(Sources(SCREEN_SLEEP).to_string(), "screens-asleep");
        assert_eq!(
            Sources(SYSTEM_SLEEP | SESSION_INACTIVE | STARTUP).to_string(),
            "system-sleep+session-inactive+startup",
        );
    }
}
