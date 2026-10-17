//! Media Foundation camera capture (Windows): a one-shot snapshot and a live
//! frame stream.
//!
//! A dedicated reader thread owns the whole Media Foundation object graph —
//! device activation, `IMFSourceReader`, format negotiation — and pulls
//! samples synchronously, decoding into the same tightly-packed BGRA
//! [`Frame`]s the macOS backend produces. RGB32 sample memory is BGRX in
//! little-endian byte order — the channel order gpui wants, but with an
//! undefined fourth byte that is forced opaque during the copy.
//! Dropping the [`CameraStream`] stops the thread, which releases the device
//! (camera LED off).
//!
//! There is no per-app consent prompt to drive here: desktop apps see the
//! camera unless the system-wide privacy toggle blocks them, which surfaces
//! as an activation error — reported as [`CaptureError::AccessDenied`].
//!
//! Activation and format negotiation can succeed even when the first sample
//! request will fail. Startup waits for a decoded frame to be stored, not just
//! a successful sample request. This proves initial delivery, not continued
//! stream liveness after startup.

#![expect(
    unsafe_code,
    reason = "Media Foundation COM (device activation + IMFSourceReader sample loop)"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_HW_MFT_FAILED_START_STREAMING,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video,
    MFVideoFormat_NV12, MFVideoFormat_RGB24, MFVideoFormat_RGB32, MFVideoFormat_YUY2,
};
use windows::Win32::System::Com::CoTaskMemFree;

pub use crate::capture_types::{CaptureError, Frame};
use crate::com_windows::{ComApartment, MediaFoundation};

/// The preview's target frame width: matches the macOS backend's 720p preset —
/// Retina-sharp in the 480pt preview box without 1080p copy/upload cost. The
/// native format closest to this width wins.
const TARGET_WIDTH: u32 = 1280;

/// How long [`start_stream`] waits for setup and the first stored frame,
/// including any skipped samples. Retries do not reset this budget.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The latest decoded frame plus its generation counter, shared between the
/// reader thread and the polling preview.
struct Shared {
    latest: Mutex<Option<Arc<Frame>>>,
    generation: AtomicU64,
    stop: AtomicBool,
}

/// Cooperative setup cancellation. Media Foundation's setup calls are
/// synchronous and expose no cancellation handle, so this can stop only at
/// the resource-safe boundaries between them.
struct SetupCancellation<'a> {
    stop: &'a AtomicBool,
}

impl SetupCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn checkpoint(&self) -> Result<(), CaptureError> {
        if self.is_cancelled() {
            Err(CaptureError::Timeout)
        } else {
            Ok(())
        }
    }
}

/// A live preview stream. Holds the reader thread; [`CameraStream::take_frame`]
/// hands out the most recent frame each time it's polled. Dropping it stops
/// the camera.
pub struct CameraStream {
    shared: Arc<Shared>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl CameraStream {
    /// The most recently delivered frame, or `None` before the first arrives.
    #[must_use]
    pub fn latest_frame(&self) -> Option<Arc<Frame>> {
        self.shared.latest.lock().ok().and_then(|slot| slot.clone())
    }

    /// Take the most recent frame out of the slot (the next delivered frame
    /// refills it). A sole consumer that unwraps the [`Arc`] gets the pixel
    /// buffer without copying it.
    #[must_use]
    pub fn take_frame(&self) -> Option<Arc<Frame>> {
        self.shared
            .latest
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// A counter that increments on every delivered frame, so the preview can
    /// skip rebuilding its texture when no new frame has arrived.
    #[must_use]
    pub fn frame_generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }
}

impl Drop for CameraStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // The reader wakes from its blocking ReadSample within a frame
        // interval, sees the flag, and releases the device on its way out.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Start a live capture stream on the camera with `unique_id`.
///
/// Returns only after a decoded frame has been stored. Later read failures
/// can still end the stream; startup does not guarantee continued delivery.
///
/// # Errors
/// [`CaptureError::NotFound`] for an unknown id, [`CaptureError::AccessDenied`]
/// when the system privacy toggle blocks cameras,
/// [`CaptureError::ResourcesUnavailable`] when hardware resources are unavailable,
/// [`CaptureError::Timeout`] when setup and the first frame exceed five seconds,
/// or [`CaptureError::Setup`] on other Media Foundation errors.
pub fn start_stream(unique_id: &str) -> Result<CameraStream, CaptureError> {
    let shared = Arc::new(Shared {
        latest: Mutex::new(None),
        generation: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });
    let (setup_tx, setup_rx) = mpsc::channel();
    let thread_shared = Arc::clone(&shared);
    let id = unique_id.to_string();
    let reader = std::thread::Builder::new()
        .name("openlogi-camera-reader".into())
        .spawn(move || reader_thread(&id, &thread_shared, &setup_tx))
        .map_err(|e| CaptureError::Setup(e.to_string()))?;

    match setup_rx.recv_timeout(SETUP_TIMEOUT) {
        Ok(Ok(())) => Ok(CameraStream {
            shared,
            reader: Some(reader),
        }),
        Ok(Err(e)) => {
            let _ = reader.join();
            Err(e)
        }
        Err(_) => {
            shared.stop.store(true, Ordering::Relaxed);
            Err(CaptureError::Timeout)
        }
    }
}

/// Capture a single [`Frame`] from the camera with `unique_id`.
///
/// # Errors
/// As [`start_stream`], plus [`CaptureError::Timeout`] when no frame arrives.
pub fn capture_frame(unique_id: &str, timeout: Duration) -> Result<Frame, CaptureError> {
    let stream = start_stream(unique_id)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = stream.take_frame() {
            return Ok(Arc::unwrap_or_clone(frame));
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Desktop apps are governed only by the system-wide privacy toggle, which
/// can't be queried up front — report usable and let activation surface a
/// denial.
#[must_use]
pub fn camera_access_granted() -> bool {
    true
}

/// Windows has no per-app camera consent for desktop apps.
#[must_use]
pub fn camera_authorization() -> crate::CameraAuthorization {
    crate::CameraAuthorization::Granted
}

/// No-op: Windows has no consent prompt to trigger for desktop apps.
pub fn request_camera_access() {}

/// The reader thread: builds the Media Foundation graph, reports the outcome
/// through `setup`, then pulls and decodes samples until told to stop.
///
/// This is an ordinary application thread pulling samples synchronously, not
/// one of Media Foundation's work queues — the one thread kind on which the
/// platform calls, and so the guards below, would be illegal.
fn reader_thread(unique_id: &str, shared: &Shared, setup: &mpsc::Sender<Result<(), CaptureError>>) {
    // Declared before anything COM so it drops last: every Media Foundation
    // interface built below is a COM object, and releasing one after its
    // apartment closed is exactly the bug these guards exist to prevent.
    let com = ComApartment::enter();
    let cancellation = SetupCancellation { stop: &shared.stop };
    if let Err(error) = cancellation.checkpoint() {
        let _ = setup.send(Err(error));
        return;
    }
    let media_foundation = match com.start_media_foundation() {
        Ok(started) => started,
        Err(e) => {
            let _ = setup.send(Err(setup_err(e)));
            return;
        }
    };

    // The whole object graph lives in this `match`, so the reader — and with it
    // the media source and every media type — has released by the time the
    // platform guard drops at the end of the function.
    let opened = cancellation
        .checkpoint()
        .and_then(|()| open_reader(&media_foundation, unique_id, &cancellation));
    match opened {
        Ok((reader, stride_hint)) => {
            pump_frames(shared, setup, || read_sample(&reader, shared, stride_hint));
        }
        Err(e) => {
            let _ = setup.send(Err(e));
        }
    }
    drop(media_foundation);
}

/// Report startup only after `read` stores a frame, then keep reading until
/// cancellation or failure. Empty/dropped samples leave startup pending.
fn pump_frames(
    shared: &Shared,
    setup: &mpsc::Sender<Result<(), CaptureError>>,
    mut read: impl FnMut() -> Result<bool, CaptureError>,
) {
    let cancellation = SetupCancellation { stop: &shared.stop };
    let mut pending_setup = Some(setup);
    loop {
        let result = cancellation.checkpoint().and_then(|()| {
            let stored = read();
            // A timeout during a blocking call must win over its late result.
            cancellation.checkpoint()?;
            stored
        });
        match result {
            Ok(false) => {}
            Ok(true) => {
                if let Some(setup) = pending_setup.take()
                    && setup.send(Ok(())).is_err()
                {
                    // The caller timed out and detached. Do not read again.
                    return;
                }
            }
            Err(error) => {
                if let Some(setup) = pending_setup.take() {
                    let _ = setup.send(Err(error));
                }
                return;
            }
        }
    }
}

/// Pull one sample on the reader's owning thread, with Media Foundation still
/// started. `true` means a decoded frame was actually stored, not just read.
fn read_sample(
    reader: &IMFSourceReader,
    shared: &Shared,
    stride_hint: StrideHint,
) -> Result<bool, CaptureError> {
    let cancellation = SetupCancellation { stop: &shared.stop };
    cancellation.checkpoint()?;
    let (mut flags, mut sample) = (0u32, None);
    // SAFETY: synchronous ReadSample with documented out-params, on the thread
    // owning the reader while the platform guard keeps Media Foundation alive.
    let result = unsafe {
        reader.ReadSample(
            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned(),
            0,
            None,
            Some(&raw mut flags),
            None,
            Some(&raw mut sample),
        )
    };
    cancellation.checkpoint()?;
    result.map_err(|error| capture_err(&error))?;
    store_sample(shared, flags, sample, stride_hint)
}

/// Interpret the reader's flags before touching its sample, then decode and
/// publish it. A null sample or malformed buffer is skipped, not readiness.
fn store_sample(
    shared: &Shared,
    flags: u32,
    sample: Option<IMFSample>,
    stride_hint: StrideHint,
) -> Result<bool, CaptureError> {
    if flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() != 0 {
        // The SDK forbids further source-reader calls after this flag.
        return Err(setup_err("camera source reader reported an error"));
    }
    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
        return Err(setup_err("camera stream ended"));
    }
    let Some(sample) = sample else {
        return Ok(false);
    };
    let cancellation = SetupCancellation { stop: &shared.stop };
    cancellation.checkpoint()?;
    // SAFETY: `sample` is owned and live on the reader's thread.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() };
    cancellation.checkpoint()?;
    let buffer = buffer.map_err(|error| capture_err(&error))?;
    let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
    // SAFETY: the owned buffer fills the pointer and its readable length. A
    // successful lock is balanced below, even if cancellation or storage fails.
    let locked = unsafe { buffer.Lock(&raw mut data, None, Some(&raw mut len)) };
    if let Err(error) = locked {
        cancellation.checkpoint()?;
        return Err(capture_err(&error));
    }
    let stored = cancellation
        .checkpoint()
        .and_then(|()| store_frame(shared, data, len as usize, stride_hint));
    // SAFETY: balances the successful Lock above, after all reads from `data`.
    let unlocked = unsafe { buffer.Unlock() };
    cancellation.checkpoint()?;
    let stored = stored?;
    unlocked.map_err(|error| capture_err(&error))?;
    Ok(stored)
}

/// Frame geometry negotiated at setup: dimensions plus the RGB32 stride (a
/// negative stride means the rows arrive bottom-up and must be flipped).
#[derive(Clone, Copy)]
struct StrideHint {
    width: u32,
    height: u32,
    stride: i32,
}

/// Build the source reader for `unique_id`: activate the matching device,
/// pick the native format closest to [`TARGET_WIDTH`], and negotiate RGB32
/// output (Media Foundation inserts the decoder/converter).
fn open_reader(
    platform: &MediaFoundation<'_>,
    unique_id: &str,
    cancellation: &SetupCancellation<'_>,
) -> Result<(IMFSourceReader, StrideHint), CaptureError> {
    // SAFETY: the `platform` borrow is the precondition every call below needs
    // — Media Foundation started, inside an apartment — and the caller holds
    // both guards for longer than the returned reader.
    // `MFCreateAttributes` writes its +1 `IMFAttributes` into the local
    // `Option`, which is checked before use; that attribute set, the reader, the
    // media types the enumeration keeps and the output type are all refcounted
    // wrappers released when they drop. Each method call runs on this thread
    // against a live interface, and the reader is handed back to the very thread
    // that will pull samples from it.
    unsafe {
        let mut reader_attrs = None;
        MFCreateAttributes(&raw mut reader_attrs, 1).map_err(setup_err)?;
        let reader_attrs = reader_attrs.ok_or_else(|| setup_err("MFCreateAttributes"))?;
        reader_attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(setup_err)?;
        cancellation.checkpoint()?;

        let source = activate_source(platform, unique_id, cancellation)?;
        if let Err(error) = cancellation.checkpoint() {
            shutdown_source(&source);
            return Err(error);
        }
        let reader = match MFCreateSourceReaderFromMediaSource(&source, &reader_attrs) {
            Ok(reader) => reader,
            Err(error) => {
                // No source reader exists to perform its documented source
                // shutdown on drop, so balance the successful activation here.
                shutdown_source(&source);
                return Err(capture_err(&error));
            }
        };
        cancellation.checkpoint()?;

        // Prefer the native type closest to the preview's target width, so a
        // 4K-capable camera doesn't stream (and we don't convert) 8x the
        // pixels the preview can show. Only formats the reader's (legacy)
        // processor can convert to RGB32 count — a compressed 720p mode it
        // can't decode would fail below, while a convertible mode at another
        // 16:9 size still previews fine.
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
        let mut best: Option<(u32, IMFMediaType)> = None;
        let mut index = 0u32;
        loop {
            cancellation.checkpoint()?;
            let Ok(native) = reader.GetNativeMediaType(stream, index) else {
                break;
            };
            index += 1;
            let convertible = native.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|subtype| {
                [
                    MFVideoFormat_NV12,
                    MFVideoFormat_YUY2,
                    MFVideoFormat_RGB24,
                    MFVideoFormat_RGB32,
                ]
                .contains(&subtype)
            });
            if !convertible {
                continue;
            }
            if let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE) {
                let width = (size >> 32) as u32;
                let score = width.abs_diff(TARGET_WIDTH);
                if best.as_ref().is_none_or(|(s, _)| score < *s) {
                    best = Some((score, native));
                }
            }
        }
        cancellation.checkpoint()?;
        // Selecting the native type switches the device to that mode — a size
        // hint on the RGB32 output type alone is quietly dropped (the legacy
        // processor converts but never scales), leaving whatever mode the
        // device was in.
        if let Some((_, native)) = &best {
            reader
                .SetCurrentMediaType(stream, None, native)
                .map_err(setup_err)?;
            cancellation.checkpoint()?;
        }

        let output = MFCreateMediaType().map_err(setup_err)?;
        output
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(setup_err)?;
        output
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(setup_err)?;
        reader
            .SetCurrentMediaType(stream, None, &output)
            .map_err(setup_err)?;
        cancellation.checkpoint()?;

        // Read the negotiated geometry back — the converter may have kept the
        // native size, and the stride tells us whether rows arrive bottom-up.
        let current = reader.GetCurrentMediaType(stream).map_err(setup_err)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(setup_err)?;
        let width = (size >> 32) as u32;
        let height = (size & 0xFFFF_FFFF) as u32;
        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or_else(|_| width.cast_signed() * 4, u32::cast_signed);
        cancellation.checkpoint()?;
        Ok((
            reader,
            StrideHint {
                width,
                height,
                stride,
            },
        ))
    }
}

/// Activate the video-capture device whose Media Foundation symbolic link
/// identifies the same physical device as `unique_id` (the stored DirectShow
/// device path). The two APIs register the camera under different
/// interface-class GUIDs, so they are matched on the shared device-instance
/// portion (see [`device_instance`]).
fn activate_source(
    _platform: &MediaFoundation<'_>,
    unique_id: &str,
    cancellation: &SetupCancellation<'_>,
) -> Result<IMFMediaSource, CaptureError> {
    // SAFETY: the `_platform` borrow proves Media Foundation is started inside
    // an apartment on this thread, which is what these MF calls require of it.
    // `MFEnumDeviceSources` fills `devices` with one CoTaskMem
    // allocation holding exactly `count` nullable interface pointers —
    // `Option<IMFActivate>` is a pointer-sized niche over that — so the slice
    // spans only memory MF allocated, and the null case is ruled out before it
    // is built. Every element is moved out with `take`, which is what carries
    // MF's reference into Rust's ownership, and the array itself is freed once,
    // after the borrow ends. `GetAllocatedString` hands back a CoTaskMem PWSTR
    // that is copied into `link_str` before being freed, so nothing outlives
    // its allocation.
    unsafe {
        let mut enum_attrs = None;
        MFCreateAttributes(&raw mut enum_attrs, 1).map_err(setup_err)?;
        let enum_attrs = enum_attrs.ok_or_else(|| setup_err("MFCreateAttributes"))?;
        enum_attrs
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .map_err(setup_err)?;
        cancellation.checkpoint()?;

        let (mut devices, mut count) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
        MFEnumDeviceSources(&enum_attrs, &raw mut devices, &raw mut count).map_err(setup_err)?;
        if devices.is_null() {
            return Err(CaptureError::NotFound);
        }
        let list = std::slice::from_raw_parts_mut(devices, count as usize);
        let mut chosen = None;
        for slot in &mut *list {
            // MF hands the caller one reference per device and freeing the
            // array releases none of them, so every activate is moved out —
            // the ones that are not chosen are released when they drop below.
            let Some(activate) = slot.take() else {
                continue;
            };
            if chosen.is_some() || cancellation.is_cancelled() {
                continue;
            }
            let (mut link, mut len) = (windows::core::PWSTR::null(), 0u32);
            if activate
                .GetAllocatedString(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    &raw mut link,
                    &raw mut len,
                )
                .is_err()
            {
                continue;
            }
            let link_str = link.to_string().unwrap_or_default();
            CoTaskMemFree(Some(link.as_ptr().cast()));
            if device_instance(&link_str).eq_ignore_ascii_case(device_instance(unique_id)) {
                chosen = Some(activate);
            }
        }
        let result = match cancellation.checkpoint() {
            Err(error) => Err(error),
            Ok(()) => match chosen {
                // ActivateObject is synchronous and provides no cancellation
                // handle. A timeout while the driver is inside this call can
                // only be observed after it returns; `open_reader` then shuts
                // the returned source down before doing any further setup.
                Some(activate) => activate
                    .ActivateObject::<IMFMediaSource>()
                    .map_err(|e| capture_err(&e)),
                None => Err(CaptureError::NotFound),
            },
        };
        CoTaskMemFree(Some(devices.cast()));
        result
    }
}

/// Shut down an activated source when no source reader exists to own that
/// responsibility. The COM wrapper still drops afterward, before the Media
/// Foundation and apartment guards in [`reader_thread`].
fn shutdown_source(source: &IMFMediaSource) {
    // SAFETY: `source` is a live media source activated on this thread. This
    // call is the documented teardown for an activated media source that was
    // not transferred into an IMFSourceReader.
    if let Err(error) = unsafe { source.Shutdown() } {
        tracing::warn!(%error, "could not shut down cancelled camera source");
    }
}

/// The device-instance portion of a Windows device-interface path, dropping the
/// trailing `#{interface-class-guid}\reference`. DirectShow (the id we enumerate
/// and persist) tags a camera under `KSCATEGORY_VIDEO`, while Media Foundation
/// tags the same physical device under `KSCATEGORY_VIDEO_CAMERA` — so the paths
/// differ only by that GUID, and comparing the instance links the two.
fn device_instance(interface_path: &str) -> &str {
    interface_path.split("#{").next().unwrap_or(interface_path)
}

/// Copy one locked RGB32 sample into a tightly-packed BGRA [`Frame`] in the
/// shared slot, flipping bottom-up rows when the stride is negative. Returns
/// `false` for a malformed buffer; only a published frame returns `true`.
fn store_frame(
    shared: &Shared,
    data: *mut u8,
    len: usize,
    hint: StrideHint,
) -> Result<bool, CaptureError> {
    let (width, height) = (hint.width as usize, hint.height as usize);
    let row_bytes = width * 4;
    let stride = hint.stride.unsigned_abs() as usize;
    if width == 0
        || height == 0
        || data.is_null()
        || stride < row_bytes
        || stride * (height - 1) + row_bytes > len
    {
        return Ok(false);
    }
    let mut bgra = vec![0u8; row_bytes * height];
    for y in 0..height {
        // A negative stride means the buffer's first row is the bottom line.
        let src_row = if hint.stride < 0 { height - 1 - y } else { y };
        // SAFETY: both row offsets are bounds-checked against `len` above.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.add(src_row * stride),
                bgra.as_mut_ptr().add(y * row_bytes),
                row_bytes,
            );
        }
    }
    // RGB32 is really BGRX: Media Foundation leaves the fourth byte undefined
    // (zero in practice), which gpui would alpha-blend into an invisible frame.
    // Force every pixel opaque to make the buffer true BGRA.
    for px in bgra.as_chunks_mut::<4>().0 {
        px[3] = 0xFF;
    }
    let mut slot = shared
        .latest
        .lock()
        .map_err(|_| setup_err("camera frame slot is poisoned"))?;
    *slot = Some(Arc::new(Frame {
        width: hint.width,
        height: hint.height,
        bgra,
    }));
    shared.generation.fetch_add(1, Ordering::Relaxed);
    Ok(true)
}

fn setup_err(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Setup(e.to_string())
}

/// Preserve known causes at activation and sample time. Hardware-resource
/// exhaustion does not prove that another application owns the camera.
fn capture_err(e: &windows::core::Error) -> CaptureError {
    const E_ACCESSDENIED: windows::core::HRESULT =
        windows::core::HRESULT(0x8007_0005_u32.cast_signed());
    match e.code() {
        E_ACCESSDENIED => CaptureError::AccessDenied,
        MF_E_HW_MFT_FAILED_START_STREAMING => CaptureError::ResourcesUnavailable,
        _ => setup_err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    use windows::Win32::Foundation::{E_ACCESSDENIED, E_OUTOFMEMORY};
    use windows::Win32::Media::MediaFoundation::{
        MF_E_HW_MFT_FAILED_START_STREAMING, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
        MF_SOURCE_READERF_STREAMTICK,
    };

    use super::{
        CaptureError, SetupCancellation, Shared, StrideHint, capture_err, device_instance,
        pump_frames, setup_err, store_frame, store_sample,
    };

    const HINT: StrideHint = StrideHint {
        width: 2,
        height: 2,
        stride: -12,
    };
    // Two distinct BGRX rows, separated by padding. The final row needs no
    // trailing padding, so 20 bytes is exactly sufficient (19 is too short).
    const PIXELS: [u8; 20] = [
        1, 2, 3, 0, 4, 5, 6, 0, 99, 98, 97, 96, 7, 8, 9, 0, 10, 11, 12, 0,
    ];

    fn shared() -> Shared {
        Shared {
            latest: Mutex::new(None),
            generation: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    #[test]
    fn startup_waits_for_a_stored_frame_and_acknowledges_only_once() {
        let shared = shared();
        let (tx, rx) = mpsc::channel();
        let mut pixels = PIXELS;
        let mut reads = 0;
        pump_frames(&shared, &tx, || {
            reads += 1;
            if reads <= 5 {
                assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
                assert!(shared.latest.lock().unwrap().is_none());
                assert_eq!(shared.generation.load(Ordering::Relaxed), 0);
            }
            match reads {
                1 => store_sample(&shared, 0, None, HINT),
                2 => store_sample(
                    &shared,
                    MF_SOURCE_READERF_STREAMTICK.0.cast_unsigned(),
                    None,
                    HINT,
                ),
                3 => store_frame(&shared, std::ptr::null_mut(), pixels.len(), HINT),
                4 => store_frame(&shared, pixels.as_mut_ptr(), 19, HINT),
                5 => store_frame(&shared, pixels.as_mut_ptr(), pixels.len(), HINT),
                6 => {
                    rx.try_recv().unwrap().unwrap();
                    let slot = shared.latest.lock().unwrap();
                    let frame = slot.as_ref().unwrap();
                    assert_eq!((frame.width, frame.height), (2, 2));
                    assert_eq!(
                        frame.bgra,
                        [7, 8, 9, 255, 10, 11, 12, 255, 1, 2, 3, 255, 4, 5, 6, 255]
                    );
                    assert_eq!(shared.generation.load(Ordering::Relaxed), 1);
                    drop(slot);
                    store_frame(&shared, pixels.as_mut_ptr(), pixels.len(), HINT)
                }
                7 => Err(setup_err("later read failed")),
                _ => panic!("read again after a terminal error"),
            }
        });
        assert_eq!(reads, 7);
        assert_eq!(shared.generation.load(Ordering::Relaxed), 2);
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn reader_failure_and_eof_flags_end_startup_even_with_other_bits() {
        let error = MF_SOURCE_READERF_ERROR.0.cast_unsigned();
        let eof = MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned();
        let tick = MF_SOURCE_READERF_STREAMTICK.0.cast_unsigned();
        for (flags, expected) in [
            (error, "camera source reader reported an error"),
            (error | tick | eof, "camera source reader reported an error"),
            (eof, "camera stream ended"),
            (eof | tick, "camera stream ended"),
        ] {
            let shared = shared();
            let (tx, rx) = mpsc::channel();
            let mut reads = 0;
            pump_frames(&shared, &tx, || {
                reads += 1;
                assert_eq!(reads, 1, "terminal flags must prevent another read");
                store_sample(&shared, flags, None, HINT)
            });
            assert!(matches!(
                rx.try_recv().unwrap(),
                Err(CaptureError::Setup(message)) if message == expected
            ));
            assert!(shared.latest.lock().unwrap().is_none());
            assert_eq!(shared.generation.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn cancellation_before_startup_prevents_reading() {
        let shared = shared();
        shared.stop.store(true, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        pump_frames(&shared, &tx, || panic!("read after cancellation"));
        assert!(matches!(rx.try_recv().unwrap(), Err(CaptureError::Timeout)));
    }

    #[test]
    fn cancellation_during_blocking_read_rejects_even_a_late_stored_frame() {
        let shared = shared();
        let (tx, rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                pump_frames(shared, &tx, || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    let mut pixels = PIXELS;
                    store_frame(shared, pixels.as_mut_ptr(), pixels.len(), HINT)
                });
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            shared.stop.store(true, Ordering::Relaxed);
            release_tx.send(()).unwrap();
            assert!(matches!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                Err(CaptureError::Timeout)
            ));
        });
        assert_eq!(shared.generation.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn skipped_samples_remain_pending_until_cancellation() {
        let shared = shared();
        let (tx, rx) = mpsc::channel();
        let mut reads = 0;
        pump_frames(&shared, &tx, || {
            reads += 1;
            assert!(reads <= 3, "read again after cancellation");
            assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
            if reads == 3 {
                shared.stop.store(true, Ordering::Relaxed);
            }
            Ok(false)
        });
        assert_eq!(reads, 3);
        assert!(matches!(rx.try_recv().unwrap(), Err(CaptureError::Timeout)));
        assert!(shared.latest.lock().unwrap().is_none());
    }

    #[test]
    fn disconnected_startup_caller_prevents_another_read() {
        let shared = shared();
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let mut reads = 0;
        pump_frames(&shared, &tx, || {
            reads += 1;
            assert_eq!(reads, 1);
            let mut pixels = PIXELS;
            store_frame(&shared, pixels.as_mut_ptr(), pixels.len(), HINT)
        });
        assert_eq!(reads, 1);
    }

    #[test]
    fn poisoned_frame_slot_fails_startup_instead_of_claiming_readiness() {
        let shared = shared();
        let _ = std::panic::catch_unwind(|| {
            let _slot = shared.latest.lock().unwrap();
            panic!("poison the frame slot");
        });
        let (tx, rx) = mpsc::channel();
        let mut reads = 0;
        pump_frames(&shared, &tx, || {
            reads += 1;
            assert_eq!(reads, 1, "a poisoned slot must fail, not skip the frame");
            let mut pixels = PIXELS;
            store_frame(&shared, pixels.as_mut_ptr(), pixels.len(), HINT)
        });
        assert!(matches!(
            rx.try_recv().unwrap(),
            Err(CaptureError::Setup(message)) if message == "camera frame slot is poisoned"
        ));
        assert_eq!(shared.generation.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn sample_errors_preserve_resource_permission_and_other_causes() {
        for code in [
            MF_E_HW_MFT_FAILED_START_STREAMING,
            E_ACCESSDENIED,
            E_OUTOFMEMORY,
        ] {
            let shared = shared();
            let (tx, rx) = mpsc::channel();
            let error = windows::core::Error::from_hresult(code);
            let mut reads = 0;
            pump_frames(&shared, &tx, || {
                reads += 1;
                assert_eq!(reads, 1, "read again after failure");
                Err(capture_err(&error))
            });
            match (code, rx.try_recv().unwrap().unwrap_err()) {
                (MF_E_HW_MFT_FAILED_START_STREAMING, CaptureError::ResourcesUnavailable)
                | (E_ACCESSDENIED, CaptureError::AccessDenied) => {}
                (E_OUTOFMEMORY, CaptureError::Setup(message)) => {
                    assert_eq!(message, error.to_string());
                }
                (_, error) => panic!("incorrect classification for {code:?}: {error}"),
            }
            assert!(shared.latest.lock().unwrap().is_none());
        }
    }

    // The same StreamCam function, as DirectShow enumerates it (KSCATEGORY_VIDEO)
    // vs. as Media Foundation enumerates it (KSCATEGORY_VIDEO_CAMERA): identical
    // but for the trailing interface-class GUID.
    const DIRECTSHOW: &str = r"\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
    const MEDIA_FOUNDATION: &str = r"\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";

    #[test]
    fn setup_stops_at_the_first_checkpoint_after_cancellation() {
        let stop = AtomicBool::new(false);
        let cancellation = SetupCancellation { stop: &stop };
        cancellation
            .checkpoint()
            .expect("setup may continue before cancellation");

        stop.store(true, Ordering::Relaxed);
        assert!(matches!(
            cancellation.checkpoint(),
            Err(CaptureError::Timeout)
        ));
    }

    #[test]
    fn instance_matches_across_interface_class_guids() {
        assert_eq!(
            device_instance(DIRECTSHOW),
            device_instance(MEDIA_FOUNDATION),
            "the stored DirectShow id must match MF's symbolic link"
        );
        assert_eq!(
            device_instance(DIRECTSHOW),
            r"\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000"
        );
    }

    #[test]
    fn distinct_devices_stay_distinct() {
        let other = r"\\?\usb#vid_046d&pid_0825&mi_00#7&1a2b3c&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        assert_ne!(device_instance(DIRECTSHOW), device_instance(other));
    }

    #[test]
    fn path_without_interface_guid_is_returned_whole() {
        assert_eq!(device_instance("not-a-device-path"), "not-a-device-path");
    }
}
