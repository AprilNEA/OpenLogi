//! Extension Unit controls on Linux, over uvcvideo's `UVCIOC_CTRL_QUERY`.
//!
//! V4L2 maps only the standard UVC controls to control ids. A vendor
//! Extension Unit control has none, so it is addressed the way the camera
//! sees it: by the unit id its descriptor declares and the selector inside
//! that unit. The descriptor comes from sysfs, which is world-readable; the
//! query goes through the same `/dev/video*` node the standard controls use,
//! so it needs no access beyond that.

#![expect(unsafe_code, reason = "the uvcvideo UVCIOC_CTRL_QUERY ioctl")]

use v4l::Device;
use v4l::v4l2::vidioc::_IOC_TYPE;

use crate::controls::{ControlError, ControlRange};
use crate::extension_unit::ExtensionControl;
use crate::linux;
use crate::uvc_descriptor::{VcTopology, scan_descriptors};

/// `UVC_SET_CUR`, `UVC_GET_CUR`, `UVC_GET_LEN` and `UVC_GET_INFO` request
/// codes (`linux/usb/video.h`).
const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;
/// `GET_INFO` bits: the control answers `GET_*` and accepts `SET_CUR`.
const INFO_GET_SET: u8 = 0x03;

/// `struct uvc_xu_control_query` (`linux/uvcvideo.h`).
#[repr(C)]
struct XuQuery {
    unit: u8,
    selector: u8,
    query: u8,
    size: u16,
    data: *mut u8,
}

/// `size_of::<XuQuery>()`, written out so the request code below needs no
/// cast. A test checks it against the real layout.
#[cfg(target_pointer_width = "64")]
const QUERY_SIZE: u32 = 16;
#[cfg(target_pointer_width = "32")]
const QUERY_SIZE: u32 = 12;

/// `UVCIOC_CTRL_QUERY`: `_IOWR('u', 0x21, struct uvc_xu_control_query)`.
/// Direction read|write sits in bits 31..30, the size in 29..16, the type
/// `'u'` (0x75) in 15..8 and the number in 7..0.
const UVCIOC_CTRL_QUERY: u32 = (3 << 30) | (QUERY_SIZE << 16) | (0x75 << 8) | 0x21;

/// The request code in the type the `v4l` ioctl wrapper takes: `c_ulong` on
/// glibc, `c_int` on musl.
#[cfg(not(target_env = "musl"))]
fn request_code() -> _IOC_TYPE {
    _IOC_TYPE::from(UVCIOC_CTRL_QUERY)
}
#[cfg(target_env = "musl")]
fn request_code() -> _IOC_TYPE {
    _IOC_TYPE::from_ne_bytes(UVCIOC_CTRL_QUERY.to_ne_bytes())
}

/// `errno` values that mean the camera or driver refuses the query, rather
/// than that the call failed: no such unit or selector (`ENOENT`), a length
/// other than the control's (`ENOBUFS`), a request the control does not
/// support (`EBADRQC`), a device stall (`EPIPE`), and the rejections a
/// standard control write can also see.
const REFUSED: [i32; 8] = [
    2,   // ENOENT
    13,  // EACCES
    16,  // EBUSY
    22,  // EINVAL
    32,  // EPIPE
    34,  // ERANGE
    56,  // EBADRQC
    105, // ENOBUFS
];

/// The Extension Units of one camera, read from its USB descriptor once per
/// device open.
pub(super) struct ExtensionUnits(Option<VcTopology>);

impl ExtensionUnits {
    /// Read the topology of the camera `unique_id` names. A camera whose
    /// descriptor cannot be read or parsed has no Extension Units, so every
    /// vendor control on it reports `Unsupported`.
    pub(super) fn of(unique_id: &str) -> Self {
        Self(linux::usb_descriptors(unique_id).and_then(|blob| scan_descriptors(&blob)))
    }

    /// The unit id that carries `control` on this camera.
    fn unit(&self, control: ExtensionControl) -> Result<u8, ControlError> {
        self.0
            .as_ref()
            .and_then(|topology| topology.extension_unit(control))
            .ok_or(ControlError::Unsupported)
    }

    /// Read a vendor control's range. The camera must report the one-byte,
    /// readable and writable control the table describes; the table, not the
    /// camera's MIN/MAX, owns the value set.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] when the camera lacks the control or
    /// reports it differently.
    pub(super) fn range(
        &self,
        device: &Device,
        control: ExtensionControl,
    ) -> Result<ControlRange, ControlError> {
        let unit = self.unit(control)?;
        let mut len = [0u8; 2];
        query(device, unit, control.selector, UVC_GET_LEN, &mut len)?;
        let mut info = [0u8; 1];
        query(device, unit, control.selector, UVC_GET_INFO, &mut info)?;
        if u16::from_le_bytes(len) != 1 || info[0] & INFO_GET_SET != INFO_GET_SET {
            return Err(ControlError::Unsupported);
        }
        let mut current = [0u8; 1];
        query(device, unit, control.selector, UVC_GET_CUR, &mut current)?;
        control.range(current[0])
    }

    /// Write a vendor control's value.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] when the camera lacks the control, or the
    /// value is outside the table's set.
    pub(super) fn write(
        &self,
        device: &Device,
        control: ExtensionControl,
        value: i32,
    ) -> Result<(), ControlError> {
        let mut byte = [control.encode(value)?];
        let unit = self.unit(control)?;
        query(device, unit, control.selector, UVC_SET_CUR, &mut byte)
    }
}

/// Issue one `UVCIOC_CTRL_QUERY`, reading into or writing from `data`.
fn query(
    device: &Device,
    unit: u8,
    selector: u8,
    request: u8,
    data: &mut [u8],
) -> Result<(), ControlError> {
    let mut query = XuQuery {
        unit,
        selector,
        query: request,
        size: u16::try_from(data.len()).map_err(|_| ControlError::Unsupported)?,
        data: data.as_mut_ptr(),
    };
    // SAFETY: `query` is a live `uvc_xu_control_query` for the whole call, and
    // its `data` points at `size` bytes that `data` keeps borrowed and
    // writable. uvcvideo copies at most `size` bytes in either direction.
    unsafe {
        v4l::v4l2::ioctl(
            device.handle().fd(),
            request_code(),
            (&raw mut query).cast(),
        )
    }
    .map_err(|error| {
        if error.raw_os_error().is_some_and(|no| REFUSED.contains(&no)) {
            ControlError::Unsupported
        } else {
            ControlError::Io(error.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{QUERY_SIZE, UVCIOC_CTRL_QUERY, XuQuery};

    #[test]
    fn the_query_struct_matches_the_kernel_layout() {
        assert_eq!(
            u32::try_from(size_of::<XuQuery>()).expect("a small struct"),
            QUERY_SIZE
        );
    }

    /// `UVCIOC_CTRL_QUERY` as `linux/uvcvideo.h` defines it on a 64-bit build.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_request_code_matches_the_kernel_header() {
        assert_eq!(UVCIOC_CTRL_QUERY, 0xC010_7521);
    }
}
