//! Platform-independent capture vocabulary shared by every capture backend
//! (AVFoundation on macOS, Media Foundation on Windows, stubs elsewhere).

use thiserror::Error;

/// One decoded camera frame, tightly-packed BGRA8 (`width * height * 4` bytes) —
/// gpui's native texture order, so the preview uploads it without a channel
/// swap. The snapshot path swaps to RGBA when it writes the PNG.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Why a capture attempt failed.
#[derive(Debug, Clone, Error)]
pub enum CaptureError {
    /// Camera permission is denied/restricted, or this process can't request
    /// it (e.g. an unbundled macOS binary with no `NSCameraUsageDescription`).
    #[error(
        "camera access denied — grant Camera permission \
         (on macOS, run inside an app bundle with NSCameraUsageDescription)"
    )]
    AccessDenied,
    /// No camera matched the requested unique id.
    #[error("no camera matched that id")]
    NotFound,
    /// The session ran but produced no frame within the timeout.
    #[error("camera produced no frame in time")]
    Timeout,
    /// A platform capture object failed to construct.
    #[error("capture setup failed: {0}")]
    Setup(String),
    /// Capture has no backend on this platform.
    #[error("camera capture is not implemented on this platform")]
    Unsupported,
}
