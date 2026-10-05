//! DockSwipe injection with read-only, per-display Space confirmation.
//!
//! Private event format follows yabai's SIP-enabled implementation:
//! <https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/space_manager.c#L927-L981>
//! No Dock injection, cursor warp, focus click, or symbolic-hotkey mutation.

use std::ffi::{c_int, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Once, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFUUID,
};
use objc2_core_graphics::{
    CGError, CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGGetDisplaysWithPoint,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};

use super::super::SpaceSwipePhase;
use super::super::space_switch::{self, Backend, Direction, Failure, Lease, PostGate, SpaceState};
use super::{app_services, app_services_symbol};

static BUSY: AtomicBool = AtomicBool::new(false);

pub(super) fn previous_desktop() {
    start(Direction::Previous);
}
pub(super) fn next_desktop() {
    start(Direction::Next);
}

fn start(direction: Direction) {
    let Some(lease) = Lease::acquire(&BUSY) else {
        tracing::debug!(?direction, "Space switch already pending — skipped");
        return;
    };
    // Capture the display before scheduling. A moved cursor must not retarget
    // a delayed action to another monitor; `post` checks it again.
    let Some(display_id) = cursor_display() else {
        tracing::warn!(?direction, "Space switch: cursor display unavailable");
        return;
    };
    let result = space_switch::spawn_ordered(move |gate| {
        let _lease = lease;
        autoreleasepool(|_| {
            let result = Native::new(display_id)
                .and_then(|mut native| space_switch::run(&mut native, direction, gate));
            match result {
                Ok(outcome) => {
                    tracing::debug!(display_id, ?direction, ?outcome, "Space switch result");
                }
                Err(error) => tracing::warn!(
                    display_id,
                    ?direction,
                    ?error,
                    "Space switch unconfirmed — no retry"
                ),
            }
        });
    });
    if let Err(error) = result {
        tracing::warn!(?error, "Space switch preparation failed — no retry");
    }
}

/// The SkyLight display identifier of `display`.
fn display_uuid(api: &Api, display: u32) -> Option<String> {
    // SAFETY: dynamically resolved CGDisplayCreateUUIDFromDisplayID accepts
    // a public CGDirectDisplayID and returns a Create-rule CFUUID or null.
    let uuid = NonNull::new(unsafe { (api.display_uuid)(display) })?;
    // SAFETY: adopt the Create-rule result once; release via CFRetained.
    let uuid = unsafe { CFRetained::from_raw(uuid) };
    Some(CFUUID::new_string(None, Some(&uuid))?.to_string())
}

/// Whether the display under the cursor has a Space before and after the
/// current one, read-only; `None` when the Space list is unavailable.
pub(in crate::inject) fn space_neighbors() -> Option<(bool, bool)> {
    let api = API.as_ref()?;
    let uuid = display_uuid(api, cursor_display()?)?;
    let state = api.state(&uuid)?;
    let previous = state.target(Direction::Previous).ok()?.is_some();
    let next = state.target(Direction::Next).ok()?.is_some();
    Some((previous, next))
}

static SPACE_CHANGES: AtomicU64 = AtomicU64::new(0);
static SPACE_CHANGE_WATCH: Once = Once::new();

/// A count of active-Space changes since the first call, which subscribes to
/// them for the rest of the process.
pub(in crate::inject) fn space_change_count() -> u64 {
    SPACE_CHANGE_WATCH.call_once(|| {
        let block: RcBlock<dyn Fn(NonNull<NSNotification>)> = RcBlock::new(|_| {
            SPACE_CHANGES.fetch_add(1, Ordering::Relaxed);
        });
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        // SAFETY: the notification name and object are valid AppKit objects;
        // the block is copied by AppKit and only touches a static atomic.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                Some(&workspace),
                None,
                &block,
            )
        };
        // Observed for the life of the process.
        std::mem::forget(token);
    });
    SPACE_CHANGES.load(Ordering::Relaxed)
}

fn cursor_display() -> Option<u32> {
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    let mut displays = [0; 2];
    let mut count = 0;
    // SAFETY: the two-element buffer and count are valid writable outputs.
    let error = unsafe { CGGetDisplaysWithPoint(point, 2, displays.as_mut_ptr(), &raw mut count) };
    // Ambiguous mirrored displays fail closed rather than guessing a target.
    (error == CGError::Success && count == 1 && displays[0] != 0).then_some(displays[0])
}

struct Observer {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Observer {
    fn new() -> (Self, mpsc::Receiver<()>) {
        let (send, receive) = mpsc::sync_channel(1);
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        let block: RcBlock<dyn Fn(NonNull<NSNotification>)> = RcBlock::new(move |_| {
            // Deliberately coalesce wakeups; the authoritative state is queried
            // after every wake. No locks, blocking, or panics in this callback.
            let _ = send.try_send(());
        });
        // SAFETY: immutable AppKit notification name, exact block ABI, and a
        // Send + Sync sender. A nil queue invokes the block on the posting thread.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                Some(&workspace),
                None,
                &block,
            )
        };
        (Self { center, token }, receive)
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: this token belongs to this center and is removed exactly once.
        unsafe { self.center.removeObserver(self.token.as_ref()) };
    }
}

struct Native {
    api: &'static Api,
    display: u32,
    uuid: String,
    started: Instant,
    changed: mpsc::Receiver<()>,
    _observer: Observer,
}

impl Native {
    fn new(display: u32) -> Result<Self, Failure> {
        let api = API.as_ref().ok_or(Failure::Unavailable)?;
        let uuid = display_uuid(api, display).ok_or(Failure::Unavailable)?;
        // Subscribe before the first state query and before any output event.
        let (observer, changed) = Observer::new();
        Ok(Self {
            api,
            display,
            uuid,
            started: Instant::now(),
            changed,
            _observer: observer,
        })
    }
}

impl Backend for Native {
    fn state(&mut self) -> Result<SpaceState, Failure> {
        // Drain only an old wakeup BEFORE querying. A notification during or
        // after the query stays queued, including between query and recv_timeout.
        let _ = self.changed.try_recv();
        let state = self.api.state(&self.uuid).ok_or(Failure::Unavailable)?;
        tracing::debug!(display_id = self.display, current = state.current, spaces = ?state.ordered,
            "Space state observed");
        Ok(state)
    }

    fn post(&mut self, direction: Direction, gate: PostGate) -> Result<(), Failure> {
        let events = swipe_events(direction).ok_or(Failure::PostFailed)?;
        if cursor_display() != Some(self.display) {
            return Err(Failure::ContextChanged);
        }
        gate.commit(|| {
            for event in &events {
                CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
            }
        })
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn wait_for_change(&mut self, remaining: Duration) {
        // Deadline wakeup is cleanup/reconciliation, never evidence of success.
        let _ = self.changed.recv_timeout(remaining);
    }
}

fn swipe_events(direction: Direction) -> Option<[CFRetained<CGEvent>; 2]> {
    let make = |phase| {
        let event = CGEvent::new(None)?;
        // Establish the private DockControl type before setting its fields,
        // as required by CGEventSetIntegerValueField's API contract. This is
        // type 30 (also private field 55), not the generic Gesture type 29.
        CGEvent::set_type(Some(&event), CGEventType(30));
        // DockSwipe HID kind, horizontal motion, and balanced gesture phases.
        for (field, value) in [(110, 23), (123, 1), (132, phase)] {
            CGEvent::set_integer_value_field(Some(&event), CGEventField(field), value);
        }
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::EventSourceUserData,
            super::super::SYNTHETIC_EVENT_USER_DATA,
        );
        CGEvent::set_double_value_field(Some(&event), CGEventField(124), direction.sign());
        CGEvent::set_double_value_field(Some(&event), CGEventField(129), direction.sign() * 9999.0);
        Some(event)
    };
    // Prepare both phases before posting either: allocation failure cannot
    // leave an unterminated gesture. Send exactly one adjacent-Space swipe.
    Some([make(1)?, make(4)?])
}

/// Live Dock swipes use the type-30 field layout verified interactively
/// through macOS 26 (OpenLogi PR #1389, Mac Mouse Fix). Later releases keep
/// the one-shot switch until a live layout is verified there.
static LIVE_SWIPE_SUPPORTED: LazyLock<bool> = LazyLock::new(|| {
    objc2_foundation::NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 27
});

/// Post one frame of a live Space transition; see
/// [`crate::post_space_swipe`].
pub(in crate::inject) fn post_space_swipe(progress: f64, phase: SpaceSwipePhase) -> bool {
    if !*LIVE_SWIPE_SUPPORTED {
        return false;
    }
    let Some(event) = live_swipe_event(progress, phase) else {
        tracing::warn!(?phase, "could not build a live Space swipe event");
        return false;
    };
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
    true
}

fn live_swipe_event(progress: f64, phase: SpaceSwipePhase) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(None)?;
    CGEvent::set_type(Some(&event), CGEventType(30));
    let phase = live_swipe_phase(phase);
    // Dock swipe subtype, horizontal motion, and the phase in both slots the
    // trackpad path fills.
    for (field, value) in [(110, 23), (123, 1), (165, 1), (132, phase), (134, phase)] {
        CGEvent::set_integer_value_field(Some(&event), CGEventField(field), value);
    }
    CGEvent::set_double_value_field(Some(&event), CGEventField(41), 33_231.0);
    CGEvent::set_double_value_field(Some(&event), CGEventField(124), progress);
    // The Dock also reads the progress as raw IEEE-754 f32 bits.
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField(135),
        i64::from(live_swipe_progress_bits(progress)),
    );
    let horizontal = f64::from(f32::from_bits(1));
    CGEvent::set_double_value_field(Some(&event), CGEventField(119), horizontal);
    CGEvent::set_double_value_field(Some(&event), CGEventField(139), horizontal);
    CGEvent::set_integer_value_field(Some(&event), CGEventField(136), 1);
    if phase == live_swipe_phase(SpaceSwipePhase::Ended) {
        // An emphatic exit velocity toward the progress sign asks the Dock to
        // finish the switch rather than spring back, as the one-shot path does.
        let velocity = progress.signum() * 9999.0;
        CGEvent::set_double_value_field(Some(&event), CGEventField(129), velocity);
        CGEvent::set_double_value_field(Some(&event), CGEventField(130), velocity);
    }
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::EventSourceUserData,
        super::super::SYNTHETIC_EVENT_USER_DATA,
    );
    Some(event)
}

const fn live_swipe_phase(phase: SpaceSwipePhase) -> i64 {
    match phase {
        SpaceSwipePhase::Began => 1,
        SpaceSwipePhase::Changed => 2,
        SpaceSwipePhase::Ended => 4,
        SpaceSwipePhase::Cancelled => 8,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the Dock swipe field stores its progress as IEEE-754 f32 bits"
)]
fn live_swipe_progress_bits(progress: f64) -> u32 {
    (progress as f32).to_bits()
}

type Dictionary = CFDictionary<CFString, CFType>;
type MainConnection = unsafe extern "C" fn() -> c_int;
type CopySpaces = unsafe extern "C" fn(c_int) -> *mut CFArray;
type CurrentSpace = unsafe extern "C" fn(c_int, *const CFString) -> u64;
type DisplayUuid = unsafe extern "C" fn(u32) -> *mut CFUUID;

struct Api {
    connection: MainConnection,
    spaces: CopySpaces,
    current: CurrentSpace,
    display_uuid: DisplayUuid,
}

static API: LazyLock<Option<Api>> = LazyLock::new(|| {
    let connection = app_services::sky_light_symbol(c"SLSMainConnectionID")?;
    let spaces = app_services::sky_light_symbol(c"SLSCopyManagedDisplaySpaces")?;
    let current = app_services::sky_light_symbol(c"SLSManagedDisplayGetCurrentSpace")?;
    let display_uuid = app_services_symbol(c"CGDisplayCreateUUIDFromDisplayID")?;
    // SAFETY: the resolved SPI has the int(void) C ABI.
    let connection = unsafe { std::mem::transmute::<*mut c_void, MainConnection>(connection) };
    // SAFETY: the resolved SPI has the CFArrayRef(int) C ABI and Copy ownership.
    let spaces = unsafe { std::mem::transmute::<*mut c_void, CopySpaces>(spaces) };
    // SAFETY: the resolved SPI has the uint64_t(int, CFStringRef) C ABI.
    let current = unsafe { std::mem::transmute::<*mut c_void, CurrentSpace>(current) };
    // SAFETY: the resolved SPI has the CFUUIDRef(uint32_t) C ABI and Create ownership.
    let display_uuid = unsafe { std::mem::transmute::<*mut c_void, DisplayUuid>(display_uuid) };
    Some(Api {
        connection,
        spaces,
        current,
        display_uuid,
    })
});

impl Api {
    fn state(&self, uuid: &str) -> Option<SpaceState> {
        // SAFETY: resolved read-only SPI with no arguments.
        let connection = unsafe { (self.connection)() };
        if connection == 0 {
            return None;
        }
        // SAFETY: connection belongs to this process; Copy result is owned or null.
        let spaces = NonNull::new(unsafe { (self.spaces)(connection) })?;
        // SAFETY: adopt the Copy-rule result once.
        let spaces = unsafe { CFRetained::from_raw(spaces) };
        // SAFETY: SLSCopyManagedDisplaySpaces returns a CF-object array; each
        // element and dictionary value is runtime-downcast before use.
        let displays = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(spaces) };
        // With separate Spaces disabled, WindowServer returns a single "Main"
        // record. Use that record, never an arbitrary other monitor's UUID.
        let single_display = displays.len() == 1;
        let mut selected = None;
        for value in displays {
            let display = dictionary(value)?;
            let identifier = get(&display, "Display Identifier")?
                .downcast::<CFString>()
                .ok()?;
            let name = identifier.to_string();
            if name != uuid && !(single_display && name == "Main") {
                continue;
            }
            if selected.is_some() {
                return None;
            }
            let spaces = array(get(&display, "Spaces")?)?;
            let ordered = spaces
                .into_iter()
                .map(|space| {
                    let space = dictionary(space)?;
                    // Unknown/system-only Space kinds must not be skipped: skipping
                    // would turn an adjacent gesture into a wrong-target request.
                    if !matches!(number(&space, "type")?, 0 | 4) {
                        return None;
                    }
                    number(&space, "id64")
                })
                .collect::<Option<Vec<_>>>()?;
            // SAFETY: identifier is a live CFString returned for this connection.
            let current = unsafe { (self.current)(connection, &raw const *identifier) };
            selected = Some(SpaceState {
                display: name,
                current,
                ordered,
            });
        }
        selected
    }
}

fn dictionary(value: CFRetained<CFType>) -> Option<CFRetained<Dictionary>> {
    let value = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: managed-display/Space dictionaries use CFString keys and CF values.
    Some(unsafe { CFRetained::cast_unchecked::<Dictionary>(value) })
}

fn array(value: CFRetained<CFType>) -> Option<CFRetained<CFArray<CFType>>> {
    let value = value.downcast::<CFArray>().ok()?;
    // SAFETY: the Spaces list contains CF objects, individually downcast above.
    Some(unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(value) })
}

fn get(info: &Dictionary, key: &'static str) -> Option<CFRetained<CFType>> {
    info.get(&CFString::from_static_str(key))
}

fn number(info: &Dictionary, key: &'static str) -> Option<u64> {
    u64::try_from(get(info, key)?.downcast::<CFNumber>().ok()?.as_i64()?).ok()
}

#[cfg(test)]
mod tests;
