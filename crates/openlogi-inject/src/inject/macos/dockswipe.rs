#![expect(
    unsafe_code,
    reason = "the SkyLight HIDEvent bridge and the private HIDEvent class are only reachable \
              via dlopen/dlsym FFI and ObjC runtime lookup"
)]
//! Live DockSwipe streaming for macOS 27+, which reads a DockSwipe IOHIDEvent
//! attached to the CGEvent via SkyLight's `SLEventSetIOHIDEvent` rather than
//! the pre-27 CGEvent fields. Constants follow Mac Mouse Fix #1936; verified
//! end-to-end on real hardware.
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::NSProcessInfo;

use crate::inject::{DockSwipeMotion, DockSwipePhase};

// IOHIDEventTypes.h: Velocity = 9, DockSwipe = 23; a field id is
// (type << 16) | index, and the phase rides in options bits 24–31.
const HID_TYPE_VELOCITY: u32 = 9;
const HID_TYPE_DOCK_SWIPE: u32 = 23;
const fn field(event_type: u32, index: u32) -> u32 {
    (event_type << 16) | index
}
const FIELD_DOCK_SWIPE_MOTION: u32 = field(HID_TYPE_DOCK_SWIPE, 1);
const FIELD_DOCK_SWIPE_PROGRESS: u32 = field(HID_TYPE_DOCK_SWIPE, 2);
const FIELD_DOCK_SWIPE_FLAVOR: u32 = field(HID_TYPE_DOCK_SWIPE, 5);
const FIELD_VELOCITY_X: u32 = field(HID_TYPE_VELOCITY, 0);
const FIELD_VELOCITY_Y: u32 = field(HID_TYPE_VELOCITY, 1);
const FIELD_VELOCITY_Z: u32 = field(HID_TYPE_VELOCITY, 2);

/// kIOHIDGestureFlavorDockPrimary — the flavor Mac Mouse Fix uses.
const FLAVOR_DOCK_PRIMARY: isize = 3;

// kIOHIDEventPhaseBegan/Changed/Ended/Cancelled; the shift is kIOHIDEventEventOptionPhaseShift.
const PHASE_SHIFT: u32 = 24;
const PHASE_BEGAN: u32 = 1;
const PHASE_CHANGED: u32 = 2;
const PHASE_ENDED: u32 = 4;
const PHASE_CANCELLED: u32 = 8;

/// Type 30 (NSEventTypeMagnify), the carrier CGEvent type Mac Mouse Fix posts on.
const GESTURE_CG_EVENT_TYPE: c_uint = 30;
/// kCGSessionEventTap.
const SESSION_EVENT_TAP: c_uint = 1;

/// Mac Mouse Fix: the release velocity is roughly the last frame's delta × 100.
const EXIT_VELOCITY_SCALE: f64 = 100.0;
/// Mac Mouse Fix re-posts the end event — WindowServer can drop the first under load.
const END_RESEND_DELAYS: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(500)];

/// The DockSwipe system state is global and single-owner: `owner` isolates
/// the stream to one capture session, `generation` invalidates pending end
/// resends when a new gesture begins.
static STREAM: LazyLock<Mutex<Stream>> = LazyLock::new(|| Mutex::new(Stream::default()));

#[derive(Debug, Default)]
struct Stream {
    owner: u64,
    progress: f64,
    last_delta: f64,
    generation: u64,
}

pub(in crate::inject) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        // HIDEvent only resolves after SkyLight is mapped into the process.
        if NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion
            < 27
        {
            return false;
        }
        sky_light().is_some() && AnyClass::get(c"HIDEvent").is_some()
    })
}

pub(in crate::inject) fn post(
    owner: u64,
    motion: DockSwipeMotion,
    phase: DockSwipePhase,
    delta: f64,
) -> bool {
    if phase == DockSwipePhase::Began {
        return post_began(owner, motion, delta);
    }
    let motion_id = motion_id(motion);
    let Ok(mut stream) = STREAM.lock() else {
        tracing::warn!("dock swipe stream mutex poisoned");
        return false;
    };
    let event_plan = if phase == DockSwipePhase::Changed {
        stream.advance(owner, delta)
    } else {
        stream.finish(owner, phase)
    };
    let Some(event_plan) = event_plan else {
        // Zero-delta frames and superseded-owner frames post nothing.
        return false;
    };
    drop(stream);
    let posted = post_event(motion_id, &event_plan);
    if posted && let DockSwipePhase::End | DockSwipePhase::Cancel = phase {
        schedule_end_resend(motion_id, event_plan);
    }
    if !posted {
        tracing::warn!(?motion, ?phase, "dock swipe event could not be posted");
    }
    posted
}

/// IOHIDGestureMotion values: HorizontalX = 1, VerticalY = 2, Scale = 3.
fn motion_id(motion: DockSwipeMotion) -> isize {
    match motion {
        DockSwipeMotion::Horizontal => 1,
        DockSwipeMotion::Vertical => 2,
        DockSwipeMotion::Pinch => 3,
    }
}

/// Open a gesture transactionally: ownership commits only if the Began
/// event delivers, so a failed post leaves the previous owner's gesture alive.
fn post_began(owner: u64, motion: DockSwipeMotion, delta: f64) -> bool {
    let Ok(mut stream) = STREAM.lock() else {
        tracing::warn!("dock swipe stream mutex poisoned");
        return false;
    };
    post_began_with(&mut stream, owner, delta, |plan| {
        post_event(motion_id(motion), &plan)
    })
}

fn post_began_with(
    stream: &mut Stream,
    owner: u64,
    delta: f64,
    deliver: impl FnOnce(EventPlan) -> bool,
) -> bool {
    let plan = stream.began_plan(delta);
    if deliver(plan) {
        stream.commit_began(owner, &plan);
        true
    } else {
        false
    }
}

#[derive(Clone, Copy)]
struct EventPlan {
    options: u32,
    progress: f64,
    velocity: Option<f64>,
    generation: u64,
}

impl Stream {
    /// Plan a Began event without changing stream ownership.
    fn began_plan(&self, delta: f64) -> EventPlan {
        EventPlan {
            options: PHASE_BEGAN << PHASE_SHIFT,
            progress: delta,
            velocity: None,
            generation: self.generation.wrapping_add(1),
        }
    }

    /// Commit a Began plan only after successful delivery.
    fn commit_began(&mut self, owner: u64, plan: &EventPlan) {
        self.owner = owner;
        self.generation = plan.generation;
        self.progress = plan.progress;
        self.last_delta = plan.progress;
    }

    fn advance(&mut self, owner: u64, delta: f64) -> Option<EventPlan> {
        if self.owner != owner {
            return None;
        }
        if delta == 0.0 {
            return None;
        }
        self.progress += delta;
        self.last_delta = delta;
        Some(EventPlan {
            options: PHASE_CHANGED << PHASE_SHIFT,
            progress: self.progress,
            velocity: None,
            generation: self.generation,
        })
    }

    fn finish(&mut self, owner: u64, phase: DockSwipePhase) -> Option<EventPlan> {
        if self.owner != owner {
            return None;
        }
        let velocity = self.last_delta * EXIT_VELOCITY_SCALE;
        Some(EventPlan {
            options: release_phase(phase, self.progress, self.last_delta) << PHASE_SHIFT,
            progress: self.progress,
            velocity: Some(velocity),
            generation: self.generation,
        })
    }
}

/// Mac Mouse Fix's release rule: moving with the travel commits, otherwise it springs back.
fn release_phase(phase: DockSwipePhase, progress: f64, last_delta: f64) -> u32 {
    let commits = match phase {
        DockSwipePhase::Cancel => false,
        _ => progress != 0.0 && last_delta.signum() == progress.signum(),
    };
    if commits {
        PHASE_ENDED
    } else {
        PHASE_CANCELLED
    }
}

fn post_event(motion_id: isize, plan: &EventPlan) -> bool {
    let Some(bridge) = sky_light() else {
        tracing::warn!("SkyLight SLEventSetIOHIDEvent unavailable");
        return false;
    };
    autoreleasepool(|_| {
        // SAFETY: CGEventCreate(NULL) returns a +1 CGEventRef balanced by CFRelease below.
        let cg_event = unsafe { CGEventCreate(std::ptr::null()) };
        if cg_event.is_null() {
            return false;
        }
        // SAFETY: cg_event is a live +1 CGEventRef.
        let carrier_timestamp = unsafe { CGEventGetTimestamp(cg_event) };
        let Some(hid_event) = build_hid_event(motion_id, plan, carrier_timestamp) else {
            // SAFETY: balance the +1 from CGEventCreate above.
            unsafe { CFRelease(cg_event) };
            tracing::warn!("private HIDEvent class unavailable");
            return false;
        };
        // SAFETY: live +1 CGEventRef; the bridge attaches the HIDEvent for the event's lifetime.
        unsafe {
            CGEventSetType(cg_event, GESTURE_CG_EVENT_TYPE);
            (bridge.set_hid_event)(cg_event, Retained::as_ptr(&hid_event).cast());
            CGEventPost(SESSION_EVENT_TAP, cg_event);
            CFRelease(cg_event);
        }
        tracing::debug!(
            motion = motion_id,
            options = plan.options,
            progress = plan.progress,
            velocity = plan.velocity,
            "dock swipe posted"
        );
        true
    })
}

fn build_hid_event(
    motion_id: isize,
    plan: &EventPlan,
    carrier_timestamp: u64,
) -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"HIDEvent")?;
    // SAFETY: HIDEvent responds to `alloc` (+1 result); the class is opaque.
    let alloc: *mut AnyObject = unsafe { msg_send![class, alloc] };
    if alloc.is_null() {
        return None;
    }
    // SAFETY: `initWithType:timestamp:senderID:` is `(uint32_t, uint64_t,
    // uint64_t)` on the prototype's interface; it consumes alloc's +1.
    let event: *mut AnyObject = unsafe {
        msg_send![alloc, initWithType: HID_TYPE_DOCK_SWIPE, timestamp: carrier_timestamp, senderID: 0_u64]
    };
    // SAFETY: the init's +1 transfers into the Retained before any fallible child step.
    let event: Retained<AnyObject> = unsafe { Retained::from_raw(event) }?;
    // SAFETY: the setters are `(NSInteger|double, uint32_t)` on the prototype's verified interface.
    unsafe {
        let () = msg_send![&event, setIntegerValue: motion_id, forField: FIELD_DOCK_SWIPE_MOTION];
        let () = msg_send![&event, setIntegerValue: FLAVOR_DOCK_PRIMARY, forField: FIELD_DOCK_SWIPE_FLAVOR];
        let () =
            msg_send![&event, setDoubleValue: plan.progress, forField: FIELD_DOCK_SWIPE_PROGRESS];
        let () = msg_send![&event, setOptions: plan.options];
    }
    if let Some(velocity) = plan.velocity {
        let child = velocity_event(velocity, carrier_timestamp)?;
        // SAFETY: `appendEvent:` retains the live child pointer for the call's duration.
        unsafe {
            let () = msg_send![&event, appendEvent: Retained::as_ptr(&child)];
        }
    }
    Some(event)
}

fn velocity_event(velocity: f64, carrier_timestamp: u64) -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"HIDEvent")?;
    // SAFETY: same opaque `alloc` (+1) as `build_hid_event`.
    let alloc: *mut AnyObject = unsafe { msg_send![class, alloc] };
    if alloc.is_null() {
        return None;
    }
    // SAFETY: the init consumes alloc's +1 and returns +1 or nil.
    let event: *mut AnyObject = unsafe {
        msg_send![alloc, initWithType: HID_TYPE_VELOCITY, timestamp: carrier_timestamp, senderID: 0_u64]
    };
    if event.is_null() {
        return None;
    }
    // SAFETY: `setDoubleValue:forField:` is `(double, uint32_t)`; real events carry speed on X/Y.
    unsafe {
        let () = msg_send![event, setDoubleValue: velocity, forField: FIELD_VELOCITY_X];
        let () = msg_send![event, setDoubleValue: velocity, forField: FIELD_VELOCITY_Y];
        let () = msg_send![event, setDoubleValue: 0.0, forField: FIELD_VELOCITY_Z];
    }
    // SAFETY: the init's +1 transfers into the Retained; nil returned above.
    unsafe { Retained::from_raw(event) }
}

/// Mac Mouse Fix's workaround for WindowServer dropping the first end event
/// under load; a newer gesture (generation bump) cancels the resends.
fn schedule_end_resend(motion_id: isize, plan: EventPlan) {
    let result = std::thread::Builder::new()
        .name("dockswipe-resend".into())
        .spawn(move || {
            let mut previous = Duration::ZERO;
            for delay in END_RESEND_DELAYS {
                std::thread::sleep(delay.saturating_sub(previous));
                previous = delay;
                let Ok(stream) = STREAM.lock() else {
                    return;
                };
                if stream.generation != plan.generation {
                    return;
                }
                drop(stream);
                if !post_event(motion_id, &plan) {
                    return;
                }
            }
        });
    if let Err(error) = result {
        tracing::warn!(%error, "dock swipe end resend could not be scheduled");
    }
}

struct SkyLightBridge {
    set_hid_event: unsafe extern "C" fn(*const c_void, *const c_void),
}

fn sky_light() -> Option<&'static SkyLightBridge> {
    static BRIDGE: OnceLock<Option<SkyLightBridge>> = OnceLock::new();
    BRIDGE
        .get_or_init(|| {
            const RTLD_LAZY: c_int = 0x1;
            const SKY_LIGHT: &CStr =
                c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
            // SAFETY: `dlopen`/`dlsym` take valid C strings; the handle is never closed.
            let set_hid_event = unsafe {
                let handle = dlopen(SKY_LIGHT.as_ptr(), RTLD_LAZY);
                if handle.is_null() {
                    return None;
                }
                dlsym(handle, c"SLEventSetIOHIDEvent".as_ptr())
            };
            if set_hid_event.is_null() {
                return None;
            }
            Some(SkyLightBridge {
                set_hid_event: unsafe {
                    // SAFETY: the symbol, when present, matches the fn pointer signature below.
                    std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(*const c_void, *const c_void),
                    >(set_hid_event)
                },
            })
        })
        .as_ref()
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreate(source: *const c_void) -> *const c_void;
    fn CGEventGetTimestamp(event: *const c_void) -> u64;
    fn CGEventSetType(event: *const c_void, event_type: c_uint);
    fn CGEventPost(tap: c_uint, event: *const c_void);
    fn CFRelease(cf: *const c_void);
}

#[cfg(test)]
mod tests {
    use super::{
        FIELD_DOCK_SWIPE_FLAVOR, FIELD_DOCK_SWIPE_MOTION, FIELD_DOCK_SWIPE_PROGRESS,
        FIELD_VELOCITY_X, PHASE_BEGAN, PHASE_CANCELLED, PHASE_ENDED, PHASE_SHIFT, Stream,
        post_began_with, release_phase,
    };
    use crate::inject::DockSwipePhase;

    #[test]
    fn failed_began_delivery_leaves_the_previous_owner_intact() {
        let mut stream = Stream::default();
        assert!(post_began_with(&mut stream, 1, 0.1, |_| true));
        assert!(stream.advance(1, 0.1).is_some());

        // Owner 2's failed Began commits nothing: owner 1 keeps the stream.
        assert!(!post_began_with(&mut stream, 2, 0.2, |_| false));
        assert!(stream.advance(1, -0.05).is_some());
        assert!(stream.finish(1, DockSwipePhase::End).is_some());
        assert!(stream.advance(2, 0.2).is_none());
        assert!(stream.finish(2, DockSwipePhase::Cancel).is_none());
    }

    #[test]
    fn delivered_began_commits_the_new_owner() {
        let mut stream = Stream::default();
        assert!(post_began_with(&mut stream, 2, 0.2, |_| true));

        assert_eq!(stream.owner, 2);
        assert!((stream.progress - 0.2).abs() < 1e-9);
        assert!((stream.last_delta - 0.2).abs() < 1e-9);
        assert!(stream.advance(1, 0.1).is_none());
        assert!(stream.advance(2, 0.1).is_some());
    }

    #[test]
    fn dock_swipe_field_ids_match_iohid_event_types() {
        assert_eq!(FIELD_DOCK_SWIPE_MOTION, 23 << 16 | 1);
        assert_eq!(FIELD_DOCK_SWIPE_PROGRESS, 23 << 16 | 2);
        assert_eq!(FIELD_DOCK_SWIPE_FLAVOR, 23 << 16 | 5);
        assert_eq!(FIELD_VELOCITY_X, 9 << 16);
    }

    #[test]
    fn release_along_travel_commits_and_against_springs_back() {
        let released = u64::from(PHASE_ENDED) << PHASE_SHIFT;
        let cancelled = u64::from(PHASE_CANCELLED) << PHASE_SHIFT;
        assert_eq!(
            u64::from(release_phase(DockSwipePhase::End, 0.5, 0.1)) << PHASE_SHIFT,
            released
        );
        assert_eq!(
            u64::from(release_phase(DockSwipePhase::End, -0.5, -0.1)) << PHASE_SHIFT,
            released
        );
        assert_eq!(
            u64::from(release_phase(DockSwipePhase::End, 0.5, -0.1)) << PHASE_SHIFT,
            cancelled,
            "a release moving against the travel springs back"
        );
        assert_eq!(
            u64::from(release_phase(DockSwipePhase::End, 0.0, 0.1)) << PHASE_SHIFT,
            cancelled,
            "no accumulated progress cannot commit"
        );
        assert_eq!(
            u64::from(release_phase(DockSwipePhase::Cancel, 0.5, 0.1)) << PHASE_SHIFT,
            cancelled,
            "an explicit cancel always springs back"
        );
        assert_eq!(u64::from(PHASE_BEGAN) << PHASE_SHIFT, 1 << 24);
    }
}
