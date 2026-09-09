//! The macOS host-activity levels behind the device-I/O gate.
//!
//! The gate exists to keep Logitech HID access off while the Mac is asleep or
//! in a DarkWake: opening a Bluetooth HID device during a maintenance wake is
//! what promoted an invisible wake into a lit external display (#656). That is
//! a question about the *system's* power state, and it is answered here from
//! power management itself rather than from AppKit.
//!
//! `NSWorkspace` cannot carry it. Its sleep/wake notifications are edges, the
//! screen-wake edge is not guaranteed on a lid-close/open cycle (Apple DTS,
//! developer forums thread 796109), and `NSWorkspaceDidWakeNotification` fires
//! for a DarkWake as well — so an edge-latched gate had no "the DarkWake is
//! over" signal and wedged shut whenever a wake notification was dropped
//! (#1281).
//!
//! `IOPMConnection`, the powerd client API `pmset` is built on, reports every
//! Sleep / DarkWake / FullWake transition as the complete capability set of
//! the new state, delivered over Mach to a dispatch queue this process owns,
//! and reads the current set on demand. Every input to the gate is therefore a
//! level: nothing has to pair, a missed event is corrected by the next, and no
//! timer is needed. Console ownership (fast user switching) is the other
//! level; its AppKit edges are forwarded from `tray`, and it is re-read from
//! CoreGraphics on every power transition.
//!
//! The API is exported by IOKit since 10.6 but declared only in Apple's
//! open-source `IOKitUser/pwr_mgt.subproj/IOPMLibPrivate.h`, hence the
//! hand-written `extern` block (`.claude/rules/objc-ffi.md`). When the
//! connection cannot be opened the gate follows the console alone and says so
//! at error level: a Mac without the SPI gets pre-0.8.2 behaviour — hardware
//! usable, #656 unprotected — not an agent that can never touch hardware.

#![expect(
    unsafe_code,
    reason = "the IOPMConnection SPI has no bindings; its calls, C callback, and the CGSession dictionary cast live here"
)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex, PoisonError};

use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2_core_foundation::{CFBoolean, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;
use objc2_io_kit::{IOReturn, kIOReturnSuccess};
use openlogi_hid::DeviceIoSignal;
use tracing::{debug, error, info, warn};

/// The system power states that matter to hardware access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerState {
    /// The CPU is off in this state: sleep or hibernation.
    Sleep,
    /// The CPU runs with graphics down: a maintenance or notification wake
    /// the user cannot see. HID activity here is what #656 is about.
    DarkWake,
    /// Graphics are up: a wake the user can see.
    FullWake,
}

impl PowerState {
    /// Classify an `IOPMCapabilityBits` set.
    ///
    /// A clear CPU bit is a sleep state and makes the other bits meaningless;
    /// with it set, the video bit is what separates a DarkWake from the full
    /// wake `IOPMIsAUserWake` tests for.
    fn from_capabilities(capabilities: ffi::CapabilityBits) -> Self {
        if capabilities & ffi::CAPABILITY_CPU == 0 {
            Self::Sleep
        } else if capabilities & ffi::CAPABILITY_VIDEO == 0 {
            Self::DarkWake
        } else {
            Self::FullWake
        }
    }
}

/// Everything the gate decides from. Levels only — never an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Levels {
    power: PowerState,
    /// This login session owns the console; fast user switching moves it.
    on_console: bool,
    /// The launch hold, released once both levels above have been read.
    starting: bool,
}

impl Levels {
    fn allows_io(self) -> bool {
        !self.starting && self.power == PowerState::FullWake && self.on_console
    }
}

/// The one transition authority for the device-I/O gate on macOS.
///
/// Every writer — the powerd callback, the AppKit session observer, the launch
/// sequence — sets a level here, and only the resulting [`Levels::allows_io`]
/// moves the published [`DeviceIoSignal`], so the signal can never drift from
/// the levels.
pub struct ActivityGate {
    signal: DeviceIoSignal,
    levels: Mutex<Levels>,
}

impl ActivityGate {
    /// Close `signal` and hold it closed until [`Self::finish_startup`].
    pub fn new(signal: DeviceIoSignal) -> Arc<Self> {
        // `main` closes the gate before spawning the core thread; repeating
        // the idempotent close keeps the launch hold self-contained.
        let _ = signal.suspend();
        Arc::new(Self {
            signal,
            levels: Mutex::new(Levels {
                // Unread until `connect` seeds it; the launch hold covers the
                // gap, and a launch into a DarkWake must not guess "awake".
                power: PowerState::Sleep,
                on_console: false,
                starting: true,
            }),
        })
    }

    pub fn set_power(&self, power: PowerState) {
        self.update(|levels| levels.power = power);
    }

    pub fn set_on_console(&self, on_console: bool) {
        self.update(|levels| levels.on_console = on_console);
    }

    /// Release the launch hold. What the gate does next follows from the
    /// levels alone: a launch into a DarkWake stays closed until powerd
    /// reports the full wake.
    pub fn finish_startup(&self) {
        self.update(|levels| levels.starting = false);
    }

    fn update(&self, change: impl FnOnce(&mut Levels)) {
        let mut levels = self.levels.lock().unwrap_or_else(PoisonError::into_inner);
        let before = *levels;
        change(&mut levels);
        let after = *levels;
        // Published — and logged — under the lock, so two writers cannot
        // reorder each other's transitions.
        if after.allows_io() {
            if self.signal.resume() {
                info!(power = ?after.power, on_console = after.on_console, "device I/O resumed");
            }
        } else if self.signal.suspend() {
            info!(
                power = ?after.power,
                on_console = after.on_console,
                starting = after.starting,
                "device I/O paused"
            );
        } else if before != after {
            debug!(
                ?before,
                ?after,
                "host activity level changed; device I/O stays paused"
            );
        }
    }
}

/// Whether this login session currently owns the console.
///
/// `kCGSessionOnConsoleKey` (`CGSession.h`) is the level whose edges
/// `NSWorkspaceSessionDidBecomeActive` / `…DidResignActive` announce; the
/// header defines it as `CFSTR("kCGSSessionOnConsoleKey")`, hence the literal.
/// No session dictionary at all means no GUI session, which is not a state to
/// resume into either.
#[must_use]
pub fn session_is_on_console() -> bool {
    let Some(session) = CGSessionCopyCurrentDictionary() else {
        return false;
    };
    // SAFETY: `CGSession.h` documents the dictionary as keyed by the
    // `kCGSession*Key` CFStrings with CoreFoundation values.
    let session = unsafe { session.cast_unchecked::<CFString, CFType>() };
    session
        .get(&CFString::from_static_str("kCGSSessionOnConsoleKey"))
        .as_deref()
        .and_then(CFType::downcast_ref::<CFBoolean>)
        .is_some_and(CFBoolean::value)
}

/// A live powerd subscription.
///
/// `tray::run_app_loop` binds it next to the AppKit loop it never returns
/// from. The gate it reports to is kept alive by the `Arc` held here, and
/// [`Drop`] ends delivery before that `Arc` can go, which is what makes the
/// callback's raw `param` sound.
pub struct PowerConnection {
    _gate: Arc<ActivityGate>,
    queue: DispatchRetained<DispatchQueue>,
    connection: ffi::Connection,
}

/// Subscribe `gate` to powerd's Sleep / DarkWake / FullWake transitions and
/// seed it with the current state.
///
/// `None` means the SPI could not be reached. The gate is then told the
/// system is in a permanent full wake and follows the console alone — the
/// fail-open degradation the module docs describe.
pub fn connect(gate: &Arc<ActivityGate>) -> Option<PowerConnection> {
    match PowerConnection::open(Arc::clone(gate)) {
        Ok(connection) => Some(connection),
        Err(status) => {
            error!(
                status,
                "could not subscribe to power management — device I/O will follow the login session only, and a DarkWake will not pause it"
            );
            gate.set_power(PowerState::FullWake);
            None
        }
    }
}

impl PowerConnection {
    fn open(gate: Arc<ActivityGate>) -> Result<Self, IOReturn> {
        let mut connection: ffi::Connection = std::ptr::null();
        let name = CFString::from_static_str("openlogi-agent");
        // SAFETY: `name` is a live CFString and `connection` a valid
        // out-pointer; powerd copies the name for its own logging.
        let status = unsafe {
            ffi::IOPMConnectionCreate(&name, ffi::SLEEP_WAKE_INTEREST, &raw mut connection)
        };
        if status != kIOReturnSuccess || connection.is_null() {
            return Err(status);
        }
        let param = Arc::as_ptr(&gate).cast_mut().cast::<c_void>();
        // SAFETY: `connection` came from a successful create, `on_event` has
        // the handler's exact C signature, and `param` points at the gate the
        // returned `PowerConnection` keeps alive.
        let status = unsafe { ffi::IOPMConnectionSetNotification(connection, param, on_event) };
        if status != kIOReturnSuccess {
            // SAFETY: releasing the connection this function created and has
            // not yet scheduled for delivery.
            unsafe { ffi::IOPMConnectionRelease(connection) };
            return Err(status);
        }
        // Seed the level before delivery starts, so the first event can only
        // move forward from a read state rather than from the launch default.
        // SAFETY: a plain read of powerd's current capability set.
        let capabilities = unsafe { ffi::IOPMConnectionGetSystemCapabilities() };
        gate.set_power(PowerState::from_capabilities(capabilities));
        let queue = DispatchQueue::new("org.openlogi.agent.power", DispatchQueueAttr::SERIAL);
        // SAFETY: both handles are live; the queue is retained by the
        // returned value for as long as the connection can deliver to it.
        unsafe { ffi::IOPMConnectionSetDispatchQueue(connection, &queue) };
        Ok(Self {
            _gate: gate,
            queue,
            connection,
        })
    }
}

impl Drop for PowerConnection {
    fn drop(&mut self) {
        // SAFETY: the connection was created by `open` and is released
        // exactly once, here.
        let status = unsafe { ffi::IOPMConnectionRelease(self.connection) };
        if status != kIOReturnSuccess {
            warn!(
                status,
                "could not release the power management subscription"
            );
        }
        // A transition already handed to the queue may still be running with
        // a pointer to the gate; let the serial queue drain before the `Arc`
        // behind `param` is dropped with the rest of `self`.
        self.queue.exec_sync(|| {});
    }
}

/// powerd's transition callback, on the connection's dispatch queue.
unsafe extern "C" fn on_event(
    param: *mut c_void,
    connection: ffi::Connection,
    token: ffi::MessageToken,
    capabilities: ffi::CapabilityBits,
) {
    // SAFETY: `param` is the `ActivityGate` behind the `Arc` the
    // `PowerConnection` holds, alive for the connection's lifetime.
    let gate = unsafe { &*param.cast::<ActivityGate>() };
    let power = PowerState::from_capabilities(capabilities);
    debug!(
        capabilities = format_args!("{capabilities:#x}"),
        ?power,
        "power transition"
    );
    gate.set_power(power);
    // The console can move while the machine is dark; the wake is the moment
    // a dropped session notification would otherwise start to matter.
    gate.set_on_console(session_is_on_console());
    // SAFETY: `token` identifies this event on `connection`, both handed to
    // this callback by powerd.
    let status = unsafe { ffi::IOPMConnectionAcknowledgeEvent(connection, token) };
    if status != kIOReturnSuccess {
        warn!(
            status,
            "could not acknowledge the power transition — power management continues after its timeout"
        );
    }
}

/// The `IOPMConnection` SPI, transcribed from
/// `IOKitUser/pwr_mgt.subproj/IOPMLibPrivate.h` (IOKitUser-100231).
mod ffi {
    use std::ffi::c_void;

    use dispatch2::DispatchQueue;
    use objc2_core_foundation::CFString;
    use objc2_io_kit::IOReturn;

    /// `IOPMConnection`: an opaque, powerd-owned handle.
    #[repr(C)]
    pub struct OpaqueConnection {
        _private: [u8; 0],
    }
    pub type Connection = *const OpaqueConnection;
    /// `IOPMCapabilityBits`.
    pub type CapabilityBits = u32;
    /// `IOPMConnectionMessageToken`.
    pub type MessageToken = u32;
    /// `IOPMEventHandlerType`.
    pub type EventHandler =
        unsafe extern "C" fn(*mut c_void, Connection, MessageToken, CapabilityBits);

    /// `kIOPMCapabilityCPU`: clear in every sleep state.
    pub const CAPABILITY_CPU: CapabilityBits = 0x1;
    /// `kIOPMCapabilityVideo`: "graphic output to displays are supported".
    pub const CAPABILITY_VIDEO: CapabilityBits = 0x2;
    const CAPABILITY_AUDIO: CapabilityBits = 0x4;
    const CAPABILITY_NETWORK: CapabilityBits = 0x8;
    const CAPABILITY_DISK: CapabilityBits = 0x10;
    /// `kIOPMSleepWakeInterest`: "notifications for Sleep, FullWake, and
    /// DarkWake system events".
    pub const SLEEP_WAKE_INTEREST: CapabilityBits =
        CAPABILITY_CPU | CAPABILITY_DISK | CAPABILITY_NETWORK | CAPABILITY_VIDEO | CAPABILITY_AUDIO;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOPMConnectionCreate(
            name: &CFString,
            interests: CapabilityBits,
            connection: *mut Connection,
        ) -> IOReturn;
        pub fn IOPMConnectionSetNotification(
            connection: Connection,
            param: *mut c_void,
            handler: EventHandler,
        ) -> IOReturn;
        pub fn IOPMConnectionSetDispatchQueue(connection: Connection, queue: &DispatchQueue);
        pub fn IOPMConnectionAcknowledgeEvent(
            connection: Connection,
            token: MessageToken,
        ) -> IOReturn;
        pub fn IOPMConnectionRelease(connection: Connection) -> IOReturn;
        pub fn IOPMConnectionGetSystemCapabilities() -> CapabilityBits;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_hid::device_io_channel;

    /// `pmset -g log` renders these as `[CDNP]` (DarkWake) and `[CDNVA]`
    /// (FullWake); the letters are the same bits.
    const DARK_WAKE: ffi::CapabilityBits = 0x19;
    const FULL_WAKE: ffi::CapabilityBits = 0x1f;

    #[test]
    fn capability_bits_classify_by_cpu_then_video() {
        assert_eq!(PowerState::from_capabilities(0), PowerState::Sleep);
        assert_eq!(
            PowerState::from_capabilities(DARK_WAKE),
            PowerState::DarkWake
        );
        assert_eq!(
            PowerState::from_capabilities(FULL_WAKE),
            PowerState::FullWake
        );
        // Video without CPU is not a state powerd reports; read it as the
        // sleep the CPU bit says it is.
        assert_eq!(
            PowerState::from_capabilities(ffi::CAPABILITY_VIDEO),
            PowerState::Sleep
        );
    }

    fn awake_on_console() -> (Arc<ActivityGate>, openlogi_hid::DeviceIoGate) {
        let (signal, io) = device_io_channel();
        let gate = ActivityGate::new(signal);
        gate.set_power(PowerState::FullWake);
        gate.set_on_console(true);
        gate.finish_startup();
        assert!(io.allows_io());
        (gate, io)
    }

    #[test]
    fn the_launch_hold_outlasts_favourable_levels() {
        let (signal, io) = device_io_channel();
        let gate = ActivityGate::new(signal);
        assert!(!io.allows_io(), "startup must fail closed");
        gate.set_power(PowerState::FullWake);
        gate.set_on_console(true);
        assert!(!io.allows_io(), "levels alone must not release the hold");
        gate.finish_startup();
        assert!(io.allows_io());
    }

    #[test]
    fn a_launch_into_a_dark_wake_stays_closed_until_the_full_wake() {
        // The relaunch loop of #952: a watchdog restart during the sleep
        // transition used to guess "awake" and probe HID behind a dark panel.
        let (signal, io) = device_io_channel();
        let gate = ActivityGate::new(signal);
        gate.set_power(PowerState::DarkWake);
        gate.set_on_console(true);
        gate.finish_startup();
        assert!(!io.allows_io());
        gate.set_power(PowerState::FullWake);
        assert!(io.allows_io());
    }

    #[test]
    fn a_dark_wake_blip_reopens_on_the_full_wake_alone() {
        // #1281: a one-second DarkWake at unlock, promoted back to a full wake
        // by HID activity, with no screen-wake notification ever delivered.
        let (gate, io) = awake_on_console();
        gate.set_power(PowerState::DarkWake);
        assert!(!io.allows_io());
        gate.set_power(PowerState::FullWake);
        assert!(io.allows_io());
    }

    #[test]
    fn sleep_closes_and_only_a_full_wake_reopens() {
        let (gate, io) = awake_on_console();
        gate.set_power(PowerState::Sleep);
        assert!(!io.allows_io());
        gate.set_power(PowerState::DarkWake);
        assert!(!io.allows_io(), "a maintenance wake is not a resume (#656)");
        gate.set_power(PowerState::FullWake);
        assert!(io.allows_io());
    }

    #[test]
    fn another_users_console_holds_the_gate_through_a_full_wake() {
        let (gate, io) = awake_on_console();
        gate.set_on_console(false);
        assert!(!io.allows_io());
        gate.set_power(PowerState::Sleep);
        gate.set_power(PowerState::FullWake);
        assert!(
            !io.allows_io(),
            "a wake into another user's session must not resume"
        );
        gate.set_on_console(true);
        assert!(io.allows_io());
    }

    #[test]
    fn the_spi_reports_a_running_cpu_and_accepts_a_subscription() {
        // Resolves the hand-declared symbols against the host's IOKit and
        // pins the one fact any running process can assert about itself.
        // SAFETY: a plain read of powerd's current capability set.
        let capabilities = unsafe { ffi::IOPMConnectionGetSystemCapabilities() };
        assert_ne!(
            capabilities & ffi::CAPABILITY_CPU,
            0,
            "a process reading its own capability set is not asleep"
        );
        let (signal, _io) = device_io_channel();
        let gate = ActivityGate::new(signal);
        assert!(
            PowerConnection::open(gate).is_ok(),
            "powerd must accept an unprivileged subscription"
        );
    }
}
