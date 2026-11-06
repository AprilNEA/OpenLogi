//! V4L2 camera capture on Linux: a one-shot snapshot and a live frame stream.
//!
//! Buffers are mmap'd from the kernel and decoded to **BGRA**, gpui's native
//! texture order, so the preview uploads them without a channel swap.
//!
//! Format choice prefers **MJPEG**: at 720p a YUYV stream is ~27 MB/s over USB
//! and starves other bandwidth on the same controller, while MJPEG is a tenth
//! of that, and `zune-jpeg` decodes it straight to BGRA in one pass. YUYV is
//! the fallback for the few cameras that don't offer MJPEG.
//!
//! Resolution follows the session's [`Quality`]: the live preview streams the
//! [`StreamMode`] it is asked for (720p at about 30 fps when none is chosen),
//! while a snapshot takes the camera's largest mode. Note this only shapes
//! *OpenLogi's own* stream — resolution and frame rate are negotiated per
//! handle (V4L2's set-format and set-parameters ioctls), so they are not device
//! settings other applications observe, unlike the UVC controls in `uvc_linux`.
//!
//! Unlike macOS, Linux has no per-app camera consent model — access is decided
//! by filesystem permission on `/dev/video*` (the `video` group). So
//! [`camera_authorization`] reports `Granted`/`Denied` by probing whether the
//! node actually opens, and never `Undetermined`: there is nothing to prompt.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use v4l::buffer::Type;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::{CaptureStream, Stream as StreamTrait};
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub use crate::capture_types::{CaptureError, Frame};
use crate::stream_mode::DEFAULT_RESOLUTION;
use crate::{CameraAuthorization, StreamMode, StreamModes, linux};

/// How long a camera needs after its [`CameraStream`] is dropped before it can
/// be opened again without the driver stalling the caller.
///
/// A dropped stream is torn down on its own thread, and `VIDIOC_STREAMOFF`
/// takes ~600 ms on a UVC camera (see [`CameraStream::drop`]). Opening sooner
/// is not an error — [`with_busy_retry`] waits it out — but the wait would land
/// on the caller's thread, so a caller reopening on purpose, a mode change,
/// sleeps this long first instead. 100 ms of headroom over the measurement.
pub const RELEASE_DELAY: Duration = Duration::from_millis(700);

/// Size requested for a native-resolution session when the driver reports only
/// stepwise or continuous frame sizes, with no discrete list to pick a maximum
/// from. `VIDIOC_S_FMT` clamps a request to what the device supports, so asking
/// for more than any current sensor offers resolves to its largest mode.
const OVERSIZED_REQUEST: u32 = 16384;

/// Mapped buffers to keep in flight. Four is the usual V4L2 default: enough to
/// absorb a scheduling hiccup without adding a frame of latency.
const BUFFER_COUNT: u32 = 4;

/// How long a live stream waits for one frame before giving up on the camera.
///
/// Sized for **stream start-up**, not the steady state: a UVC camera negotiates
/// bandwidth and spins up its sensor on the first `STREAMON`, which measured
/// ~730 ms on an MX Brio, against ~32 ms per frame once running. A timeout is
/// unrecoverable (see [`run_stream`]), so this must clear the slowest start-up
/// comfortably rather than sit close to it.
const STREAM_TIMEOUT: Duration = Duration::from_secs(3);

/// The pixel layouts this backend decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// Motion-JPEG: one baseline JPEG per frame.
    Mjpeg,
    /// Packed YUV 4:2:2, two pixels per four bytes (`Y0 Cb Y1 Cr`).
    Yuyv,
}

impl Encoding {
    /// FourCCs in preference order — MJPEG first, for the bandwidth reason in
    /// the module docs.
    const PREFERRED: [(Self, &'static [u8; 4]); 2] =
        [(Self::Mjpeg, b"MJPG"), (Self::Yuyv, b"YUYV")];
}

/// What a capture session optimises for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quality {
    /// The mode the user chose, or — when none is chosen, or the chosen one is
    /// no longer offered — the camera's mode nearest 720p at 30 fps. Sharp in
    /// the preview box at a fraction of a 4K frame's decode, copy and texture
    /// upload, and a live stream pays that cost every frame.
    Preview(Option<StreamMode>),
    /// The camera's largest mode. A snapshot is taken once and kept, so it is
    /// worth the sensor's full detail; this mirrors the macOS backend, where
    /// only the preview session carries a 720p preset.
    Native,
}

/// A negotiated capture session: the open device plus what its frames contain.
struct Session {
    device: Device,
    encoding: Encoding,
    width: u32,
    height: u32,
}

/// An opened camera node and the pixel format this backend will stream it in.
struct Opened {
    device: Device,
    encoding: Encoding,
    fourcc: FourCC,
}

/// Open `unique_id` and pick the decodable pixel format to stream it in.
///
/// Shared by [`open_session`] and [`stream_modes`], so the list of modes a user
/// chooses from is always the list of the format that will then be streamed —
/// a camera's MJPEG and YUYV modes differ (many top out lower, or at a slower
/// rate, in YUYV).
fn open_device(unique_id: &str) -> Result<Opened, CaptureError> {
    let path = linux::node_for_unique_id(unique_id).ok_or(CaptureError::NotFound)?;
    let device = Device::with_path(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            CaptureError::AccessDenied
        } else {
            CaptureError::Setup(format!("{}: {error}", path.display()))
        }
    })?;

    let available = device
        .enum_formats()
        .map_err(|error| CaptureError::Setup(error.to_string()))?;

    let (encoding, fourcc) = Encoding::PREFERRED
        .into_iter()
        .find(|(_, fourcc)| {
            available
                .iter()
                .any(|format| format.fourcc == FourCC::new(fourcc))
        })
        .ok_or_else(|| {
            CaptureError::Setup(format!(
                "camera offers no MJPEG or YUYV format (has: {})",
                available
                    .iter()
                    .map(|format| format.fourcc.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

    Ok(Opened {
        device,
        encoding,
        fourcc: FourCC::new(fourcc),
    })
}

/// Open `unique_id` and negotiate a decodable format on it.
fn open_session(unique_id: &str, quality: Quality) -> Result<Session, CaptureError> {
    let Opened {
        device,
        encoding,
        fourcc,
    } = open_device(unique_id)?;

    let (size, fps) = match quality {
        Quality::Preview(requested) => {
            preview_target(&linux::stream_modes(&device, fourcc), requested)
        }
        Quality::Native => (largest_size(&device, fourcc), None),
    };
    let requested = Format::new(size.0, size.1, fourcc);
    let actual = with_busy_retry(|| device.set_format(&requested))
        .map_err(|error| CaptureError::Setup(error.to_string()))?;

    // The driver may substitute a format it prefers; decoding the wrong layout
    // would render as noise, so fail loudly instead.
    if actual.fourcc != fourcc {
        return Err(CaptureError::Setup(format!(
            "driver substituted {} for the requested {fourcc}",
            actual.fourcc,
        )));
    }

    // After the format, not before: `S_FMT` resets the frame interval to the
    // new size's default, so a rate set earlier would be thrown away.
    if let Some(fps) = fps {
        request_fps(&device, fps);
    }

    Ok(Session {
        device,
        encoding,
        width: actual.width,
        height: actual.height,
    })
}

/// The frame size, and frame rate when there is one to ask for, a preview
/// session requests: the user's `requested` mode if the camera offers it, else
/// the camera's default mode (see [`StreamModes::resolve`]).
///
/// A camera that lists no discrete modes — a stepwise or continuous driver, or
/// one whose enumeration failed — gets the preset 720p and **no** frame rate,
/// the behaviour before modes were selectable: there is no rate to ask for.
fn preview_target(modes: &StreamModes, requested: Option<StreamMode>) -> ((u32, u32), Option<u32>) {
    match modes.resolve(requested) {
        Some(mode) => (mode.resolution(), Some(mode.fps)),
        None => (DEFAULT_RESOLUTION, None),
    }
}

/// Ask the driver for `fps` on this handle (V4L2's set-parameters ioctl).
///
/// Best effort: the stream is already usable at the driver's own rate, so a
/// refusal costs the user's frame rate, not their preview. The driver answers
/// with the rate it settled on, which can differ from the request, and that is
/// what gets logged.
fn request_fps(device: &Device, fps: u32) {
    match with_busy_retry(|| device.set_params(&Parameters::with_fps(fps))) {
        Ok(actual) => {
            let granted = linux::fps_of(actual.interval);
            if granted == Some(fps) {
                tracing::debug!(fps, "camera frame rate set");
            } else {
                tracing::warn!(
                    requested = fps,
                    ?granted,
                    "camera granted a different frame rate"
                );
            }
        }
        Err(error) => {
            tracing::warn!(%error, fps, "camera refused the frame rate; streaming at its default");
        }
    }
}

/// The largest frame size the camera offers for `fourcc`, by pixel count.
///
/// Only discrete sizes can be compared directly; a driver advertising a
/// stepwise or continuous range gets [`OVERSIZED_REQUEST`] instead and clamps
/// it down itself.
fn largest_size(device: &Device, fourcc: FourCC) -> (u32, u32) {
    device
        .enum_framesizes(fourcc)
        .into_iter()
        .flatten()
        .flat_map(|size| size.size.to_discrete())
        .map(|discrete| (discrete.width, discrete.height))
        .max_by_key(|&(width, height)| u64::from(width) * u64::from(height))
        .unwrap_or((OVERSIZED_REQUEST, OVERSIZED_REQUEST))
}

/// `EBUSY` — the device is still streaming, here always on another handle.
const BUSY: i32 = 16;

/// How long to keep retrying `REQBUFS` while a previous stream finishes tearing
/// down. Covers the ~600 ms `STREAMOFF` that [`CameraStream::drop`] leaves
/// running, with headroom for a slower camera.
const REOPEN_GRACE: Duration = Duration::from_millis(1500);

/// Gap between `REQBUFS` attempts while waiting out a teardown.
const REOPEN_POLL: Duration = Duration::from_millis(25);

/// Map buffers for a capture stream and arm its per-frame timeout.
///
/// The stream is deliberately **not** started here: `MmapStream::next` enqueues
/// every buffer and issues `STREAMON` itself on first use. Calling `start()`
/// first would mark the stream active with an empty queue, so `next` would take
/// its steady-state path and only ever cycle one buffer.
///
/// `REQBUFS` is retried while the device reports `EBUSY`, which happens when a
/// just-dropped stream is still in `STREAMOFF` — reselecting the same camera
/// within ~600 ms otherwise fails outright. Waiting here (rarely, and only on a
/// re-open) is the trade for never blocking the UI thread in `drop`.
fn build_stream(session: &Session, timeout: Duration) -> Result<MmapStream<'static>, CaptureError> {
    let mut stream = with_busy_retry(|| {
        MmapStream::with_buffers(&session.device, Type::VideoCapture, BUFFER_COUNT)
    })
    .map_err(|error| CaptureError::Setup(error.to_string()))?;
    stream.set_timeout(timeout);
    Ok(stream)
}

/// Run a V4L2 setup ioctl, retrying while the driver reports the device busy.
///
/// Both `VIDIOC_S_FMT` and `VIDIOC_REQBUFS` return `EBUSY` while *any* handle
/// is still streaming, so a re-open racing a previous stream's `STREAMOFF` hits
/// this on whichever call comes first.
fn with_busy_retry<T>(mut step: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let deadline = Instant::now() + REOPEN_GRACE;
    loop {
        match step() {
            Err(error) if error.raw_os_error() == Some(BUSY) && Instant::now() < deadline => {
                std::thread::sleep(REOPEN_POLL);
            }
            outcome => return outcome,
        }
    }
}

/// Capture a single frame from the camera with `unique_id`, at the camera's
/// native resolution.
///
/// A partially-filled MJPEG buffer (a dropped USB packet) fails to decode, so
/// this keeps reading until one decodes or `timeout` elapses. The caller's whole
/// budget is given to the dequeue, since the first frame carries the stream
/// start-up cost described on `STREAM_TIMEOUT` — and more of it at full
/// resolution, where the sensor has more to read out per frame.
///
/// # Errors
/// [`CaptureError::NotFound`] when no camera matches, [`CaptureError::AccessDenied`]
/// without permission on the node, [`CaptureError::Timeout`] when no frame
/// decodes in time.
pub fn capture_frame(unique_id: &str, timeout: Duration) -> Result<Frame, CaptureError> {
    let session = open_session(unique_id, Quality::Native)?;
    let mut stream = build_stream(&session, timeout)?;
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        // A dequeue error leaves the stream unusable (see `run_stream`), so
        // there is nothing to retry — only a torn frame is worth another pass.
        let Ok((buffer, meta)) = stream.next() else {
            break;
        };
        if let Some(frame) = decode(&buffer[..used(buffer, meta.bytesused)], &session) {
            return Ok(frame);
        }
    }

    Err(CaptureError::Timeout)
}

/// The filled prefix of a mapped buffer. `bytesused` is what the driver wrote;
/// the mapping itself is the larger negotiated buffer size, and the tail is
/// stale data from an earlier frame.
fn used(buffer: &[u8], bytesused: u32) -> usize {
    (bytesused as usize).min(buffer.len())
}

/// Frame slot shared between the capture thread and the UI's polling.
struct Shared {
    latest: Mutex<Option<Arc<Frame>>>,
    generation: AtomicU64,
}

/// A running capture stream. Dropping it stops the thread and releases the
/// camera, which is what turns the hardware LED back off.
pub struct CameraStream {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
}

impl CameraStream {
    /// The most recently delivered frame, or `None` before the first arrives.
    /// Returns a shared [`Arc`] so polling at preview rate never copies the
    /// pixel buffer.
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
        // Signal and return: the worker tears the stream down on its own.
        //
        // Deliberately *not* a join. `VIDIOC_STREAMOFF` blocks for ~600 ms on a
        // UVC camera while the kernel gives back the USB isochronous bandwidth
        // reservation, and the GUI drops the preview from `set_target` on the
        // UI thread — joining would freeze the window for that long on every
        // switch away from the Camera tab. The cost of not waiting is that the
        // device stays busy briefly, which [`build_stream`] absorbs.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The [`StreamMode`]s the camera with `unique_id` offers, in the pixel format
/// [`start_stream`] will use — so every mode listed is one it can start.
///
/// Reads the driver's metadata only (no buffers, no streaming), so it is cheap
/// and works while another handle, including this process's own preview, is
/// streaming. Empty when the driver reports only stepwise or continuous sizes.
///
/// # Errors
/// [`CaptureError::NotFound`] when no camera matches, [`CaptureError::AccessDenied`]
/// without permission on the node, [`CaptureError::Setup`] when the camera
/// offers neither MJPEG nor YUYV.
pub fn stream_modes(unique_id: &str) -> Result<StreamModes, CaptureError> {
    let Opened { device, fourcc, .. } = open_device(unique_id)?;
    Ok(linux::stream_modes(&device, fourcc))
}

/// Start a live capture stream on the camera with `unique_id`, at `mode` when
/// the camera offers it and at its default mode (nearest 720p at 30 fps)
/// otherwise. A stale `mode` — saved for a camera that has since been swapped,
/// or a driver that dropped it — therefore degrades to a working stream rather
/// than failing.
///
/// # Errors
/// Same as [`capture_frame`], minus `Timeout` (frames are polled, not awaited).
pub fn start_stream(
    unique_id: &str,
    mode: Option<StreamMode>,
) -> Result<CameraStream, CaptureError> {
    let session = open_session(unique_id, Quality::Preview(mode))?;
    let stream = build_stream(&session, STREAM_TIMEOUT)?;

    let shared = Arc::new(Shared {
        latest: Mutex::new(None),
        generation: AtomicU64::new(0),
    });
    let stop = Arc::new(AtomicBool::new(false));

    std::thread::Builder::new()
        .name("openlogi-camera".into())
        .spawn({
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            move || run_stream(stream, &session, &shared, &stop)
        })
        .map_err(|error| CaptureError::Setup(error.to_string()))?;

    Ok(CameraStream { shared, stop })
}

/// Pump frames into `shared` until `stop` is set or the camera stops delivering.
///
/// A dequeue error ends the loop rather than retrying. `MmapStream::next` only
/// re-queues the buffer it last dequeued, so after a timeout the buffer it
/// points at is still queued and every later call fails `VIDIOC_QBUF` with
/// `EINVAL` — retrying would spin the CPU forever without ever recovering. The
/// preview freezes on the last good frame, which the stalled frame generation
/// makes visible to the caller.
fn run_stream(
    mut stream: MmapStream<'static>,
    session: &Session,
    shared: &Shared,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Relaxed) {
        let (buffer, meta) = match stream.next() {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "camera stream ended");
                break;
            }
        };
        let Some(frame) = decode(&buffer[..used(buffer, meta.bytesused)], session) else {
            continue;
        };
        if let Ok(mut slot) = shared.latest.lock() {
            *slot = Some(Arc::new(frame));
        }
        shared.generation.fetch_add(1, Ordering::Relaxed);
    }
    let _ = stream.stop();
}

/// Decode one raw buffer into a BGRA frame, or `None` when the buffer is
/// truncated or malformed (a dropped USB packet mid-frame).
fn decode(buffer: &[u8], session: &Session) -> Option<Frame> {
    match session.encoding {
        Encoding::Mjpeg => decode_mjpeg(buffer),
        Encoding::Yuyv => decode_yuyv(buffer, session.width, session.height),
    }
}

/// Decode a Motion-JPEG frame straight to BGRA.
///
/// Dimensions come from the JPEG header rather than the negotiated format:
/// they agree in practice, but trusting the header keeps the buffer length and
/// the reported size consistent even if a driver lies.
fn decode_mjpeg(buffer: &[u8]) -> Option<Frame> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(buffer), options);
    let bgra = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));

    // A frame whose payload doesn't match its header is a torn capture.
    if bgra.len() < (width as usize) * (height as usize) * 4 {
        return None;
    }

    Some(Frame {
        width,
        height,
        bgra,
    })
}

/// Convert packed YUYV 4:2:2 to BGRA using BT.601, the colour space UVC
/// cameras encode in.
///
/// Coefficients are scaled by 256 so the whole conversion is integer work; at
/// 720p30 this runs per pixel on the capture thread.
fn decode_yuyv(buffer: &[u8], width: u32, height: u32) -> Option<Frame> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    if buffer.len() < pixels * 2 {
        return None;
    }

    let mut bgra = vec![0u8; pixels * 4];
    for (pair, out) in buffer[..pixels * 2]
        .as_chunks::<4>()
        .0
        .iter()
        .zip(bgra.as_chunks_mut::<8>().0)
    {
        let (y0, u, y1, v) = (
            i32::from(pair[0]),
            i32::from(pair[1]) - 128,
            i32::from(pair[2]),
            i32::from(pair[3]) - 128,
        );
        write_bgra(&mut out[..4], y0, u, v);
        write_bgra(&mut out[4..], y1, u, v);
    }

    Some(Frame {
        width,
        height,
        bgra,
    })
}

/// Write one BT.601 YUV sample as a BGRA pixel.
fn write_bgra(out: &mut [u8], y: i32, u: i32, v: i32) {
    let y = y * 256;
    out[0] = clamp_u8(y + 452 * u);
    out[1] = clamp_u8(y - 88 * u - 183 * v);
    out[2] = clamp_u8(y + 359 * v);
    out[3] = 0xFF;
}

/// Saturate a fixed-point channel (scaled by 256) into a byte.
#[expect(
    clippy::cast_sign_loss,
    reason = "the channel is clamped to 0..=255 before the narrowing"
)]
fn clamp_u8(scaled: i32) -> u8 {
    (scaled / 256).clamp(0, 255) as u8
}

/// Whether this process can open the camera nodes it can see.
#[must_use]
pub fn camera_access_granted() -> bool {
    camera_authorization() == CameraAuthorization::Granted
}

/// Report camera access by probing a node.
///
/// Linux has no consent prompt: a node either opens or it doesn't, decided by
/// its group permissions. `Undetermined` is therefore never returned — with no
/// camera present at all there is nothing to authorize, which reads as
/// `Granted` (nothing is being withheld).
#[must_use]
pub fn camera_authorization() -> CameraAuthorization {
    let nodes = linux::nodes();
    if nodes.is_empty() {
        return CameraAuthorization::Granted;
    }
    if nodes
        .iter()
        .any(|node| Device::with_path(&node.path).is_ok())
    {
        CameraAuthorization::Granted
    } else {
        CameraAuthorization::Denied
    }
}

/// No-op: Linux has no consent prompt — access is device-node permissions.
pub fn request_camera_access() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_rejects_a_short_buffer() {
        // One byte short of a single 2x1 macropixel.
        assert!(decode_yuyv(&[0; 3], 2, 1).is_none());
    }

    #[test]
    fn yuyv_decodes_grey_to_grey() {
        // Y=128 with neutral chroma is mid-grey in every channel.
        let frame = decode_yuyv(&[128, 128, 128, 128], 2, 1).expect("2x1 frame");
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.bgra, vec![128, 128, 128, 255, 128, 128, 128, 255]);
    }

    #[test]
    fn yuyv_saturates_out_of_gamut_chroma() {
        // Peak luma with peak chroma drives blue to 479 and red to 433 before
        // clamping. They must saturate at 255, not wrap (479 as a truncated
        // byte would be 223 — a vivid colour turning muddy). Green lands at
        // 120 legitimately, inside the range, so it pins the coefficients too.
        let frame = decode_yuyv(&[255, 255, 255, 255], 2, 1).expect("2x1 frame");
        assert_eq!(&frame.bgra[..4], &[255, 120, 255, 255]);
    }

    #[test]
    fn mjpeg_rejects_a_non_jpeg_buffer() {
        assert!(decode_mjpeg(&[0xFF; 64]).is_none());
    }

    #[test]
    fn used_clamps_a_driver_overreporting_bytesused() {
        // A driver claiming more than the mapping holds must not panic the
        // slice below.
        assert_eq!(used(&[0; 10], 99), 10);
        assert_eq!(used(&[0; 10], 4), 4);
    }

    fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
        StreamMode { width, height, fps }
    }

    /// What a StreamCam-like camera offers over MJPEG.
    fn modes() -> StreamModes {
        StreamModes::new([
            mode(1920, 1080, 60),
            mode(1920, 1080, 30),
            mode(1280, 720, 60),
            mode(1280, 720, 30),
            mode(640, 360, 30),
        ])
    }

    #[test]
    fn preview_asks_for_the_chosen_mode() {
        let chosen = mode(1920, 1080, 60);
        assert_eq!(
            preview_target(&modes(), Some(chosen)),
            ((1920, 1080), Some(60))
        );
    }

    #[test]
    fn preview_without_a_choice_uses_720p30() {
        assert_eq!(preview_target(&modes(), None), ((1280, 720), Some(30)));
    }

    #[test]
    fn preview_falls_back_when_the_chosen_mode_is_no_longer_offered() {
        // Saved for another camera, or one this driver doesn't list.
        let stale = mode(3840, 2160, 30);
        assert_eq!(
            preview_target(&modes(), Some(stale)),
            ((1280, 720), Some(30))
        );
    }

    #[test]
    fn preview_of_a_camera_with_no_modes_keeps_the_preset_and_no_rate() {
        // A stepwise driver: nothing to resolve against, and no rate to ask for.
        let none = StreamModes::default();
        assert_eq!(preview_target(&none, None), (DEFAULT_RESOLUTION, None));
        assert_eq!(
            preview_target(&none, Some(mode(1920, 1080, 60))),
            (DEFAULT_RESOLUTION, None)
        );
    }
}
