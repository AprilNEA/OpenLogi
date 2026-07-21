//! Media Foundation camera capture (Windows): a one-shot snapshot and a live
//! frame stream.
//!
//! A dedicated reader thread owns the whole Media Foundation object graph —
//! device activation, `IMFSourceReader`, format negotiation — and pulls
//! samples synchronously, decoding into the same tightly-packed BGRA
//! [`Frame`]s the macOS backend produces (RGB32 sample memory is BGRA in
//! little-endian byte order, so gpui uploads them without a channel swap).
//! Dropping the [`CameraStream`] stops the thread, which releases the device
//! (camera LED off).
//!
//! There is no per-app consent prompt to drive here: desktop apps see the
//! camera unless the system-wide privacy toggle blocks them, which surfaces
//! as an activation error — reported as [`CaptureError::AccessDenied`].

#![expect(
    unsafe_code,
    reason = "Media Foundation COM (device activation + IMFSourceReader sample loop)"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "pixel dimensions and strides are bounded and copied verbatim"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaSource, IMFSourceReader, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFSTARTUP_LITE,
    MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};

pub use crate::capture_types::{CaptureError, Frame};

/// The preview's target frame width: matches the macOS backend's 720p preset —
/// Retina-sharp in the 480pt preview box without 1080p copy/upload cost. The
/// native format closest to this width wins.
const TARGET_WIDTH: u32 = 1280;

/// How long [`start_stream`] waits for the reader thread to finish setup.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The latest decoded frame plus its generation counter, shared between the
/// reader thread and the polling preview.
struct Shared {
    latest: Mutex<Option<Arc<Frame>>>,
    generation: AtomicU64,
    stop: AtomicBool,
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
/// # Errors
/// [`CaptureError::NotFound`] for an unknown id, [`CaptureError::AccessDenied`]
/// when the system privacy toggle blocks cameras, or [`CaptureError::Setup`]
/// on Media Foundation errors.
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

/// The reader thread: builds the Media Foundation graph, reports the outcome
/// through `setup`, then pulls and decodes samples until told to stop.
fn reader_thread(unique_id: &str, shared: &Shared, setup: &mpsc::Sender<Result<(), CaptureError>>) {
    // SAFETY: COM + MF init on this thread; every interface is released by the
    // `windows` wrappers when the thread's locals drop.
    let reader = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
            let _ = setup.send(Err(CaptureError::Setup(e.to_string())));
            return;
        }
        match open_reader(unique_id) {
            Ok(opened) => opened,
            Err(e) => {
                let _ = setup.send(Err(e));
                return;
            }
        }
    };
    let (reader, stride_hint) = reader;
    let _ = setup.send(Ok(()));

    while !shared.stop.load(Ordering::Relaxed) {
        // SAFETY: synchronous ReadSample with documented out-params; the
        // sample and its buffer are released when the wrappers drop.
        unsafe {
            let (mut flags, mut sample) = (0u32, None);
            if reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&raw mut flags),
                    None,
                    Some(&raw mut sample),
                )
                .is_err()
            {
                break;
            }
            let Some(sample) = sample else { continue };
            let Ok(buffer) = sample.ConvertToContiguousBuffer() else {
                continue;
            };
            let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
            if buffer
                .Lock(&raw mut data, None, Some(&raw mut len))
                .is_err()
            {
                continue;
            }
            store_frame(shared, data, len as usize, stride_hint);
            let _ = buffer.Unlock();
        }
    }
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
unsafe fn open_reader(unique_id: &str) -> Result<(IMFSourceReader, StrideHint), CaptureError> {
    unsafe {
        let source = activate_source(unique_id)?;

        let mut reader_attrs = None;
        MFCreateAttributes(&raw mut reader_attrs, 1).map_err(setup_err)?;
        let reader_attrs = reader_attrs.ok_or_else(|| setup_err("MFCreateAttributes"))?;
        reader_attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(setup_err)?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &reader_attrs)
            .map_err(|e| access_or_setup(&e))?;

        // Prefer the native type closest to the preview's target width, so a
        // 4K-capable camera doesn't stream (and we don't convert) 8x the
        // pixels the preview can show.
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let mut best: Option<(u32, u64)> = None;
        let mut index = 0u32;
        while let Ok(native) = reader.GetNativeMediaType(stream, index) {
            if let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE) {
                let width = (size >> 32) as u32;
                let score = width.abs_diff(TARGET_WIDTH);
                if best.is_none_or(|(s, _)| score < s) {
                    best = Some((score, size));
                }
            }
            index += 1;
        }

        let output = MFCreateMediaType().map_err(setup_err)?;
        output
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(setup_err)?;
        output
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(setup_err)?;
        if let Some((_, size)) = best {
            output
                .SetUINT64(&MF_MT_FRAME_SIZE, size)
                .map_err(setup_err)?;
        }
        reader
            .SetCurrentMediaType(stream, None, &output)
            .map_err(setup_err)?;

        // Read the negotiated geometry back — the converter may have kept the
        // native size, and the stride tells us whether rows arrive bottom-up.
        let current = reader.GetCurrentMediaType(stream).map_err(setup_err)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE).map_err(setup_err)?;
        let width = (size >> 32) as u32;
        let height = (size & 0xFFFF_FFFF) as u32;
        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or(width as i32 * 4, |s| s as i32);
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
/// matches `unique_id` (the DirectShow device path — the same device-interface
/// string, compared case-insensitively).
unsafe fn activate_source(unique_id: &str) -> Result<IMFMediaSource, CaptureError> {
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

        let (mut devices, mut count) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
        MFEnumDeviceSources(&enum_attrs, &raw mut devices, &raw mut count).map_err(setup_err)?;
        let list = std::slice::from_raw_parts(devices, count as usize);
        let mut chosen = None;
        for activate in list.iter().flatten() {
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
            let matches = link
                .to_string()
                .is_ok_and(|s| s.eq_ignore_ascii_case(unique_id));
            CoTaskMemFree(Some(link.as_ptr().cast()));
            if matches {
                chosen = Some(activate.clone());
                break;
            }
        }
        let result = match chosen {
            Some(activate) => activate
                .ActivateObject::<IMFMediaSource>()
                .map_err(|e| access_or_setup(&e)),
            None => Err(CaptureError::NotFound),
        };
        CoTaskMemFree(Some(devices.cast()));
        result
    }
}

/// Copy one locked RGB32 sample into a tightly-packed BGRA [`Frame`] in the
/// shared slot, flipping bottom-up rows when the stride is negative.
fn store_frame(shared: &Shared, data: *mut u8, len: usize, hint: StrideHint) {
    let (width, height) = (hint.width as usize, hint.height as usize);
    let row_bytes = width * 4;
    let stride = hint.stride.unsigned_abs() as usize;
    if width == 0 || height == 0 || data.is_null() || stride * (height - 1) + row_bytes > len {
        return;
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
    if let Ok(mut slot) = shared.latest.lock() {
        *slot = Some(Arc::new(Frame {
            width: hint.width,
            height: hint.height,
            bgra,
        }));
        shared.generation.fetch_add(1, Ordering::Relaxed);
    }
}

fn setup_err(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Setup(e.to_string())
}

/// Map an activation failure to AccessDenied when the system privacy toggle
/// is the cause (E_ACCESSDENIED), Setup otherwise.
fn access_or_setup(e: &windows::core::Error) -> CaptureError {
    const E_ACCESSDENIED: windows::core::HRESULT = windows::core::HRESULT(0x8007_0005_u32 as i32);
    if e.code() == E_ACCESSDENIED {
        CaptureError::AccessDenied
    } else {
        CaptureError::Setup(e.to_string())
    }
}
