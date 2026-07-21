//! AVFoundation camera capture: a one-shot snapshot and a live frame stream.
//!
//! Both open an `AVCaptureSession` on the chosen camera and read BGRA frames
//! through an `AVCaptureVideoDataOutput` delegate. Frames are kept in gpui's
//! native **BGRA** order so the preview uploads them with no channel swap; the
//! snapshot path swaps to RGBA once when it encodes the PNG. Capturing (unlike
//! enumeration) needs Camera permission *and* an app bundle carrying
//! `NSCameraUsageDescription`; from an unbundled binary macOS denies access,
//! which surfaces as [`CaptureError::AccessDenied`].

#![expect(
    unsafe_code,
    reason = "AVFoundation / CoreMedia / CoreVideo capture FFI"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "pixel dimensions and FourCC constants are bounded and copied verbatim"
)]

use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use block::ConcreteBlock;
use objc::declare::ClassDecl;
use objc::rc::StrongPtr;
use objc::runtime::{BOOL, Class, NO, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

pub use crate::capture_types::{CaptureError, Frame};

/// kCVPixelFormatType_32BGRA ('BGRA').
const PIXEL_FORMAT_32BGRA: u32 = 0x4247_5241;
/// kCVPixelBufferLock_ReadOnly.
const LOCK_READ_ONLY: u64 = 1;

// The most recent frame the delegate decoded, behind an `Arc` so the preview's
// poll hands out a cheap refcount bump instead of copying the whole buffer. A
// process previews one camera at a time, so a single global sink is enough and
// keeps the delegate stateless.
static LATEST: OnceLock<Mutex<Option<Arc<Frame>>>> = OnceLock::new();
fn latest() -> &'static Mutex<Option<Arc<Frame>>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Increments on every delivered frame, so a poller can tell a new frame from a
/// repeat without comparing pixel buffers.
static FRAME_GEN: AtomicU64 = AtomicU64::new(0);

/// Target max width for delegate downsampling (0 = full resolution). Previews
/// set this so an oversized buffer decimates down in one strided pass instead
/// of copying (and uploading) pixels the preview can never show.
static PREVIEW_TARGET_W: AtomicU32 = AtomicU32::new(0);

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeVideo: *const Object;
    static AVCaptureSessionPreset1280x720: *const Object;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *mut Object) -> *mut Object;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const Object;
    fn CVPixelBufferLockBaseAddress(pb: *mut Object, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut Object, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut Object) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pb: *mut Object) -> usize;
    fn CVPixelBufferGetWidth(pb: *mut Object) -> usize;
    fn CVPixelBufferGetHeight(pb: *mut Object) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: BOOL,
    ) -> i32;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut Object;
}

/// Delegate callback: `captureOutput:didOutputSampleBuffer:fromConnection:`.
/// Copies the sample buffer's BGRA pixels (optionally decimated) into [`latest`].
extern "C" fn did_output(
    _this: &Object,
    _sel: Sel,
    _output: *mut Object,
    sbuf: *mut Object,
    _conn: *mut Object,
) {
    // SAFETY: `sbuf` is a valid CMSampleBuffer delivered by AVFoundation; the
    // image buffer is locked for the span of the read and unlocked before return.
    unsafe {
        let pb = CMSampleBufferGetImageBuffer(sbuf);
        if pb.is_null() || CVPixelBufferLockBaseAddress(pb, LOCK_READ_ONLY) != 0 {
            return;
        }
        let base = CVPixelBufferGetBaseAddress(pb).cast::<u8>();
        let bytes_per_row = CVPixelBufferGetBytesPerRow(pb);
        let width = CVPixelBufferGetWidth(pb);
        let height = CVPixelBufferGetHeight(pb);
        let target = PREVIEW_TARGET_W.load(Ordering::Relaxed) as usize;
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
            if let Ok(mut slot) = latest().lock() {
                *slot = Some(Arc::new(Frame {
                    width: out_w as u32,
                    height: out_h as u32,
                    bgra,
                }));
                FRAME_GEN.fetch_add(1, Ordering::Relaxed);
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, LOCK_READ_ONLY);
    }
}

fn delegate_class() -> *const Class {
    static CACHE: OnceLock<usize> = OnceLock::new();
    let ptr = *CACHE.get_or_init(|| {
        let superclass = class!(NSObject);
        match ClassDecl::new("OLCameraFrameDelegate", superclass) {
            Some(mut decl) => {
                // SAFETY: the registered selector matches `did_output`'s ABI
                // (the standard sample-buffer delegate signature).
                unsafe {
                    decl.add_method(
                        sel!(captureOutput:didOutputSampleBuffer:fromConnection:),
                        did_output
                            as extern "C" fn(&Object, Sel, *mut Object, *mut Object, *mut Object),
                    );
                }
                std::ptr::from_ref::<Class>(decl.register()) as usize
            }
            // Already registered (re-entry): look it up.
            None => Class::get("OLCameraFrameDelegate")
                .map_or(std::ptr::null::<Class>() as usize, |c| {
                    std::ptr::from_ref(c) as usize
                }),
        }
    });
    ptr as *const Class
}

/// Current Camera authorization: `Some(true)` usable, `Some(false)` denied,
/// `None` undetermined (caller should request access).
fn authorization() -> Option<bool> {
    let cls = class!(AVCaptureDevice);
    // SAFETY: documented class method returning an AVAuthorizationStatus NSInteger.
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
    let handler = ConcreteBlock::new(move |granted: BOOL| {
        if let Ok(mut slot) = sink.lock() {
            *slot = Some(granted != NO);
        }
    });
    let handler = handler.copy();
    let cls = class!(AVCaptureDevice);
    // SAFETY: documented async class method taking an AVMediaType + a
    // `void(^)(BOOL)` completion block; the block outlives the call below.
    unsafe {
        let _: () = msg_send![cls, requestAccessForMediaType: AVMediaTypeVideo completionHandler: &*handler];
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
    // after `seconds` or the first handled source.
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, NO);
    }
}

fn device_with_unique_id(unique_id: &str) -> Option<StrongPtr> {
    let cls = class!(AVCaptureDevice);
    let Ok(ns) = CString::new(unique_id) else {
        return None;
    };
    // SAFETY: building an autoreleased NSString from a valid C string, then a
    // `deviceWithUniqueID:` lookup; the result is retained into a StrongPtr.
    unsafe {
        let nsstr: *mut Object = msg_send![class!(NSString), stringWithUTF8String: ns.as_ptr()];
        let device: *mut Object = msg_send![cls, deviceWithUniqueID: nsstr];
        if device.is_null() {
            None
        } else {
            Some(StrongPtr::retain(device))
        }
    }
}

/// A running capture session. Frames flow to the delegate on a background
/// dispatch queue and land in [`latest`]; dropping the session stops it.
struct Session {
    handle: StrongPtr,
    _output: StrongPtr,
    _delegate: StrongPtr,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: `self.session` is a valid, retained AVCaptureSession.
        unsafe {
            let _: () = msg_send![*self.handle, stopRunning];
        }
    }
}

/// Authorize, wire up, and start a capture session on `unique_id`. Frames begin
/// arriving in [`latest`] shortly after this returns.
fn open_session(unique_id: &str, low_res: bool) -> Result<Session, CaptureError> {
    ensure_access()?;
    let device = device_with_unique_id(unique_id).ok_or(CaptureError::NotFound)?;
    if let Ok(mut slot) = latest().lock() {
        *slot = None;
    }
    // Previews cap at 720p-wide frames (the preview preset below already
    // delivers exactly that; the decimator only kicks in if a camera ignores
    // the preset and streams wider). Snapshots keep full resolution.
    PREVIEW_TARGET_W.store(if low_res { 1280 } else { 0 }, Ordering::Relaxed);

    // SAFETY: standard AVCaptureSession wiring with documented selectors; every
    // object added is retained by the session, and the session is stopped on Drop.
    unsafe {
        let session: *mut Object = msg_send![class!(AVCaptureSession), new];
        if session.is_null() {
            return Err(CaptureError::Setup("AVCaptureSession".into()));
        }
        let session = StrongPtr::new(session);

        let mut err: *mut Object = std::ptr::null_mut();
        let input: *mut Object = msg_send![
            class!(AVCaptureDeviceInput),
            deviceInputWithDevice: *device error: std::ptr::addr_of_mut!(err)
        ];
        if input.is_null() {
            return Err(CaptureError::Setup("AVCaptureDeviceInput".into()));
        }
        let can_in: BOOL = msg_send![*session, canAddInput: input];
        if can_in == NO {
            return Err(CaptureError::Setup("session rejected input".into()));
        }
        let _: () = msg_send![*session, addInput: input];

        // Preview streams capture at 720p — sharp on a Retina-scale preview
        // (the 480pt box is 960 physical pixels wide) while still a fraction
        // of the native 1080p per-frame copy + texture upload.
        if low_res {
            let can: BOOL =
                msg_send![*session, canSetSessionPreset: AVCaptureSessionPreset1280x720];
            if can != NO {
                let _: () = msg_send![*session, setSessionPreset: AVCaptureSessionPreset1280x720];
            }
        }

        let output: *mut Object = msg_send![class!(AVCaptureVideoDataOutput), new];
        let output = StrongPtr::new(output);
        let num: *mut Object =
            msg_send![class!(NSNumber), numberWithUnsignedInt: PIXEL_FORMAT_32BGRA];
        let settings: *mut Object = msg_send![
            class!(NSDictionary),
            dictionaryWithObject: num forKey: kCVPixelBufferPixelFormatTypeKey
        ];
        let _: () = msg_send![*output, setVideoSettings: settings];
        let _: () = msg_send![*output, setAlwaysDiscardsLateVideoFrames: true];

        let delegate_cls = delegate_class();
        if delegate_cls.is_null() {
            return Err(CaptureError::Setup("delegate class".into()));
        }
        let cls_ref: &Class = &*delegate_cls;
        let delegate: *mut Object = msg_send![cls_ref, new];
        let delegate = StrongPtr::new(delegate);
        let queue = dispatch_queue_create(c"org.openlogi.camera".as_ptr(), std::ptr::null());
        let _: () = msg_send![*output, setSampleBufferDelegate: *delegate queue: queue];

        let can_out: BOOL = msg_send![*session, canAddOutput: *output];
        if can_out == NO {
            return Err(CaptureError::Setup("session rejected output".into()));
        }
        let _: () = msg_send![*session, addOutput: *output];

        // Selfie-mirror the live preview (not snapshots): a webcam self-view is
        // expected to read like a mirror. The driver flips on the connection, so
        // it costs zero per-frame CPU and never alters the outbound camera feed.
        if low_res {
            let conn: *mut Object = msg_send![*output, connectionWithMediaType: AVMediaTypeVideo];
            if !conn.is_null() {
                let supported: BOOL = msg_send![conn, isVideoMirroringSupported];
                if supported != NO {
                    let _: () = msg_send![conn, setAutomaticallyAdjustsVideoMirroring: false];
                    let _: () = msg_send![conn, setVideoMirrored: true];
                }
            }
        }

        let _: () = msg_send![*session, startRunning];

        Ok(Session {
            handle: session,
            _output: output,
            _delegate: delegate,
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
    let _session = open_session(unique_id, false)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut slot) = latest().lock()
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
    _session: Session,
}

impl CameraStream {
    /// The most recently delivered frame, or `None` before the first arrives.
    /// Returns a shared [`Arc`] so polling at preview rate never copies the
    /// pixel buffer.
    #[must_use]
    pub fn latest_frame(&self) -> Option<Arc<Frame>> {
        latest().lock().ok().and_then(|slot| slot.clone())
    }

    /// Take the most recent frame out of the slot (the next delivered frame
    /// refills it). A sole consumer that unwraps the [`Arc`] gets the pixel
    /// buffer without copying it.
    #[must_use]
    pub fn take_frame(&self) -> Option<Arc<Frame>> {
        latest().lock().ok().and_then(|mut slot| slot.take())
    }

    /// A counter that increments on every delivered frame, so the preview can
    /// skip rebuilding its texture when no new frame has arrived.
    #[must_use]
    pub fn frame_generation(&self) -> u64 {
        FRAME_GEN.load(Ordering::Relaxed)
    }
}

/// Start a live capture stream on the camera with `unique_id`.
///
/// # Errors
/// Same as [`capture_frame`], minus `Timeout` (frames are polled, not awaited).
pub fn start_stream(unique_id: &str) -> Result<CameraStream, CaptureError> {
    Ok(CameraStream {
        _session: open_session(unique_id, true)?,
    })
}
