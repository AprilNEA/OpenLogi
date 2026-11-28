//! Synthesize the magnify events AppKit receives from a trackpad pinch.

use std::sync::LazyLock;

use mach2::mach_time::{mach_absolute_time, mach_timebase_info, mach_timebase_info_data_t};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType,
};

use super::super::SYNTHETIC_EVENT_USER_DATA;

// Event layout from the native pinch implementation proposed in OpenLogi #1175
// (akincan-kilic). AppKit promotes this HID gesture into NSEventTypeMagnify.
const GESTURE: CGEventType = CGEventType(29);
const HID_TYPE: CGEventField = CGEventField(110);
const MAGNIFICATION: CGEventField = CGEventField(113);
const PHASE: CGEventField = CGEventField(132);
const HID_ZOOM: i64 = 8;
const BEGAN: i64 = 1;
const CHANGED: i64 = 2;
const ENDED: i64 = 4;

/// An open pinch on the macOS HID event stream. Dropping it sends the end
/// phase, including on device disconnect or capture cancellation.
pub struct PinchGesture {
    source: CFRetained<CGEventSource>,
    end: CFRetained<CGEvent>,
}

impl PinchGesture {
    /// Begin with one nonzero magnification update. Prepare the end frame
    /// before posting the begin frame so allocation failure cannot strand it.
    #[must_use]
    pub fn begin(amount: f64) -> Option<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        let begin = frame(&source, BEGAN, 0.0)?;
        let change = frame(&source, CHANGED, amount)?;
        let end = frame(&source, ENDED, 0.0)?;
        post(&begin);
        post(&change);
        Some(Self { source, end })
    }

    /// Apply the next signed magnification increment.
    pub fn update(&self, amount: f64) {
        if let Some(event) = frame(&self.source, CHANGED, amount) {
            post(&event);
        }
    }
}

impl Drop for PinchGesture {
    fn drop(&mut self) {
        post(&self.end);
    }
}

fn frame(source: &CGEventSource, phase: i64, amount: f64) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(Some(source))?;
    CGEvent::set_type(Some(&event), GESTURE);
    CGEvent::set_flags(Some(&event), CGEventFlags::empty());
    CGEvent::set_integer_value_field(Some(&event), HID_TYPE, HID_ZOOM);
    CGEvent::set_integer_value_field(Some(&event), PHASE, phase);
    CGEvent::set_double_value_field(Some(&event), MAGNIFICATION, amount);
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::EventSourceUserData,
        SYNTHETIC_EVENT_USER_DATA,
    );
    Some(event)
}

fn post(event: &CGEvent) {
    CGEvent::set_timestamp(Some(event), uptime_ns());
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
}

#[expect(
    unsafe_code,
    reason = "mach2 exposes the two Darwin time functions as unsafe FFI"
)]
fn uptime_ns() -> u64 {
    static TIMEBASE: LazyLock<mach_timebase_info_data_t> = LazyLock::new(|| {
        let mut info = mach_timebase_info_data_t { numer: 1, denom: 1 };
        // SAFETY: the out parameter points to valid, writable timebase storage.
        if unsafe { mach_timebase_info(&raw mut info) } == 0 && info.denom != 0 {
            info
        } else {
            mach_timebase_info_data_t { numer: 1, denom: 1 }
        }
    });
    // SAFETY: the function takes no parameters and returns a monotonic tick count.
    let ticks = unsafe { mach_absolute_time() };
    let nanos = u128::from(ticks) * u128::from(TIMEBASE.numer) / u128::from(TIMEBASE.denom);
    u64::try_from(nanos).unwrap_or(u64::MAX)
}
