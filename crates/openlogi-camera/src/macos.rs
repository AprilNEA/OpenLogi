//! AVFoundation-backed camera enumeration.
//!
//! `+[AVCaptureDevice devicesWithMediaType:AVMediaTypeVideo]` returns every
//! video-capable capture device macOS knows about; for each we read the same
//! `localizedName` / `uniqueID` / `modelID` strings `system_profiler
//! SPCameraDataType` reports, plus the device's supported formats (resolution +
//! frame rate). Vendor parsing + the Logitech filter live in the
//! platform-independent parent module.
//!
//! All of this is metadata — no capture session is opened, so no Camera
//! permission is required.
//!
//! FFI is `objc2` (matching the rest of the workspace's ObjC surface): the
//! dynamic `AVCaptureDevice` classes aren't in a typed framework crate, so this
//! uses the `objc2` runtime (`AnyClass::get` + `msg_send!`) like
//! `platform/permissions.rs`. There is no long-lived ownership here — every
//! object AVFoundation hands back is autoreleased and copied into an owned Rust
//! value before the enclosing [`autoreleasepool`] drains — so no `Retained<T>`
//! is needed (an off-run-loop caller thread has no pool of its own).

#![expect(
    unsafe_code,
    reason = "AVFoundation (AVCaptureDevice) camera-enumeration FFI"
)]

use std::ffi::CStr;
use std::os::raw::c_char;

use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject};

pub(crate) mod formats;
use formats::{DeviceFormat, RateRange, round_fps};

/// Raw camera metadata as reported by `AVCaptureDevice`, before vendor parsing.
pub(crate) struct RawCamera {
    pub name: String,
    pub unique_id: String,
    pub model_id: String,
    /// Largest supported frame size, `(0, 0)` if none was reported.
    pub max_width: u32,
    pub max_height: u32,
    /// Highest supported frame rate (fps) at any format, `0` if none.
    pub max_fps: u32,
}

// `AVMediaTypeVideo` is an `NSString` constant exported by AVFoundation; the
// framework must be linked for it and the `AVCaptureDevice` class to resolve.
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeVideo: *const AnyObject;
}

#[repr(C)]
struct CMVideoDimensions {
    width: i32,
    height: i32,
}

/// Opaque `CMFormatDescriptionRef`. `AVCaptureDeviceFormat.formatDescription`
/// hands back a CoreMedia handle, not an Objective-C object, so it needs its own
/// type encoding: objc2 verifies msg_send return types against the runtime and
/// panics if we claim `AnyObject` (`@`) for what CoreMedia reports as
/// `^{opaqueCMFormatDescription=}`.
#[repr(C)]
struct CMFormatDescription {
    _private: [u8; 0],
}

// SAFETY: only ever handled behind a pointer (a `CMFormatDescriptionRef`); the
// encoding mirrors CoreMedia's `^{opaqueCMFormatDescription=}`.
unsafe impl RefEncode for CMFormatDescription {
    const ENCODING_REF: Encoding =
        Encoding::Pointer(&Encoding::Struct("opaqueCMFormatDescription", &[]));
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionGetDimensions(desc: *mut CMFormatDescription) -> CMVideoDimensions;
}

/// Enumerate every video `AVCaptureDevice`, as raw metadata. The Logitech
/// filter is applied by the caller in `lib.rs`.
pub(crate) fn enumerate() -> Vec<RawCamera> {
    let Some(device_cls) = AnyClass::get(c"AVCaptureDevice") else {
        return Vec::new();
    };

    // An explicit pool brackets the work: the returned array and its devices are
    // autoreleased, and a caller thread with no run loop drains none on its own.
    // Every string is copied into an owned `String` before the pool drops.
    autoreleasepool(|_| {
        // SAFETY: `AVCaptureDevice` exists once AVFoundation is linked. Every
        // message uses a documented selector and matching types; `AVMediaTypeVideo`
        // is the framework's exported `NSString` constant.
        unsafe {
            let devices: *mut AnyObject =
                msg_send![device_cls, devicesWithMediaType: AVMediaTypeVideo];

            let mut out = Vec::new();
            if !devices.is_null() {
                let count: usize = msg_send![devices, count];
                out.reserve(count);
                for i in 0..count {
                    let device: *mut AnyObject = msg_send![devices, objectAtIndex: i];
                    if device.is_null() {
                        continue;
                    }
                    let name_obj: *mut AnyObject = msg_send![device, localizedName];
                    let uid_obj: *mut AnyObject = msg_send![device, uniqueID];
                    let model_obj: *mut AnyObject = msg_send![device, modelID];
                    if let (Some(name), Some(unique_id), Some(model_id)) =
                        (nsstring(name_obj), nsstring(uid_obj), nsstring(model_obj))
                    {
                        let (max_width, max_height, max_fps) = best_format(device);
                        out.push(RawCamera {
                            name,
                            unique_id,
                            model_id,
                            max_width,
                            max_height,
                            max_fps,
                        });
                    }
                }
            }
            out
        }
    })
}

/// Largest `(width, height, max_fps)` among the device's supported formats, or
/// `(0, 0, 0)` when none are reported. Reads format metadata only — no capture
/// session, so no Camera permission is needed.
fn best_format(device: *mut AnyObject) -> (u32, u32, u32) {
    // SAFETY: `device` is a non-null `AVCaptureDevice` out of the enclosing
    // `devicesWithMediaType:` array, alive for the enclosing pool.
    let formats = device_formats(unsafe { &*device });
    let mut best = (0u32, 0u32, 0u32);
    for format in formats {
        let fps = format
            .rates
            .iter()
            .map(|range| round_fps(range.max))
            .max()
            .unwrap_or(0);
        let (w, h) = (format.width, format.height);
        let area = u64::from(w) * u64::from(h);
        let best_area = u64::from(best.0) * u64::from(best.1);
        if area > best_area || (w == best.0 && h == best.1 && fps > best.2) {
            best = (w, h, fps);
        }
    }
    best
}

/// Every format `device` supports, in the order of its `formats` array: an
/// index into the result is an index into that array, which is how
/// `capture_macos` finds the format to activate. An entry the device describes
/// with no frame size has none (`0×0`) and no rates, rather than being left
/// out, so the indices stay aligned. Metadata only — no capture session, so no
/// Camera permission is needed.
///
/// The array and its elements are autoreleased: call this inside an
/// [`autoreleasepool`] on a thread that has no run loop of its own.
pub(crate) fn device_formats(device: &AnyObject) -> Vec<DeviceFormat> {
    // SAFETY: `device` is a valid `AVCaptureDevice`; `formats` is an autoreleased
    // `NSArray` of `AVCaptureDeviceFormat` (never holding nil), each exposing a
    // `CMFormatDescription` and frame-rate ranges via documented selectors.
    unsafe {
        let formats: *mut AnyObject = msg_send![device, formats];
        if formats.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![formats, count];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let format: *mut AnyObject = msg_send![formats, objectAtIndex: i];
            out.push(read_format(format));
        }
        out
    }
}

/// One `AVCaptureDeviceFormat` as plain data.
///
/// # Safety
/// `format` is null or a valid `AVCaptureDeviceFormat`.
unsafe fn read_format(format: *mut AnyObject) -> DeviceFormat {
    let mut read = DeviceFormat {
        width: 0,
        height: 0,
        rates: Vec::new(),
    };
    if format.is_null() {
        return read;
    }
    // SAFETY: documented selectors on a valid `AVCaptureDeviceFormat`.
    unsafe {
        let desc: *mut CMFormatDescription = msg_send![format, formatDescription];
        if desc.is_null() {
            return read;
        }
        let dims = CMVideoFormatDescriptionGetDimensions(desc);
        read.width = u32::try_from(dims.width).unwrap_or(0);
        read.height = u32::try_from(dims.height).unwrap_or(0);
        read.rates = rate_ranges(format);
    }
    read
}

/// A format's `videoSupportedFrameRateRanges`, in the device's order.
///
/// # Safety
/// `format` is a valid `AVCaptureDeviceFormat`.
unsafe fn rate_ranges(format: *mut AnyObject) -> Vec<RateRange> {
    // SAFETY: documented selectors on a valid `AVCaptureDeviceFormat` and its
    // `AVFrameRateRange`s; `minFrameRate` / `maxFrameRate` return a `double`.
    unsafe {
        let ranges: *mut AnyObject = msg_send![format, videoSupportedFrameRateRanges];
        if ranges.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![ranges, count];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let range: *mut AnyObject = msg_send![ranges, objectAtIndex: i];
            out.push(if range.is_null() {
                RateRange { min: 0.0, max: 0.0 }
            } else {
                RateRange {
                    min: msg_send![range, minFrameRate],
                    max: msg_send![range, maxFrameRate],
                }
            });
        }
        out
    }
}

/// Copy an `NSString` into an owned Rust `String`. `None` for a null pointer or
/// non-UTF-8 contents.
fn nsstring(s: *mut AnyObject) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: `s` is a non-null `NSString`; `UTF8String` yields a NUL-terminated
    // C string valid for the lifetime of the (autoreleased) string, which we
    // copy out of immediately.
    unsafe {
        let utf8: *const c_char = msg_send![s, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }
}
