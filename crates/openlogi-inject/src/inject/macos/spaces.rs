//! Adjacent-Space switches with read-only, per-display Space confirmation.
//!
//! An animated switch types the user's own "Move left/right a space"
//! shortcut when it is enabled, so the Dock plays the slide it plays for the
//! keyboard (#1722, #1972). Otherwise, and for an instant switch, it posts
//! DockSwipe events. Their format follows yabai's SIP-enabled implementation:
//! <https://github.com/asmvik/yabai/blob/dd845723416f5fe92af49fad5ebab00369e07edd/src/space_manager.c#L927-L981>
//! A slide's frames follow Mac Mouse Fix's three-finger swipes instead, in
//! both of its event layouts, and are paced the way Logi Options+ drives its
//! three-finger swipe (see [`crate::inject::space_switch::slide`]).
//! No Dock injection, cursor warp, focus click, or symbolic-hotkey mutation.

use std::ffi::{c_int, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_app_kit::{NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFUUID,
};
use objc2_core_graphics::{
    CGError, CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGGetDisplaysWithPoint,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSObjectProtocol, NSProcessInfo, NSUserDefaults,
    ns_string,
};

use super::super::DesktopSwitch;
use super::super::space_switch::{
    self, Backend, Direction, Failure, Lease, Pace, PostGate, SlideFrame, SlidePhase, SpaceState,
};
use super::{app_services, app_services_symbol};

static BUSY: AtomicBool = AtomicBool::new(false);
/// Whether a switch slides. The host publishes the user's choice here
/// through [`set_desktop_switch`]; until it does, switches stay instant.
static SLIDE: AtomicBool = AtomicBool::new(false);

pub(in crate::inject) fn set_desktop_switch(switch: DesktopSwitch) {
    SLIDE.store(switch == DesktopSwitch::Animated, Ordering::Relaxed);
}

fn desktop_switch() -> DesktopSwitch {
    if SLIDE.load(Ordering::Relaxed) {
        DesktopSwitch::Animated
    } else {
        DesktopSwitch::Instant
    }
}

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
    // Read once: a setting changed mid-switch applies to the next one.
    let switch = desktop_switch();
    let result = space_switch::spawn_ordered(move |gate| {
        let _lease = lease;
        autoreleasepool(|_| {
            let result = Native::new(display_id, switch)
                .and_then(|mut native| space_switch::run(&mut native, direction, gate));
            match result {
                Ok(outcome) => {
                    tracing::debug!(
                        display_id,
                        ?direction,
                        ?switch,
                        ?outcome,
                        "Space switch result"
                    );
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
    switch: DesktopSwitch,
    uuid: String,
    started: Instant,
    changed: mpsc::Receiver<()>,
    _observer: Observer,
}

impl Native {
    fn new(display: u32, switch: DesktopSwitch) -> Result<Self, Failure> {
        let api = API.as_ref().ok_or(Failure::Unavailable)?;
        // SAFETY: dynamically resolved CGDisplayCreateUUIDFromDisplayID accepts
        // a public CGDirectDisplayID and returns a Create-rule CFUUID or null.
        let uuid =
            NonNull::new(unsafe { (api.display_uuid)(display) }).ok_or(Failure::Unavailable)?;
        // SAFETY: adopt the Create-rule result once; release via CFRetained.
        let uuid = unsafe { CFRetained::from_raw(uuid) };
        let uuid = CFUUID::new_string(None, Some(&uuid))
            .ok_or(Failure::Unavailable)?
            .to_string();
        // Subscribe before the first state query and before any output event.
        let (observer, changed) = Observer::new();
        Ok(Self {
            api,
            display,
            switch,
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
        let output = Output::prepare(self.switch, direction).ok_or(Failure::PostFailed)?;
        if cursor_display() != Some(self.display) {
            return Err(Failure::ContextChanged);
        }
        gate.commit(|| output.post())
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn wait_for_change(&mut self, remaining: Duration) {
        // Deadline wakeup is cleanup/reconciliation, never evidence of success.
        let _ = self.changed.recv_timeout(remaining);
    }
}

/// Every event of one switch. All of them are built before the first is
/// posted, so a failed allocation cannot leave the Dock in an unterminated
/// gesture. The shortcut builds its key edges the same way, when it posts.
enum Output {
    /// Began and Ended at full travel: the Dock switches at once.
    Instant([CFRetained<CGEvent>; 2]),
    /// A slide's frames, each with its time from the first.
    Slide(Vec<(Duration, CFRetained<CGEvent>)>),
    /// The user's enabled Space shortcut, typed as a keyboard types it. The
    /// Dock then slides exactly as it does for the keyboard. `fallback` is
    /// the slide to post instead if a held chord owns the shortcut's key.
    Shortcut {
        shortcut: super::symbolic_hotkey::SpaceShortcut,
        fallback: Vec<(Duration, CFRetained<CGEvent>)>,
    },
}

impl Output {
    fn prepare(switch: DesktopSwitch, direction: Direction) -> Option<Self> {
        match switch {
            DesktopSwitch::Instant => swipe_events(direction).map(Self::Instant),
            DesktopSwitch::Animated => {
                let frames = slide_events(direction)?;
                if let Some(shortcut) = super::symbolic_hotkey::space_shortcut(direction) {
                    return Some(Self::Shortcut {
                        shortcut,
                        fallback: frames,
                    });
                }
                tracing::debug!(?direction, "no usable Space shortcut — sliding");
                Some(Self::Slide(frames))
            }
        }
    }

    /// Post every event, each frame of a slide at its time.
    fn post(&self) {
        match self {
            Self::Instant(events) => {
                for event in events {
                    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
                }
            }
            Self::Slide(frames) => post_slide(frames),
            Self::Shortcut { shortcut, fallback } => {
                if !shortcut.post() {
                    tracing::debug!("Space shortcut not typed — sliding");
                    post_slide(fallback);
                }
            }
        }
    }
}

fn post_slide(frames: &[(Duration, CFRetained<CGEvent>)]) {
    space_switch::pace(
        frames,
        &mut SessionTap {
            origin: Instant::now(),
        },
    );
}

/// Posts a slide's frames to the session event tap on the real clock.
struct SessionTap {
    origin: Instant,
}

impl Pace<CFRetained<CGEvent>> for SessionTap {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn post(&mut self, event: &CFRetained<CGEvent>) {
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
    }
}

/// The Dock swipe event layout this macOS reads. The one owner of that
/// version split, for instant switches and slides alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockSwipeLayout {
    /// Private type-30 event fields, through macOS 26.
    Fields,
    /// An `IOHIDEvent` attached to the event, from macOS 27 (#1703).
    HidEvent,
}

static DOCK_SWIPE_LAYOUT: LazyLock<DockSwipeLayout> = LazyLock::new(|| {
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 27
    {
        DockSwipeLayout::HidEvent
    } else {
        DockSwipeLayout::Fields
    }
});

fn swipe_events(direction: Direction) -> Option<[CFRetained<CGEvent>; 2]> {
    match *DOCK_SWIPE_LAYOUT {
        DockSwipeLayout::HidEvent => hid_swipe_events(direction, natural_scrolling()),
        DockSwipeLayout::Fields => field_swipe_events(direction),
    }
}

fn field_swipe_events(direction: Direction) -> Option<[CFRetained<CGEvent>; 2]> {
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

/// The events of a slide in `direction`, each with its time from the first.
fn slide_events(direction: Direction) -> Option<Vec<(Duration, CFRetained<CGEvent>)>> {
    let frames = space_switch::slide(direction);
    match *DOCK_SWIPE_LAYOUT {
        DockSwipeLayout::HidEvent => hid_slide_events(&frames, natural_scrolling()),
        DockSwipeLayout::Fields => frames
            .into_iter()
            .map(|frame| Some((frame.at, slide_event(frame)?)))
            .collect(),
    }
}

/// One frame of a slide, in the fields a trackpad's three-finger swipe
/// fills. The layout follows Mac Mouse Fix's `TouchSimulator`, as proposed in
/// #1672.
fn slide_event(frame: SlideFrame) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(None)?;
    CGEvent::set_type(Some(&event), CGEventType(30));
    let phase = i64::from(slide_phase(frame.phase));
    // Dock swipe subtype, horizontal motion, and the phase in both slots the
    // trackpad path fills.
    for (field, value) in [(110, 23), (123, 1), (165, 1), (132, phase), (134, phase)] {
        CGEvent::set_integer_value_field(Some(&event), CGEventField(field), value);
    }
    CGEvent::set_double_value_field(Some(&event), CGEventField(41), 33_231.0);
    CGEvent::set_double_value_field(Some(&event), CGEventField(124), frame.progress);
    // The Dock also reads the progress as raw IEEE-754 f32 bits.
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField(135),
        i64::from(slide_progress_bits(frame.progress)),
    );
    let horizontal = f64::from(f32::from_bits(1));
    CGEvent::set_double_value_field(Some(&event), CGEventField(119), horizontal);
    CGEvent::set_double_value_field(Some(&event), CGEventField(139), horizontal);
    CGEvent::set_integer_value_field(Some(&event), CGEventField(136), 1);
    if frame.phase == SlidePhase::Ended {
        // An emphatic exit velocity toward the progress sign asks the Dock to
        // finish the switch rather than spring back, as the instant switch does.
        let velocity = frame.progress.signum() * 9999.0;
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

/// A slide phase as the Dock reads it, in both event layouts.
const fn slide_phase(phase: SlidePhase) -> u32 {
    match phase {
        SlidePhase::Began => 1,
        SlidePhase::Changed => 2,
        SlidePhase::Ended => 4,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the Dock swipe field stores its progress as IEEE-754 f32 bits"
)]
fn slide_progress_bits(progress: f64) -> u32 {
    (progress as f32).to_bits()
}

const HID_TYPE_VELOCITY: u32 = 9;
const HID_TYPE_DOCK_SWIPE: u32 = 23;
const HID_FIELD_MOTION: u32 = HID_TYPE_DOCK_SWIPE << 16 | 1;
const HID_FIELD_PROGRESS: u32 = HID_TYPE_DOCK_SWIPE << 16 | 2;
const HID_FIELD_FLAVOR: u32 = HID_TYPE_DOCK_SWIPE << 16 | 5;
const HID_FIELD_VELOCITY_X: u32 = HID_TYPE_VELOCITY << 16;
const HID_FIELD_VELOCITY_Y: u32 = HID_TYPE_VELOCITY << 16 | 1;
const HID_MOTION_HORIZONTAL: isize = 1;
const HID_FLAVOR_DOCK_PRIMARY: isize = 3;
const HID_OPTION_PHASE_SHIFT: u32 = 24;

type SetHidEvent = unsafe extern "C" fn(*const CGEvent, *const c_void);

fn hid_event(class: &AnyClass, kind: u32) -> Option<Retained<AnyObject>> {
    // SAFETY: `-[HIDEvent initWithType:timestamp:senderID:]` takes (u32, u64, u64).
    unsafe {
        let this: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![this, initWithType: kind, timestamp: 0_u64, senderID: 0_u64]
    }
}

/// Whether trackpad/mouse scrolling is set to "natural" direction — read
/// fresh on every call since it's a user preference that can change at
/// runtime without restarting the agent. `com.apple.swipescrolldirection`
/// lives in `NSGlobalDomain`, which `standardUserDefaults` already searches
/// as the last entry in its domain list, and is absent (meaning on, the
/// macOS default since 10.7) until the user has touched the Trackpad/Mouse
/// settings pane at least once.
pub(in crate::inject) fn natural_scrolling() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = ns_string!("com.apple.swipescrolldirection");
    defaults.objectForKey(key).is_none() || defaults.boolForKey(key)
}

/// Builds macOS 27's Dock swipes. macOS 27 reads an `IOHIDEvent` attached to
/// the event instead of its private fields.
struct HidSwipe {
    class: &'static AnyClass,
    attach: SetHidEvent,
    /// macOS 27 reads the swipe's sign the opposite way from the legacy
    /// field-based encoding, and only when natural scrolling is on (the
    /// default): with it off, Next/Previous are inverted (#1703, reported
    /// against two displays and 13/13 swipes on macOS 27.0.1).
    flip: f64,
}

impl HidSwipe {
    fn new(natural: bool) -> Option<Self> {
        let class = AnyClass::get(c"HIDEvent")?;
        let attach = app_services::sky_light_symbol(c"SLEventSetIOHIDEvent")?;
        // SAFETY: the resolved SPI has the `void(CGEventRef, IOHIDEventRef)` ABI.
        let attach = unsafe { std::mem::transmute::<*mut c_void, SetHidEvent>(attach) };
        let flip = if natural { -1.0 } else { 1.0 };
        Some(Self {
            class,
            attach,
            flip,
        })
    }

    /// A swipe event at `progress`, signed the way the private fields sign it.
    fn event(&self, phase: u32, progress: f64) -> Option<CFRetained<CGEvent>> {
        let hid = dock_swipe_hid_event(self.class, phase, self.flip * progress)?;
        let event = CGEvent::new(None)?;
        CGEvent::set_type(Some(&event), CGEventType(30));
        // SAFETY: both objects are live; SkyLight retains the HID event.
        unsafe { (self.attach)(&raw const *event, Retained::as_ptr(&hid).cast()) };
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::EventSourceUserData,
            super::super::SYNTHETIC_EVENT_USER_DATA,
        );
        Some(event)
    }
}

/// macOS 27's instant switch: Began and Ended at full travel.
fn hid_swipe_events(direction: Direction, natural: bool) -> Option<[CFRetained<CGEvent>; 2]> {
    let swipe = HidSwipe::new(natural)?;
    Some([
        swipe.event(1, direction.sign())?,
        swipe.event(4, direction.sign())?,
    ])
}

/// macOS 27's slide: every frame carries its cumulative progress in the
/// attached event, as Mac Mouse Fix posts its three-finger swipes on macOS 27.
fn hid_slide_events(
    frames: &[SlideFrame],
    natural: bool,
) -> Option<Vec<(Duration, CFRetained<CGEvent>)>> {
    let swipe = HidSwipe::new(natural)?;
    frames
        .iter()
        .map(|frame| {
            Some((
                frame.at,
                swipe.event(slide_phase(frame.phase), frame.progress)?,
            ))
        })
        .collect()
}

/// macOS 27 requires a velocity child on the end phase of a DockSwipe; it
/// points the way `progress` does. Private fields follow Mac Mouse Fix's event
/// investigation: <https://github.com/noah-nuebling/mac-mouse-fix/commit/f92d2d53a>.
fn dock_swipe_hid_event(
    class: &AnyClass,
    phase: u32,
    progress: f64,
) -> Option<Retained<AnyObject>> {
    let hid = hid_event(class, HID_TYPE_DOCK_SWIPE)?;
    // SAFETY: private `HIDEvent` setters taking (u32), (NSInteger, u32),
    // (double, u32) and (HIDEvent) respectively.
    unsafe {
        let _: () = msg_send![&*hid, setOptions: phase << HID_OPTION_PHASE_SHIFT];
        let _: () =
            msg_send![&*hid, setIntegerValue: HID_MOTION_HORIZONTAL, forField: HID_FIELD_MOTION];
        let _: () =
            msg_send![&*hid, setIntegerValue: HID_FLAVOR_DOCK_PRIMARY, forField: HID_FIELD_FLAVOR];
        let _: () = msg_send![&*hid, setDoubleValue: progress, forField: HID_FIELD_PROGRESS];
        if phase == 4 {
            let velocity = hid_event(class, HID_TYPE_VELOCITY)?;
            let speed = progress.signum() * 9999.0;
            let _: () =
                msg_send![&*velocity, setDoubleValue: speed, forField: HID_FIELD_VELOCITY_X];
            let _: () =
                msg_send![&*velocity, setDoubleValue: speed, forField: HID_FIELD_VELOCITY_Y];
            let _: () = msg_send![&*hid, appendEvent: &*velocity];
        }
    }
    Some(hid)
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
        // #1086: several reports of "Space switch unconfirmed" came from
        // multi-display setups with "Displays have separate Spaces" off
        // (the default) — a configuration this match was never observed
        // against. Log every candidate's identifier so a debug run from an
        // affected setup can show what WindowServer actually reports there,
        // rather than guessing at the private SPI's undocumented shape.
        let mut seen = Vec::new();
        for value in displays {
            let display = dictionary(value)?;
            let identifier = get(&display, "Display Identifier")?
                .downcast::<CFString>()
                .ok()?;
            let name = identifier.to_string();
            let matched = name == uuid || (single_display && name == "Main");
            seen.push((name.clone(), matched));
            if !matched {
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
        if selected.is_none() {
            tracing::debug!(
                uuid,
                single_display,
                candidates = ?seen,
                "Space state: no display matched the cursor's UUID"
            );
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
