//! AVFoundation camera capture: a one-shot snapshot and a live frame stream.
//!
//! Both open an `AVCaptureSession` on the chosen camera and read BGRA frames
//! through an `AVCaptureVideoDataOutput` delegate. Frames are kept in gpui's
//! native **BGRA** order so the preview uploads them with no channel swap; the
//! snapshot path swaps to RGBA once when it encodes the PNG. Capturing (unlike
//! enumeration) needs Camera permission *and* an app bundle carrying
//! `NSCameraUsageDescription`; from an unbundled binary macOS denies access,
//! which surfaces as [`CaptureError::AccessDenied`].
//!
//! A preview streams at the [`StreamMode`] it is handed, when the camera lists
//! any (see [`stream_modes`]): the format and frame duration are set on the
//! device itself, and the preview keeps the device's configuration lock until
//! it ends. That lock is what makes the mode real: a session that does not
//! hold it resets the device to its preset's format at `startRunning` (1080p
//! at 24 fps by default), whatever was set before. The mode belongs to the
//! preview alone. The device object is shared by every process and keeps the
//! last format anyone set, but an application that opens the camera sets its
//! own, so nothing here is a setting for other applications to inherit.
//!
//! FFI is `objc2` (matching the rest of the workspace's ObjC surface). The
//! `AVFoundation` classes aren't in a typed framework crate, so this uses the
//! `objc2` runtime (`class!` + `msg_send!`) with **`Retained<T>` for every
//! retained object** (session / output / delegate), so ownership is leak-proof
//! by construction rather than hand-balanced `retain`/`release`. The frame
//! delegate is an `NSObject` subclass declared with [`define_class!`] (the same
//! macro the agent's tray target uses); its one ivar is the owning session's
//! [`FrameSink`], and it is driven on a background dispatch queue, so it
//! inherits `NSObject`'s any-thread kind and needs no main-thread affinity (no
//! `MainThreadMarker`) — AVFoundation's session start/stop and its
//! sample-buffer callback are all off-main.

#![expect(
    unsafe_code,
    reason = "AVFoundation / CoreMedia / CoreVideo capture FFI"
)]
use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{AnyThread, DefinedClass, class, define_class, msg_send};

pub use crate::capture_types::{CaptureError, Frame};
use crate::macos::formats::{self, FrameTiming, Selection};
use crate::{StreamMode, StreamModes};

/// kCVPixelFormatType_32BGRA ('BGRA').
const PIXEL_FORMAT_32BGRA: u32 = 0x4247_5241;
/// kCVPixelBufferLock_ReadOnly.
const LOCK_READ_ONLY: u64 = 1;

/// CoreMedia's `CMTime`: `value / timescale` seconds, plus flags and an epoch.
#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

// SAFETY: `repr(C)` and field-for-field the C struct `{?=qiIq}`
// (`CMTimeValue`, `CMTimeScale`, `CMTimeFlags`, `CMTimeEpoch`).
unsafe impl Encode for CMTime {
    const ENCODING: Encoding = Encoding::Struct(
        "?",
        &[
            Encoding::LongLong,
            Encoding::Int,
            Encoding::UInt,
            Encoding::LongLong,
        ],
    );
}

/// One session's frame plumbing: the delegate writes, the owning [`Session`] /
/// [`CameraStream`] reads. Owned per session — the Windows and Linux backends
/// already model this per stream — so which session a frame belongs to is
/// carried by ownership: two live sessions (a preview plus a snapshot, or an
/// in-flight delegate callback from a session being replaced) can never stamp
/// frames into each other's slot or clobber each other's decimation target.
struct FrameSink {
    /// The most recent frame the delegate decoded, behind an `Arc` so the
    /// preview's poll hands out a cheap refcount bump instead of copying the
    /// whole buffer.
    latest: Mutex<Option<Arc<Frame>>>,
    /// Increments on every delivered frame, so a poller can tell a new frame
    /// from a repeat without comparing pixel buffers.
    generation: AtomicU64,
    /// Target max width for delegate downsampling (0 = full resolution).
    /// A preview on the fixed 720p preset sets this so a camera that streams
    /// wider than asked decimates down in one strided pass instead of copying
    /// (and uploading) pixels the preview does not need. A preview at a chosen
    /// mode leaves it 0: the size picked is the size shown.
    target_width: AtomicU32,
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeVideo: *const AnyObject;
    static AVCaptureSessionPreset1280x720: *const AnyObject;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *mut AnyObject) -> *mut AnyObject;
    /// `value / timescale` seconds, as a valid `CMTime`. A pure function of its
    /// arguments, with no precondition.
    safe fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const AnyObject;
    fn CVPixelBufferLockBaseAddress(pb: *mut AnyObject, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut AnyObject, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut AnyObject) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pb: *mut AnyObject) -> usize;
    fn CVPixelBufferGetWidth(pb: *mut AnyObject) -> usize;
    fn CVPixelBufferGetHeight(pb: *mut AnyObject) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    // The last parameter is a CoreFoundation `Boolean` (unsigned char); `0` is
    // false. Kept as `u8` rather than an objc `BOOL` to match the C type exactly.
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut AnyObject;
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `FrameDelegate` does
    // not implement `Drop`. Its one ivar is an `Arc<FrameSink>` (`Send + Sync`)
    // and its one method touches only that sink, so it is safe to use from the
    // background dispatch queue AVFoundation drives it on (default any-thread
    // kind, no `thread_kind`).
    #[unsafe(super(NSObject))]
    #[name = "OLCameraFrameDelegate"]
    #[ivars = Arc<FrameSink>]
    struct FrameDelegate;

    impl FrameDelegate {
        /// `captureOutput:didOutputSampleBuffer:fromConnection:` — copies the
        /// sample buffer's BGRA pixels (optionally decimated) into its
        /// session's [`FrameSink`].
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: *mut AnyObject,
            sbuf: *mut AnyObject,
            _conn: *mut AnyObject,
        ) {
            let sink = self.ivars();
            // SAFETY: `sbuf` is a valid CMSampleBuffer delivered by AVFoundation;
            // the image buffer is locked for the read and unlocked before return.
            unsafe {
                let pb = CMSampleBufferGetImageBuffer(sbuf);
                if pb.is_null() || CVPixelBufferLockBaseAddress(pb, LOCK_READ_ONLY) != 0 {
                    return;
                }
                let base = CVPixelBufferGetBaseAddress(pb).cast::<u8>();
                let bytes_per_row = CVPixelBufferGetBytesPerRow(pb);
                let width = CVPixelBufferGetWidth(pb);
                let height = CVPixelBufferGetHeight(pb);
                let target = sink.target_width.load(Ordering::Relaxed) as usize;
                let step = if target > 0 && width > target {
                    width.div_ceil(target)
                } else {
                    1
                };
                let out_w = width / step;
                let out_h = height / step;
                if !base.is_null() && out_w > 0 && out_h > 0 {
                    let mut bgra = vec![0u8; out_w * out_h * 4];
                    let dst = bgra.as_mut_ptr();
                    if step == 1 {
                        // Source is already BGRA (kCVPixelFormatType_32BGRA) — one
                        // memcpy per row, skipping any driver row padding.
                        for oy in 0..out_h {
                            let row = base.add(oy * bytes_per_row);
                            std::ptr::copy_nonoverlapping(row, dst.add(oy * out_w * 4), out_w * 4);
                        }
                    } else {
                        for oy in 0..out_h {
                            let row = base.add(oy * step * bytes_per_row);
                            for ox in 0..out_w {
                                let src = row.add(ox * step * 4);
                                let out = (oy * out_w + ox) * 4;
                                std::ptr::copy_nonoverlapping(src, dst.add(out), 4);
                            }
                        }
                    }
                    if let Ok(mut slot) = sink.latest.lock() {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "CoreVideo reports sensor-sized dimensions, divided down again by `step`"
                        )]
                        let frame = Frame {
                            width: out_w as u32,
                            height: out_h as u32,
                            bgra,
                        };
                        *slot = Some(Arc::new(frame));
                        sink.generation.fetch_add(1, Ordering::Relaxed);
                    }
                }
                CVPixelBufferUnlockBaseAddress(pb, LOCK_READ_ONLY);
            }
        }
    }
);

impl FrameDelegate {
    fn new(sink: Arc<FrameSink>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(sink);
        // SAFETY: `init` initializes our freshly-allocated NSObject subclass and
        // returns it (the two-phase construction objc2's `define_class!` uses).
        unsafe { msg_send![super(this), init] }
    }
}

/// Current Camera authorization: `Some(true)` usable, `Some(false)` denied,
/// `None` undetermined (caller should request access).
fn authorization() -> Option<bool> {
    let cls = class!(AVCaptureDevice);
    // SAFETY: documented class method returning an AVAuthorizationStatus NSInteger;
    // `AVMediaTypeVideo` is AVFoundation's exported `NSString` constant.
    let status: isize =
        unsafe { msg_send![cls, authorizationStatusForMediaType: AVMediaTypeVideo] };
    match status {
        3 => Some(true),
        1 | 2 => Some(false),
        _ => None,
    }
}

/// Request Camera access and block until the user answers (or `timeout`).
fn request_access(timeout: Duration) -> bool {
    let answered = std::sync::Arc::new(Mutex::new(None::<bool>));
    let sink = answered.clone();
    // `void(^)(BOOL)` completion block. `RcBlock` is heap-allocated and
    // reference-counted, so it outlives the async call below on its own.
    let handler = RcBlock::new(move |granted: Bool| {
        if let Ok(mut slot) = sink.lock() {
            *slot = Some(granted.as_bool());
        }
    });
    let cls = class!(AVCaptureDevice);
    // SAFETY: documented async class method taking an AVMediaType + a
    // `void(^)(BOOL)` completion block; the block outlives the call.
    unsafe {
        let _: () = msg_send![
            cls,
            requestAccessForMediaType: AVMediaTypeVideo,
            completionHandler: &*handler
        ];
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(slot) = answered.lock()
            && let Some(granted) = *slot
        {
            return granted;
        }
        if Instant::now() >= deadline {
            return false;
        }
        run_loop_tick(0.05);
    }
}

/// Ensure the process may use the camera, requesting access if undetermined.
fn ensure_access() -> Result<(), CaptureError> {
    match authorization() {
        Some(true) => Ok(()),
        None if request_access(Duration::from_secs(30)) => Ok(()),
        _ => Err(CaptureError::AccessDenied),
    }
}

/// Whether the process currently holds Camera permission, without prompting.
/// Lets the GUI start a preview only when access is already granted (so it never
/// blocks the UI thread on the permission dialog).
#[must_use]
pub fn camera_access_granted() -> bool {
    matches!(authorization(), Some(true))
}

/// Fire the system Camera-permission prompt without blocking; poll
/// [`camera_authorization`] to observe the outcome.
pub fn request_camera_access() {
    let handler = RcBlock::new(move |_granted: Bool| {});
    let cls = class!(AVCaptureDevice);
    // SAFETY: documented async class method taking an AVMediaType + a
    // `void(^)(BOOL)` completion block; AVFoundation copies the heap block, so
    // it stays valid after `handler` drops.
    unsafe {
        let _: () = msg_send![
            cls,
            requestAccessForMediaType: AVMediaTypeVideo,
            completionHandler: &*handler
        ];
    }
}

/// Current Camera permission as a tri-state, without prompting. Lets the
/// Settings window distinguish Denied from not-yet-asked.
#[must_use]
pub fn camera_authorization() -> crate::CameraAuthorization {
    match authorization() {
        Some(true) => crate::CameraAuthorization::Granted,
        Some(false) => crate::CameraAuthorization::Denied,
        None => crate::CameraAuthorization::Undetermined,
    }
}

/// Pump the current thread's run loop briefly so AVFoundation callbacks fire.
fn run_loop_tick(seconds: f64) {
    // SAFETY: `kCFRunLoopDefaultMode` is a valid mode constant; the call returns
    // after `seconds` or the first handled source (`0` = don't return early).
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
    }
}

fn device_with_unique_id(unique_id: &str) -> Option<Retained<AnyObject>> {
    let cls = class!(AVCaptureDevice);
    let Ok(ns) = CString::new(unique_id) else {
        return None;
    };
    // SAFETY: building an autoreleased NSString from a valid C string, then a
    // `deviceWithUniqueID:` lookup; the autoreleased (+0) result is retained into
    // an owned `Retained` (`None` when the lookup returns nil).
    unsafe {
        let nsstr: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: ns.as_ptr()];
        let device: *mut AnyObject = msg_send![cls, deviceWithUniqueID: nsstr];
        Retained::retain(device)
    }
}

/// A running capture session. Frames flow to the delegate on a background
/// dispatch queue and land in this session's own [`FrameSink`]; dropping the
/// session stops it. The `Retained` fields keep the output + delegate alive
/// for the session's life (the session references them, but we hold owning
/// handles for clarity).
struct Session {
    handle: Retained<AnyObject>,
    _output: Retained<AnyObject>,
    _delegate: Retained<FrameDelegate>,
    sink: Arc<FrameSink>,
    /// The device's configuration lock, held for as long as the session runs
    /// when a stream mode was set (see [`DeviceLock`]). It is a field, so it
    /// is released *after* [`Drop for Session`](Session) has stopped the
    /// session: the format the camera is left at is the one the stream used.
    _format_lock: Option<DeviceLock>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is a valid, retained AVCaptureSession.
        unsafe {
            let _: () = msg_send![&*self.handle, stopRunning];
        }
    }
}

/// The exclusive configuration lock on a capture device
/// (`lockForConfiguration:`), released on drop.
///
/// A session's own `startRunning` resets a device it does not hold the lock on
/// to the session preset's format (1080p at 24 fps for the default `High`),
/// discarding the format and frame duration set a moment earlier. Measured on
/// a StreamCam: a 1080p / 60 fps request delivered 24 fps at 1080p, and a 720p
/// request delivered 1080p, until the lock was kept for the whole session, after
/// which every requested mode arrived exactly (59.7 and 5.0 fps at both sizes).
/// So whoever sets a stream mode keeps this guard until the stream ends.
///
/// The cost is what Apple documents for holding it: no other process can change
/// the camera's format meanwhile, and only for as long as the preview is open.
struct DeviceLock {
    device: Retained<AnyObject>,
}

impl DeviceLock {
    /// Take the lock on `device`, which fails when another process (or an
    /// earlier session of this one) holds it.
    fn acquire(device: &Retained<AnyObject>) -> Result<Self, CaptureError> {
        let mut error: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: documented selector on a valid `AVCaptureDevice`; `error` is
        // only written to and never read.
        let locked: bool = unsafe { msg_send![&**device, lockForConfiguration: &mut error] };
        if locked {
            Ok(Self {
                device: Retained::clone(device),
            })
        } else {
            Err(CaptureError::Setup("device configuration lock".into()))
        }
    }
}

impl Drop for DeviceLock {
    fn drop(&mut self) {
        // SAFETY: this guard took the lock in `acquire` and releases it once.
        unsafe {
            let _: () = msg_send![&*self.device, unlockForConfiguration];
        }
    }
}

/// What a session is opened for.
#[derive(Clone, Copy)]
enum Purpose {
    /// One full-resolution frame, at whatever the camera streams by default.
    Snapshot,
    /// The live preview: mirrored. It opens at the given mode when the camera
    /// lists any, with `None` asking for the camera's default; a camera that
    /// lists none keeps the 720p preset, with frames capped at its width.
    Preview(Option<StreamMode>),
}

/// The modes the camera with `unique_id` can stream at.
///
/// Reads the device's format list only — no capture session, so no Camera
/// permission. A camera whose formats name no frame rate lists nothing, which
/// callers read as "nothing to choose".
///
/// # Errors
/// [`CaptureError::NotFound`] when no camera has that id.
pub fn stream_modes(unique_id: &str) -> Result<StreamModes, CaptureError> {
    // An explicit pool: the device and its format list are autoreleased, and a
    // caller thread with no run loop drains none on its own.
    autoreleasepool(|_| {
        let device = device_with_unique_id(unique_id).ok_or(CaptureError::NotFound)?;
        Ok(formats::offered(&crate::macos::device_formats(&device)))
    })
}

/// Make `selection` the device's active format and frame duration, and return
/// the configuration lock that keeps it so.
///
/// `device` must already be an input of `session`: the properties are set
/// together between the session's `beginConfiguration` and `commitConfiguration`,
/// which is how AVFoundation documents changing them. The returned lock must
/// outlive the session's `startRunning` (see [`DeviceLock`]): without it the
/// session puts the device back on its preset's format.
/// (`AVCaptureSessionPresetInputPriority`, the preset that would say otherwise,
/// does not exist on macOS, and setting the format on an attached device is
/// not enough on its own, as measured.)
fn activate_format(
    session: &AnyObject,
    device: &Retained<AnyObject>,
    selection: &Selection,
) -> Result<DeviceLock, CaptureError> {
    // SAFETY: documented selectors on a valid session; every `begin` is paired
    // with a `commit` on all paths, including the error return of the closure.
    unsafe {
        let _: () = msg_send![session, beginConfiguration];
        let result = DeviceLock::acquire(device).and_then(|lock| {
            set_format(device, selection, &lock)?;
            Ok(lock)
        });
        let _: () = msg_send![session, commitConfiguration];
        result
    }
}

/// Write `selection`'s format and frame duration to `device`, which `_lock`
/// proves this caller has locked for configuration.
fn set_format(
    device: &AnyObject,
    selection: &Selection,
    _lock: &DeviceLock,
) -> Result<(), CaptureError> {
    // A duration outside the active format's range raises an
    // `NSInvalidArgumentException`, which Rust cannot unwind through: the
    // durations below come from the format's own range or from a rate inside it
    // (see `formats::FrameTiming`), and every index is checked against the live
    // arrays first.
    let stale = || CaptureError::Setup("camera formats changed".into());
    // SAFETY: documented selectors on a valid `AVCaptureDevice`, the
    // `AVCaptureDeviceFormat` and `AVFrameRateRange` it lists (autoreleased, and
    // used before returning), and CMTime values of the exact C layout.
    unsafe {
        let formats: *mut AnyObject = msg_send![device, formats];
        let format_count: usize = if formats.is_null() {
            0
        } else {
            msg_send![formats, count]
        };
        if selection.format >= format_count {
            return Err(stale());
        }
        let format: *mut AnyObject = msg_send![formats, objectAtIndex: selection.format];

        let ranges: *mut AnyObject = msg_send![format, videoSupportedFrameRateRanges];
        let range_count: usize = if ranges.is_null() {
            0
        } else {
            msg_send![ranges, count]
        };
        if selection.range >= range_count {
            return Err(stale());
        }
        let range: *mut AnyObject = msg_send![ranges, objectAtIndex: selection.range];

        let (min_duration, max_duration): (CMTime, CMTime) = match selection.timing {
            FrameTiming::Reported => (
                msg_send![range, minFrameDuration],
                msg_send![range, maxFrameDuration],
            ),
            FrameTiming::Rate(fps) => {
                let timescale = i32::try_from(fps).map_err(|_| stale())?;
                let duration = CMTimeMake(1, timescale);
                (duration, duration)
            }
        };

        let _: () = msg_send![device, setActiveFormat: format];
        let _: () = msg_send![device, setActiveVideoMinFrameDuration: min_duration];
        let _: () = msg_send![device, setActiveVideoMaxFrameDuration: max_duration];
    }
    Ok(())
}

/// Point a preview at `requested`'s frame size and rate (or the camera's
/// default), or at the fixed 720p preset when the camera lists no modes.
///
/// 720p is sharp on a Retina-scale preview (the 480pt box is 960 physical
/// pixels wide) while still a fraction of the native 1080p per-frame copy and
/// texture upload.
fn set_preview_format(
    session: &AnyObject,
    device: &Retained<AnyObject>,
    requested: Option<StreamMode>,
) -> Result<PreviewFormat, CaptureError> {
    let listed = crate::macos::device_formats(device);
    if let Some(selection) = formats::choose(&listed, requested) {
        tracing::debug!(mode = %selection.mode, "camera preview mode");
        return Ok(PreviewFormat {
            lock: Some(activate_format(session, device, &selection)?),
            // The size the user picked reaches the preview as it is: a stride
            // decimation of a 1080p frame would hand over 960x540, which is
            // less than the 720p it is meant to beat.
            max_width: 0,
        });
    }
    // SAFETY: documented selectors on a valid session; the preset is
    // AVFoundation's exported `NSString` constant.
    unsafe {
        let can: bool = msg_send![session, canSetSessionPreset: AVCaptureSessionPreset1280x720];
        if can {
            let _: () = msg_send![session, setSessionPreset: AVCaptureSessionPreset1280x720];
        }
    }
    Ok(PreviewFormat {
        lock: None,
        // A camera that ignores the preset and streams wider is decimated down.
        max_width: PRESET_PREVIEW_WIDTH,
    })
}

/// What [`set_preview_format`] leaves a preview session with.
struct PreviewFormat {
    /// The configuration lock a chosen mode needs held while the session runs
    /// (see [`DeviceLock`]); the preset fallback needs none.
    lock: Option<DeviceLock>,
    /// The widest frame the preview keeps, in pixels (0 = as delivered).
    max_width: u32,
}

/// The preview's frame width under the fixed 720p preset.
const PRESET_PREVIEW_WIDTH: u32 = 1280;

/// Authorize, wire up, and start a capture session on `unique_id`. Frames
/// begin arriving in the returned session's [`FrameSink`] shortly after this
/// returns.
fn open_session(unique_id: &str, purpose: Purpose) -> Result<Session, CaptureError> {
    ensure_access()?;
    let device = device_with_unique_id(unique_id).ok_or(CaptureError::NotFound)?;
    let preview = matches!(purpose, Purpose::Preview(_));
    // Frames are kept as delivered until the preview's format says otherwise
    // (below), which is also what a snapshot keeps.
    let sink = Arc::new(FrameSink {
        latest: Mutex::new(None),
        generation: AtomicU64::new(0),
        target_width: AtomicU32::new(0),
    });

    // SAFETY: standard AVCaptureSession wiring with documented selectors; every
    // object added is retained by the session, and the session is stopped on Drop.
    unsafe {
        let session: *mut AnyObject = msg_send![class!(AVCaptureSession), new];
        let Some(session) = Retained::from_raw(session) else {
            return Err(CaptureError::Setup("AVCaptureSession".into()));
        };

        let mut err: *mut AnyObject = std::ptr::null_mut();
        let input: *mut AnyObject = msg_send![
            class!(AVCaptureDeviceInput),
            deviceInputWithDevice: &*device,
            error: &mut err
        ];
        if input.is_null() {
            return Err(CaptureError::Setup("AVCaptureDeviceInput".into()));
        }
        let can_in: bool = msg_send![&*session, canAddInput: input];
        if !can_in {
            return Err(CaptureError::Setup("session rejected input".into()));
        }
        let _: () = msg_send![&*session, addInput: input];

        // After `addInput`: the device must belong to the session for the
        // format set here to be applied. The lock it returns is kept for the
        // session's life (see `DeviceLock`); the early `?` returns below drop
        // it, which releases the device.
        let format_lock = match purpose {
            Purpose::Preview(requested) => {
                let format = set_preview_format(&session, &device, requested)?;
                sink.target_width.store(format.max_width, Ordering::Relaxed);
                format.lock
            }
            Purpose::Snapshot => None,
        };

        let output: *mut AnyObject = msg_send![class!(AVCaptureVideoDataOutput), new];
        let Some(output) = Retained::from_raw(output) else {
            return Err(CaptureError::Setup("AVCaptureVideoDataOutput".into()));
        };
        let num: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithUnsignedInt: PIXEL_FORMAT_32BGRA];
        let settings: *mut AnyObject = msg_send![
            class!(NSDictionary),
            dictionaryWithObject: num,
            forKey: kCVPixelBufferPixelFormatTypeKey
        ];
        let _: () = msg_send![&*output, setVideoSettings: settings];
        let _: () = msg_send![&*output, setAlwaysDiscardsLateVideoFrames: true];

        let delegate = FrameDelegate::new(Arc::clone(&sink));
        let queue = dispatch_queue_create(c"org.openlogi.camera".as_ptr(), std::ptr::null());
        let _: () = msg_send![&*output, setSampleBufferDelegate: &*delegate, queue: queue];

        let can_out: bool = msg_send![&*session, canAddOutput: &*output];
        if !can_out {
            return Err(CaptureError::Setup("session rejected output".into()));
        }
        let _: () = msg_send![&*session, addOutput: &*output];

        // Selfie-mirror the live preview (not snapshots): a webcam self-view is
        // expected to read like a mirror. The driver flips on the connection, so
        // it costs zero per-frame CPU and never alters the outbound camera feed.
        if preview {
            let conn: *mut AnyObject =
                msg_send![&*output, connectionWithMediaType: AVMediaTypeVideo];
            if !conn.is_null() {
                let supported: bool = msg_send![conn, isVideoMirroringSupported];
                if supported {
                    let _: () = msg_send![conn, setAutomaticallyAdjustsVideoMirroring: false];
                    let _: () = msg_send![conn, setVideoMirrored: true];
                }
            }
        }

        let _: () = msg_send![&*session, startRunning];

        Ok(Session {
            handle: session,
            _output: output,
            _delegate: delegate,
            sink,
            _format_lock: format_lock,
        })
    }
}

/// Capture a single full-resolution [`Frame`] (BGRA) from the camera with
/// `unique_id`.
///
/// # Errors
/// [`CaptureError::AccessDenied`] when Camera permission isn't (and can't be)
/// granted, [`CaptureError::NotFound`] for an unknown id, [`CaptureError::Timeout`]
/// when no frame arrives, or [`CaptureError::Setup`] on AVFoundation errors.
pub fn capture_frame(unique_id: &str, timeout: Duration) -> Result<Frame, CaptureError> {
    let session = open_session(unique_id, Purpose::Snapshot)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut slot) = session.sink.latest.lock()
            && let Some(frame) = slot.take()
        {
            return Ok(Arc::unwrap_or_clone(frame));
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::Timeout);
        }
        run_loop_tick(0.03);
    }
}

/// A live preview stream. Holds the session open; [`CameraStream::latest_frame`]
/// returns the most recent frame each time it's polled. Dropping it stops the
/// camera.
pub struct CameraStream {
    session: Session,
}

impl CameraStream {
    /// The most recently delivered frame, or `None` before the first arrives.
    /// Returns a shared [`Arc`] so polling at preview rate never copies the
    /// pixel buffer.
    #[must_use]
    pub fn latest_frame(&self) -> Option<Arc<Frame>> {
        self.session
            .sink
            .latest
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Take the most recent frame out of the slot (the next delivered frame
    /// refills it). A sole consumer that unwraps the [`Arc`] gets the pixel
    /// buffer without copying it.
    #[must_use]
    pub fn take_frame(&self) -> Option<Arc<Frame>> {
        self.session
            .sink
            .latest
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// A counter that increments on every delivered frame, so the preview can
    /// skip rebuilding its texture when no new frame has arrived.
    #[must_use]
    pub fn frame_generation(&self) -> u64 {
        self.session.sink.generation.load(Ordering::Relaxed)
    }
}

/// Start a live capture stream on the camera with `unique_id`.
///
/// The stream opens at `mode` when the camera [lists](stream_modes) it, and
/// otherwise at the camera's default mode (the one nearest 1280×720 at 30 fps);
/// `None` asks for that default. A camera that lists no modes streams at its
/// fixed 720p preset.
///
/// # Errors
/// Same as [`capture_frame`], minus `Timeout` (frames are polled, not awaited);
/// plus [`CaptureError::Setup`] when the camera refuses the chosen format.
pub fn start_stream(
    unique_id: &str,
    mode: Option<StreamMode>,
) -> Result<CameraStream, CaptureError> {
    Ok(CameraStream {
        session: open_session(unique_id, Purpose::Preview(mode))?,
    })
}
