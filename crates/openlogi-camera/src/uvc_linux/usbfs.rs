//! Reconnecting a camera on Linux, over usbfs's `USBDEVFS_RESET`.
//!
//! A port reset makes the kernel read the camera's descriptors again. When
//! they changed — the microphone switch adds or removes the audio interface —
//! the hub driver treats the camera as a new device and enumerates it from
//! scratch, as a replug would. The request needs write access to the camera's
//! `/dev/bus/usb/BBB/DDD` node. Only root has it, because OpenLogi's udev rule
//! grants the seat user hidraw and input nodes but not usbfs; without it the
//! caller gets [`ControlError::NeedsElevation`].

#![expect(unsafe_code, reason = "the usbfs USBDEVFS_RESET ioctl")]

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use v4l::v4l2::vidioc::_IOC_TYPE;

use crate::controls::ControlError;

/// `USBDEVFS_RESET`: `_IO('U', 20)`, which carries no direction and no size,
/// only the type `'U'` (0x55) in bits 15..8 and the number (20) in 7..0.
const USBDEVFS_RESET: u32 = (0x55 << 8) | 0x14;

/// The request code in the type the `v4l` ioctl wrapper takes: `c_ulong` on
/// glibc, `c_int` on musl.
#[cfg(not(target_env = "musl"))]
fn request_code() -> _IOC_TYPE {
    _IOC_TYPE::from(USBDEVFS_RESET)
}
#[cfg(target_env = "musl")]
fn request_code() -> _IOC_TYPE {
    _IOC_TYPE::from_ne_bytes(USBDEVFS_RESET.to_ne_bytes())
}

/// `errno` values the reset can end with. `ENODEV` means the descriptors
/// changed and the old device is gone, which is the reconnect this asks for;
/// `EPERM` and `EACCES` mean the caller may not reset it.
const EPERM: i32 = 1;
const EACCES: i32 = 13;
const ENODEV: i32 = 19;

/// The usbfs node of the USB device with `address` on bus `bus`.
pub(super) fn node(bus: u16, address: u16) -> PathBuf {
    PathBuf::from(format!("/dev/bus/usb/{bus:03}/{address:03}"))
}

/// Reset the USB device at `node`, so that it enumerates again.
///
/// # Errors
/// [`ControlError::NeedsElevation`] without write access to `node`,
/// [`ControlError::NotFound`] when the node is gone, and [`ControlError::Io`]
/// for any other failure.
pub(super) fn reset(node: &Path) -> Result<(), ControlError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(node)
        .map_err(|error| match error.kind() {
            ErrorKind::PermissionDenied => ControlError::NeedsElevation,
            ErrorKind::NotFound => ControlError::NotFound,
            _ => ControlError::Io(error.to_string()),
        })?;
    // SAFETY: `file` keeps the usbfs node open for the whole call, and
    // `USBDEVFS_RESET` reads no argument, so a null pointer is what it takes.
    let reset = unsafe { v4l::v4l2::ioctl(file.as_raw_fd(), request_code(), std::ptr::null_mut()) };
    match reset {
        Ok(()) => Ok(()),
        Err(error) => match error.raw_os_error() {
            Some(ENODEV) => Ok(()),
            Some(EPERM | EACCES) => Err(ControlError::NeedsElevation),
            _ => Err(ControlError::Io(error.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{USBDEVFS_RESET, node};

    /// `linux/usbdevice_fs.h`: `#define USBDEVFS_RESET _IO('U', 20)`.
    #[test]
    fn the_reset_request_is_io_u_20() {
        assert_eq!(USBDEVFS_RESET, 0x5514);
    }

    /// usbfs names a node by the zero-padded bus and device numbers sysfs
    /// reports as `busnum` and `devnum`.
    #[test]
    fn the_node_pads_bus_and_address_to_three_digits() {
        assert_eq!(node(1, 4).to_str(), Some("/dev/bus/usb/001/004"));
        assert_eq!(node(3, 127).to_str(), Some("/dev/bus/usb/003/127"));
    }
}
