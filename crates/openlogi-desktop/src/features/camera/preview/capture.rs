//! The preview's hardware boundary. Tests substitute capture resources, not
//! the view's lifecycle, permission subscription, texture handling or tasks.

use std::sync::Arc;

use openlogi_camera::{CameraStream, CaptureError, Frame};

pub(super) trait Capture {
    fn access_granted(&self) -> bool;
    fn start_stream(&self, target: &str) -> Result<Box<dyn PreviewStream>, CaptureError>;
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

    fn start_stream(&self, target: &str) -> Result<Box<dyn PreviewStream>, CaptureError> {
        openlogi_camera::start_stream(target)
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
