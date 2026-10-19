//! Logitech UVC camera identities the hardware does not name itself.
//!
//! Camera detection needs no model table: every Logitech webcam is a standard
//! UVC device and is recognized by vendor ID alone. A camera's *name* normally
//! comes from the same place, the product string in its USB device descriptor.
//! Some older models ship without one (`iProduct = 0`), so each OS invents a
//! placeholder from the raw IDs instead — `USB Camera VID:1133 PID:2077` on
//! macOS, `UVC Camera (046d:081d)` on Linux. This table names those models.
//!
//! List a camera here only when its descriptor carries no product string. A
//! camera that names itself is already reported correctly by the OS.

use crate::LOGITECH_VENDOR_ID;

/// A UVC camera whose USB device descriptor carries no product string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnnamedCameraDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Marketed product name.
    pub name: &'static str,
}

impl UnnamedCameraDescriptor {
    const fn logitech(product_id: u16, name: &'static str) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            name,
        }
    }
}

/// All UVC cameras OpenLogi names because their firmware does not.
pub const UNNAMED_CAMERAS: &[UnnamedCameraDescriptor] = &[
    // HD Webcam C510 (720p). Its device descriptor reports `iManufacturer = 0`
    // and `iProduct = 0`; only the serial number string is present.
    UnnamedCameraDescriptor::logitech(0x081d, "Logitech HD Webcam C510"),
];

/// Finds the descriptor of a camera that reports no product string of its own.
#[must_use]
pub fn find_unnamed_camera(
    vendor_id: u16,
    product_id: u16,
) -> Option<&'static UnnamedCameraDescriptor> {
    UNNAMED_CAMERAS
        .iter()
        .find(|camera| camera.vendor_id == vendor_id && camera.product_id == product_id)
}

#[cfg(test)]
mod tests {
    use super::{UNNAMED_CAMERAS, find_unnamed_camera};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn unnamed_camera_identities_are_unique() {
        for (index, camera) in UNNAMED_CAMERAS.iter().enumerate() {
            assert!(
                UNNAMED_CAMERAS[..index].iter().all(|other| {
                    (other.vendor_id, other.product_id) != (camera.vendor_id, camera.product_id)
                }),
                "duplicate camera identity {:04x}:{:04x}",
                camera.vendor_id,
                camera.product_id
            );
        }
    }

    #[test]
    fn unnamed_cameras_carry_a_name() {
        for camera in UNNAMED_CAMERAS {
            assert!(
                !camera.name.trim().is_empty(),
                "camera {:04x}:{:04x} has no name",
                camera.vendor_id,
                camera.product_id
            );
        }
    }

    #[test]
    fn product_id_selects_the_c510() {
        let c510 = find_unnamed_camera(LOGITECH_VENDOR_ID, 0x081d).expect("C510 identity");

        assert_eq!(c510.name, "Logitech HD Webcam C510");
    }

    #[test]
    fn lookup_requires_the_complete_identity() {
        // A camera that names itself (the StreamCam) is not in the table.
        assert!(find_unnamed_camera(LOGITECH_VENDOR_ID, 0x0893).is_none());
        // Product IDs are only unique within a vendor.
        assert!(find_unnamed_camera(0x1234, 0x081d).is_none());
    }
}
