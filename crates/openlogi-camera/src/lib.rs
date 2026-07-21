//! Generic discovery of Logitech USB Video Class (UVC) webcams.
//!
//! Mice and keyboards speak Logitech's proprietary HID++ (over a Bolt/Unifying
//! receiver or directly) — see the `openlogi-hid` crate. Webcams don't: every
//! Logitech camera (StreamCam, Brio, C920, C922, C270, C930e, …) is a standard
//! UVC device and enumerates the same way. So detection keys off the USB vendor
//! id (`0x046d`) rather than any per-model quirk — plug in *any* Logitech
//! camera and it's recognised, with no model table to maintain.
//!
//! macOS has the full backend (AVFoundation capture + IOKit UVC controls);
//! Windows matches it with Media Foundation capture and DirectShow controls;
//! other platforms return an empty list.

use serde::Serialize;

mod controls;
pub use controls::{AutoState, AutoToggle, CameraControl, CameraState, ControlError, ControlRange};

mod capture_types;
pub use capture_types::{CaptureError, Frame};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
pub use capture::{
    CameraStream, camera_access_granted, camera_authorization, capture_frame, start_stream,
};

#[cfg(target_os = "windows")]
mod capture_windows;
#[cfg(target_os = "windows")]
pub use capture_windows::{
    CameraStream, camera_access_granted, camera_authorization, capture_frame, start_stream,
};

#[cfg(target_os = "macos")]
mod uvc;
#[cfg(target_os = "macos")]
pub use uvc::{
    apply_settings, control_range, control_ranges, read_camera_state, set_auto, set_control,
};

#[cfg(target_os = "windows")]
mod uvc_windows;
#[cfg(target_os = "windows")]
pub use uvc_windows::{
    apply_settings, control_range, control_ranges, read_camera_state, set_auto, set_control,
};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod capture {
    //! Stub capture backend for platforms without one.
    use std::sync::Arc;
    use std::time::Duration;

    use crate::capture_types::{CaptureError, Frame};

    /// Stub: returns [`CaptureError::Unsupported`] on this platform.
    pub fn capture_frame(_unique_id: &str, _timeout: Duration) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unsupported)
    }

    /// Stub live stream (never yields a frame on this platform).
    pub struct CameraStream;

    impl CameraStream {
        #[must_use]
        pub fn latest_frame(&self) -> Option<Arc<Frame>> {
            None
        }

        #[must_use]
        pub fn take_frame(&self) -> Option<Arc<Frame>> {
            None
        }

        #[must_use]
        pub fn frame_generation(&self) -> u64 {
            0
        }
    }

    /// Stub: returns [`CaptureError::Unsupported`] on this platform.
    pub fn start_stream(_unique_id: &str) -> Result<CameraStream, CaptureError> {
        Err(CaptureError::Unsupported)
    }

    /// Stub: camera access is never granted on this platform.
    #[must_use]
    pub fn camera_access_granted() -> bool {
        false
    }

    /// Stub: camera permission is always undetermined on this platform.
    #[must_use]
    pub fn camera_authorization() -> crate::CameraAuthorization {
        crate::CameraAuthorization::Undetermined
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use capture::{
    CameraStream, camera_access_granted, camera_authorization, capture_frame, start_stream,
};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod uvc {
    //! Stub UVC control backend for platforms without one.
    use crate::controls::{AutoToggle, CameraControl, CameraState, ControlError, ControlRange};

    /// Stub: no UVC backend on this platform.
    pub fn control_range(_id: &str, _c: CameraControl) -> Result<ControlRange, ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Stub: no UVC backend on this platform.
    pub fn control_ranges(_id: &str) -> Result<Vec<(CameraControl, ControlRange)>, ControlError> {
        Ok(Vec::new())
    }

    /// Stub: no UVC backend on this platform.
    pub fn read_camera_state(_id: &str) -> Result<CameraState, ControlError> {
        Ok(CameraState::default())
    }

    /// Stub: no UVC backend on this platform.
    pub fn set_control(_id: &str, _c: CameraControl, _value: i32) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Stub: no UVC backend on this platform.
    pub fn set_auto(_id: &str, _t: AutoToggle, _on: bool) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Stub: no UVC backend on this platform.
    pub fn apply_settings(
        _id: &str,
        _autos: &[(AutoToggle, bool)],
        _values: &[(CameraControl, i32)],
    ) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use uvc::{
    apply_settings, control_range, control_ranges, read_camera_state, set_auto, set_control,
};

/// Logitech's USB vendor id. Reported in decimal (`1133`) inside an
/// `AVCaptureDevice` modelID, and in hex (`046d`) most everywhere else.
pub const LOGITECH_VID: u16 = 0x046d;

/// Tri-state Camera permission, mirroring macOS `AVAuthorizationStatus`. Off
/// macOS there is no consent model, so the backend reports
/// [`CameraAuthorization::Undetermined`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraAuthorization {
    /// The process may open cameras.
    Granted,
    /// The user denied access, or the system restricts it.
    Denied,
    /// Not yet requested — opening a camera will prompt.
    Undetermined,
}

/// A connected USB Video Class camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Camera {
    /// Human-readable name, e.g. `"Logitech StreamCam"`.
    pub name: String,
    /// Stable per-device identifier from the OS capture layer. Keys the device
    /// in the UI so two identical cameras stay distinct.
    pub unique_id: String,
    /// USB vendor id (`0x046d` for Logitech).
    pub vendor_id: u16,
    /// USB product id (e.g. `0x0893` for the StreamCam).
    pub product_id: u16,
    /// Largest supported frame size `(width, height)`, when the OS reports the
    /// device's formats. Read from metadata only — no capture, no permission.
    pub max_resolution: Option<(u32, u32)>,
    /// Highest supported frame rate (fps) across all formats, when known.
    pub max_fps: Option<u32>,
}

/// Whether this platform has a live-capture backend (preview + snapshot).
/// Enumeration and UVC controls can be supported without it.
#[must_use]
pub const fn capture_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// Serializes UVC device seizes against enumeration within this process.
/// `USBDeviceOpenSeize` briefly detaches the camera's kernel driver, and an
/// enumeration racing that window sees no camera at all — which read as the
/// camera "disappearing" from the device list mid-slider-drag once
/// enumeration moved off the UI thread. Control paths hold this for the
/// seize's lifetime; enumeration takes it for the duration of the scan.
#[cfg(target_os = "macos")]
pub(crate) static USB_QUIESCE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Enumerate every connected **Logitech** UVC camera.
///
/// Non-Logitech cameras (the built-in FaceTime camera, virtual cameras, other
/// vendors' webcams) are filtered out. Returns an empty list on platforms with
/// no capture backend, or when no Logitech camera is attached.
#[must_use]
pub fn enumerate_cameras() -> Vec<Camera> {
    enumerate_all()
        .into_iter()
        .filter(|camera| camera.vendor_id == LOGITECH_VID)
        .collect()
}

#[cfg(target_os = "macos")]
fn enumerate_all() -> Vec<Camera> {
    // Wait out any in-flight control seize so the scan can't land in the
    // window where the kernel driver is detached (poisoning is impossible —
    // holders never panic — but recover anyway rather than unwrap).
    let _quiesce = USB_QUIESCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    macos::enumerate()
        .iter()
        .filter_map(|raw| {
            let mut camera = Camera::from_raw(&raw.name, &raw.unique_id, &raw.model_id)?;
            if raw.max_width > 0 && raw.max_height > 0 {
                camera.max_resolution = Some((raw.max_width, raw.max_height));
            }
            if raw.max_fps > 0 {
                camera.max_fps = Some(raw.max_fps);
            }
            Some(camera)
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn enumerate_all() -> Vec<Camera> {
    uvc_windows::enumerate()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn enumerate_all() -> Vec<Camera> {
    Vec::new()
}

#[cfg(any(test, target_os = "macos"))]
impl Camera {
    /// Build a [`Camera`] from an OS-reported `(name, unique_id, model_id)`.
    ///
    /// Returns `None` when `model_id` carries no USB vendor/product id — i.e.
    /// it isn't a real USB camera (the macOS FaceTime camera's modelID is just
    /// `"FaceTime HD Camera"`), so it can't be attributed to a vendor and is
    /// dropped before the Logitech filter even runs. Format fields start `None`;
    /// the platform backend fills them in.
    fn from_raw(name: &str, unique_id: &str, model_id: &str) -> Option<Self> {
        let (vendor_id, product_id) = parse_vid_pid(model_id)?;
        Some(Self {
            name: name.to_string(),
            unique_id: unique_id.to_string(),
            vendor_id,
            product_id,
            max_resolution: None,
            max_fps: None,
        })
    }
}

/// Pull the USB vendor/product id out of an `AVCaptureDevice` modelID such as
/// `"UVC Camera VendorID_1133 ProductID_2195"`. Both ids are **decimal** in
/// that string (1133 == 0x046d, 2195 == 0x0893). `None` if either marker is
/// absent.
#[cfg(any(test, target_os = "macos"))]
fn parse_vid_pid(model_id: &str) -> Option<(u16, u16)> {
    let vendor_id = parse_marker(model_id, "VendorID_")?;
    let product_id = parse_marker(model_id, "ProductID_")?;
    Some((vendor_id, product_id))
}

/// Read the decimal number immediately following `marker` in `haystack`.
#[cfg(any(test, target_os = "macos"))]
fn parse_marker(haystack: &str, marker: &str) -> Option<u16> {
    let rest = haystack.split(marker).nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logitech_streamcam_model_id() {
        assert_eq!(
            parse_vid_pid("UVC Camera VendorID_1133 ProductID_2195"),
            Some((0x046d, 0x0893))
        );
    }

    #[test]
    fn rejects_model_id_without_usb_ids() {
        assert_eq!(parse_vid_pid("FaceTime HD Camera"), None);
        assert_eq!(parse_vid_pid("VendorID_1133 only"), None);
    }

    #[test]
    fn from_raw_keeps_usb_cameras_and_drops_the_rest() {
        assert_eq!(
            Camera::from_raw(
                "Logitech StreamCam",
                "0x1123000046d0893",
                "UVC Camera VendorID_1133 ProductID_2195",
            ),
            Some(Camera {
                name: "Logitech StreamCam".to_string(),
                unique_id: "0x1123000046d0893".to_string(),
                vendor_id: LOGITECH_VID,
                product_id: 0x0893,
                max_resolution: None,
                max_fps: None,
            })
        );
        assert_eq!(
            Camera::from_raw("FaceTime HD Camera", "uuid", "FaceTime HD Camera"),
            None
        );
    }
}
