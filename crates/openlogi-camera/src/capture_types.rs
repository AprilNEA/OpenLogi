//! Platform-independent capture vocabulary shared by every capture backend
//! (AVFoundation on macOS, Media Foundation on Windows, stubs elsewhere).

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
#[derive(Debug, Clone)]
pub enum CaptureError {
    /// Camera permission is denied/restricted, or this process can't request
    /// it (e.g. an unbundled macOS binary with no `NSCameraUsageDescription`).
    AccessDenied,
    /// No camera matched the requested unique id.
    NotFound,
    /// The session ran but produced no frame within the timeout.
    Timeout,
    /// A platform capture object failed to construct.
    Setup(String),
    /// Capture has no backend on this platform.
    Unsupported,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessDenied => write!(
                f,
                "camera access denied — grant Camera permission (on macOS, run inside an app bundle with NSCameraUsageDescription)"
            ),
            Self::NotFound => write!(f, "no camera matched that id"),
            Self::Timeout => write!(f, "camera produced no frame in time"),
            Self::Setup(s) => write!(f, "capture setup failed: {s}"),
            Self::Unsupported => write!(f, "camera capture is not implemented on this platform"),
        }
    }
}

impl std::error::Error for CaptureError {}
