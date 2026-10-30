//! The preview's hardware boundary. Tests substitute capture resources, not
//! the view's lifecycle, permission subscription, texture handling or tasks.

use std::sync::Arc;
use std::time::Duration;

use openlogi_camera::{CameraStream, CaptureError, Frame, StreamMode, StreamModes};

pub(super) trait Capture {
    fn access_granted(&self) -> bool;
    /// How long a dropped stream's camera needs before it can be opened again
    /// without the open stalling the caller.
    fn release_delay(&self) -> Duration;
    /// The resolutions and frame rates `target` offers. Driver metadata only,
    /// cheap enough to read on the UI thread; empty when the platform backend
    /// lists none.
    fn stream_modes(&self, target: &str) -> Result<StreamModes, CaptureError>;
    /// Open `target` at `mode`, or at the backend's default when `None`.
    fn start_stream(
        &self,
        target: &str,
        mode: Option<StreamMode>,
    ) -> Result<Box<dyn PreviewStream>, CaptureError>;
}

pub(super) trait PreviewStream {
    fn frame_generation(&self) -> u64;
    fn take_frame(&self) -> Option<Arc<Frame>>;
}

pub(super) struct SystemCapture;

impl Capture for SystemCapture {
    fn access_granted(&self) -> bool {
        openlogi_camera::camera_access_granted()
    }

    fn release_delay(&self) -> Duration {
        openlogi_camera::RELEASE_DELAY
    }

    fn stream_modes(&self, target: &str) -> Result<StreamModes, CaptureError> {
        openlogi_camera::stream_modes(target)
    }

    fn start_stream(
        &self,
        target: &str,
        mode: Option<StreamMode>,
    ) -> Result<Box<dyn PreviewStream>, CaptureError> {
        openlogi_camera::start_stream(target, mode)
            .map(|stream| Box::new(stream) as Box<dyn PreviewStream>)
    }
}

impl PreviewStream for CameraStream {
    fn frame_generation(&self) -> u64 {
        self.frame_generation()
    }

    fn take_frame(&self) -> Option<Arc<Frame>> {
        self.take_frame()
    }
}
