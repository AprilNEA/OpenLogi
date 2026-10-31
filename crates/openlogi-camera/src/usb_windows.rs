//! The USB link each Logitech camera runs at, on Windows.
//!
//! Windows' hub driver already knows both halves of a [`UsbLink`]: the speed
//! each port's device is running at, and whether that device declared support
//! for SuperSpeed (it reads the BOS descriptor when the device enumerates).
//! Both come from one question to the hub the camera hangs off —
//! `IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX` for the speed and its `_V2`
//! for the capability, the calls the SDK's USBView sample makes. The first
//! answer's speed stops at high speed, so the `_V2` flags also say whether the
//! device runs at SuperSpeed, as they do in USBView. Nothing is sent to the
//! camera.
//!
//! Finding that hub and port from a camera's DirectShow device path takes the
//! Configuration Manager: the path names an interface, whose device node is
//! the camera's function (or the camera itself), whose parent is the USB
//! device that holds the port number, whose parent is the hub.
//!
//! Like the macOS side this runs at enumeration, off the UI thread, and the
//! UI reads what it found from memory: see [`crate::usb_link()`]. Every step
//! can fail on a machine that is not the one this was written against, and any
//! failure leaves the camera with no link, never with a wrong one.

use crate::usb_link::{UsbLink, UsbSpeed};

/// `USB_DEVICE_SPEED` values a hub reports for a connection.
const SPEED_LOW: u8 = 0;
const SPEED_FULL: u8 = 1;
const SPEED_HIGH: u8 = 2;
const SPEED_SUPER: u8 = 3;

/// `USB_NODE_CONNECTION_INFORMATION_EX_V2_FLAGS` bits that matter here.
const OPERATING_AT_SUPERSPEED: u32 = 1 << 0;
const CAPABLE_OF_SUPERSPEED: u32 = 1 << 1;
const OPERATING_AT_SUPERSPEED_PLUS: u32 = 1 << 2;
const CAPABLE_OF_SUPERSPEED_PLUS: u32 = 1 << 3;

/// The link a hub's two answers describe: the connection's `speed` and, when
/// the hub could be asked, the V2 `flags`. `None` for a speed that is not one
/// a hub reports.
///
/// The `speed` of the first answer is `SPEED_HIGH` for a device running at
/// SuperSpeed too: only the flags tell the two apart, so they are read first.
/// Without them the camera is reported as running at its speed and declaring
/// nothing, which is what the macOS side does for a camera that could not be
/// asked.
fn link_from(speed: u8, flags: Option<u32>) -> Option<UsbLink> {
    let flags = flags.unwrap_or(0);
    let speed = if flags & OPERATING_AT_SUPERSPEED_PLUS != 0 {
        UsbSpeed::SuperPlus
    } else if flags & OPERATING_AT_SUPERSPEED != 0 {
        UsbSpeed::Super
    } else {
        match speed {
            SPEED_LOW => UsbSpeed::Low,
            SPEED_FULL => UsbSpeed::Full,
            SPEED_HIGH => UsbSpeed::High,
            SPEED_SUPER => UsbSpeed::Super,
            _ => return None,
        }
    };
    let capable = if flags & CAPABLE_OF_SUPERSPEED_PLUS != 0 {
        Some(UsbSpeed::SuperPlus)
    } else if flags & CAPABLE_OF_SUPERSPEED != 0 {
        Some(UsbSpeed::Super)
    } else {
        None
    };
    Some(UsbLink { speed, capable })
}

/// The device-instance id the Configuration Manager knows the device behind a
/// USB device-interface path by: the path with its `\\?\` prefix and its
/// trailing `#{interface-class-guid}\reference` removed, and the `#`
/// separators back to the `\` an instance id has.
///
/// `\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{…}\global` is
/// `usb\vid_046d&pid_0893&mi_00\9&56d9c30&0&0000`. `None` for a path that is
/// not a USB device's.
fn instance_id(interface_path: &str) -> Option<String> {
    let path = interface_path
        .strip_prefix(r"\\?\")
        .or_else(|| interface_path.strip_prefix(r"\\.\"))?;
    let id = path.split("#{").next()?.replace('#', r"\");
    id.to_ascii_lowercase().starts_with(r"usb\").then_some(id)
}

/// Whether `instance_id` is one function of a composite device (`&MI_00`)
/// rather than the USB device itself. The port number belongs to the device,
/// which is then the function's parent.
fn is_function(instance_id: &str) -> bool {
    instance_id.to_ascii_lowercase().contains("&mi_")
}

#[cfg(target_os = "windows")]
pub(crate) use hub::observe_links;

/// Everything that talks to Windows. The pure decisions above stay testable
/// without it.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "Configuration Manager and USB hub IOCTL calls to read a camera's link"
)]
mod hub {
    use std::mem::size_of;

    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_DRP_ADDRESS, CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_DevNode_Registry_PropertyW,
        CM_Get_Device_ID_Size, CM_Get_Device_IDW, CM_Get_Device_Interface_List_SizeW,
        CM_Get_Device_Interface_ListW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW,
        CR_SUCCESS,
    };
    use windows::Win32::Devices::Usb::{
        DeviceConnected, GUID_DEVINTERFACE_USB_HUB, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
        IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2, USB_NODE_CONNECTION_INFORMATION_EX,
        USB_NODE_CONNECTION_INFORMATION_EX_V2, USB_PIPE_INFO, USB_PROTOCOLS,
    };
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::core::PCWSTR;

    use super::{instance_id, is_function, link_from};
    use crate::Camera;
    use crate::usb_link::{UsbLink, remember_links};

    /// `USB_PROTOCOLS.Usb300`: the caller understands the V2 answer.
    const USB_300: u32 = 1 << 2;
    /// Pipes' worth of room after the connection record. The hub appends one
    /// `USB_PIPE_INFO` per open pipe and a buffer too small for them can fail
    /// the call, so this is as generous as the SDK's USBView sample.
    const PIPE_ROOM: usize = 30 * size_of::<USB_PIPE_INFO>();

    /// Record the link of every Logitech camera in `cameras`.
    ///
    /// A camera whose link cannot be read is left out, and so reports no link
    /// rather than a wrong one.
    pub(crate) fn observe_links(cameras: &[Camera]) {
        let found = cameras
            .iter()
            .filter(|camera| camera.vendor_id == crate::LOGITECH_VID)
            .filter_map(|camera| Some((camera.unique_id.clone(), link_of(&camera.unique_id)?)))
            .collect();
        remember_links(found);
    }

    /// The link of the camera at `interface_path`.
    fn link_of(interface_path: &str) -> Option<UsbLink> {
        let id = instance_id(interface_path)?;
        let node = dev_node(&id)?;
        let device = if is_function(&id) {
            parent(node)?
        } else {
            node
        };
        let port = port_number(device)?;
        let hub = Hub::open(&hub_interface(parent(device)?)?)?;
        link_from(hub.speed(port)?, hub.superspeed_flags(port))
    }

    /// The Configuration Manager's handle for the present device `id`.
    fn dev_node(id: &str) -> Option<u32> {
        let wide = nul_terminated(id);
        let mut node = 0u32;
        // SAFETY: `wide` is NUL-terminated and outlives the call, and `node`
        // is a valid out-pointer.
        let status = unsafe {
            CM_Locate_DevNodeW(
                &raw mut node,
                PCWSTR(wide.as_ptr()),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        };
        (status == CR_SUCCESS).then_some(node)
    }

    /// The device node above `node`.
    fn parent(node: u32) -> Option<u32> {
        let mut above = 0u32;
        // SAFETY: `above` is a valid out-pointer; the flags must be zero.
        let status = unsafe { CM_Get_Parent(&raw mut above, node, 0) };
        (status == CR_SUCCESS).then_some(above)
    }

    /// The port of its hub that the USB device `node` is attached to.
    fn port_number(node: u32) -> Option<u32> {
        let mut port = 0u32;
        let mut len = u32::try_from(size_of::<u32>()).ok()?;
        // SAFETY: the property is a `DWORD`: `port` is exactly that many bytes
        // and `len` says so. The data-type out-pointer is optional.
        let status = unsafe {
            CM_Get_DevNode_Registry_PropertyW(
                node,
                CM_DRP_ADDRESS,
                None,
                Some((&raw mut port).cast()),
                &raw mut len,
                0,
            )
        };
        (status == CR_SUCCESS).then_some(port)
    }

    /// The instance id of `node`, NUL-terminated.
    fn device_id(node: u32) -> Option<Vec<u16>> {
        let mut len = 0u32;
        // SAFETY: `len` is a valid out-pointer; the flags must be zero.
        let status = unsafe { CM_Get_Device_ID_Size(&raw mut len, node, 0) };
        if status != CR_SUCCESS {
            return None;
        }
        // The size excludes the terminator.
        let mut id = vec![0u16; usize::try_from(len).ok()?.checked_add(1)?];
        // SAFETY: the slice is the buffer the call writes to, and it passes
        // its length so the id cannot overrun it.
        let status = unsafe { CM_Get_Device_IDW(node, &mut id, 0) };
        (status == CR_SUCCESS).then_some(id)
    }

    /// The device-interface path that opens the hub `node`, NUL-terminated.
    fn hub_interface(node: u32) -> Option<Vec<u16>> {
        let id = device_id(node)?;
        let hub_class = GUID_DEVINTERFACE_USB_HUB;
        let mut len = 0u32;
        // SAFETY: the class guid and `id` (NUL-terminated) outlive the call;
        // `len` is a valid out-pointer.
        let status = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &raw mut len,
                &raw const hub_class,
                PCWSTR(id.as_ptr()),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS {
            return None;
        }
        let mut list = vec![0u16; usize::try_from(len).ok()?];
        // SAFETY: as above, and the slice is the buffer the call writes to,
        // with its length passed.
        let status = unsafe {
            CM_Get_Device_Interface_ListW(
                &raw const hub_class,
                PCWSTR(id.as_ptr()),
                &mut list,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS {
            return None;
        }
        // The answer is a list of paths, each NUL-terminated and the list
        // ending in an empty one. A hub has one interface: take the first.
        let end = list.iter().position(|&unit| unit == 0).filter(|&n| n > 0)?;
        list.truncate(end + 1);
        Some(list)
    }

    /// `text` as UTF-16 with the terminator Windows APIs read to.
    fn nul_terminated(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// An open USB hub, closed on drop.
    struct Hub(HANDLE);

    impl Hub {
        /// Open the hub at `interface_path` (NUL-terminated) to query it.
        fn open(interface_path: &[u16]) -> Option<Self> {
            // SAFETY: `interface_path` is NUL-terminated and outlives the call.
            // A hub is opened for a query only: write access is what its
            // IOCTLs ask for, shared, and nothing is written to it.
            unsafe {
                CreateFileW(
                    PCWSTR(interface_path.as_ptr()),
                    GENERIC_WRITE.0,
                    FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            }
            .ok()
            .map(Self)
        }

        /// The `USB_DEVICE_SPEED` of the device on `port`, when one is
        /// connected there.
        fn speed(&self, port: u32) -> Option<u8> {
            /// The connection record, and the room for the pipes after it.
            #[repr(C)]
            struct Connection {
                info: USB_NODE_CONNECTION_INFORMATION_EX,
                _pipes: [u8; PIPE_ROOM],
            }
            let mut connection = Connection {
                info: USB_NODE_CONNECTION_INFORMATION_EX {
                    ConnectionIndex: port,
                    ..Default::default()
                },
                _pipes: [0; PIPE_ROOM],
            };
            self.query(
                IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
                &raw mut connection,
            )?;
            // By-value reads: the record is packed, so no field is borrowed.
            let status = connection.info.ConnectionStatus;
            (status == DeviceConnected).then_some(connection.info.Speed)
        }

        /// The V2 flags of the device on `port`: whether it runs at, and
        /// whether it is capable of, SuperSpeed and SuperSpeed Plus. `None`
        /// when the hub does not answer the V2 question (before Windows 8).
        fn superspeed_flags(&self, port: u32) -> Option<u32> {
            let mut connection = USB_NODE_CONNECTION_INFORMATION_EX_V2 {
                ConnectionIndex: port,
                Length: u32::try_from(size_of::<USB_NODE_CONNECTION_INFORMATION_EX_V2>()).ok()?,
                SupportedUsbProtocols: USB_PROTOCOLS { ul: USB_300 },
                ..Default::default()
            };
            self.query(
                IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2,
                &raw mut connection,
            )?;
            // SAFETY: both union views are a `u32`, and any bits are valid.
            Some(unsafe { connection.Flags.ul })
        }

        /// One synchronous IOCTL with `record` as both its input and its
        /// output, the shape both connection queries take.
        fn query<T>(&self, code: u32, record: *mut T) -> Option<()> {
            let len = u32::try_from(size_of::<T>()).ok()?;
            let mut returned = 0u32;
            // SAFETY: `record` points at a live `T` of `len` bytes, valid for
            // the hub to read and write; `returned` is a valid out-pointer; no
            // overlapped I/O is requested, so the call is done when it returns.
            unsafe {
                DeviceIoControl(
                    self.0,
                    code,
                    Some(record.cast_const().cast()),
                    len,
                    Some(record.cast()),
                    len,
                    Some(&raw mut returned),
                    None,
                )
            }
            .ok()
        }
    }

    impl Drop for Hub {
        fn drop(&mut self) {
            // SAFETY: the handle is the one `open` returned, closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same StreamCam function, as DirectShow lists it.
    const STREAMCAM: &str = r"\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";

    #[test]
    fn a_device_path_is_its_instance_id() {
        assert_eq!(
            instance_id(STREAMCAM).as_deref(),
            Some(r"usb\vid_046d&pid_0893&mi_00\9&56d9c30&0&0000")
        );
        // A device that reports a serial puts it where the instance goes.
        let serial = r"\\?\USB#VID_046D&PID_0893&MI_00#6B123456#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert_eq!(
            instance_id(serial).as_deref(),
            Some(r"USB\VID_046D&PID_0893&MI_00\6B123456")
        );
    }

    #[test]
    fn only_a_usb_device_path_has_an_instance_to_look_up() {
        assert_eq!(instance_id("not-a-device-path"), None);
        assert_eq!(
            instance_id(r"\\?\root#camera#0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global"),
            None,
            "not on the USB bus"
        );
        assert_eq!(instance_id(r"\\?\usbhub#x#{guid}"), None, "a different bus");
    }

    #[test]
    fn a_function_of_a_composite_device_is_told_from_the_device() {
        assert!(is_function(r"usb\vid_046d&pid_0893&mi_00\9&56d9c30&0&0000"));
        assert!(is_function(r"USB\VID_046D&PID_0893&MI_02\6B123456"));
        assert!(!is_function(r"USB\VID_046D&PID_0825\6B123456"));
    }

    #[test]
    fn a_hub_speed_is_the_speed_class_of_the_link() {
        let link = |speed| link_from(speed, None).map(|link| link.speed);
        assert_eq!(link(SPEED_LOW), Some(UsbSpeed::Low));
        assert_eq!(link(SPEED_FULL), Some(UsbSpeed::Full));
        assert_eq!(link(SPEED_HIGH), Some(UsbSpeed::High));
        assert_eq!(link(SPEED_SUPER), Some(UsbSpeed::Super));
        assert_eq!(link(4), None, "not a speed a hub reports");
    }

    #[test]
    fn a_camera_on_a_usb2_port_that_declares_superspeed_could_do_more() {
        // What a StreamCam on a USB 2.0 hub reports: high speed, capable of
        // SuperSpeed, not running at it.
        let link = link_from(SPEED_HIGH, Some(CAPABLE_OF_SUPERSPEED)).unwrap();
        assert_eq!(link.speed, UsbSpeed::High);
        assert_eq!(link.capable, Some(UsbSpeed::Super));
        assert_eq!(link.faster_available(), Some(UsbSpeed::Super));
    }

    #[test]
    fn a_camera_running_at_what_it_declares_is_not_limited() {
        // Running at SuperSpeed sets both of the first two bits, and the hub
        // still reports its speed as high: that answer stops there.
        let link = link_from(SPEED_HIGH, Some(0b11)).unwrap();
        assert_eq!(link.speed, UsbSpeed::Super);
        assert_eq!(link.capable, Some(UsbSpeed::Super));
        assert_eq!(link.faster_available(), None);
    }

    #[test]
    fn the_flags_say_superspeed_even_when_the_hub_speed_does_not() {
        let link = link_from(SPEED_HIGH, Some(OPERATING_AT_SUPERSPEED)).unwrap();
        assert_eq!(link.speed, UsbSpeed::Super);

        // The hub's own speed still counts when the flags are silent.
        let link = link_from(SPEED_SUPER, Some(0)).unwrap();
        assert_eq!(link.speed, UsbSpeed::Super);
    }

    #[test]
    fn superspeed_plus_outranks_superspeed() {
        let running = link_from(SPEED_HIGH, Some(0b1111)).unwrap();
        assert_eq!(running.speed, UsbSpeed::SuperPlus);
        assert_eq!(running.capable, Some(UsbSpeed::SuperPlus));

        // Capable of 10 Gbps on a 5 Gbps link.
        let slower = link_from(SPEED_HIGH, Some(0b1011)).unwrap();
        assert_eq!(slower.speed, UsbSpeed::Super);
        assert_eq!(slower.faster_available(), Some(UsbSpeed::SuperPlus));
    }

    #[test]
    fn a_hub_that_could_not_be_asked_claims_nothing_more_than_the_speed() {
        let link = link_from(SPEED_HIGH, None).unwrap();
        assert_eq!(link.speed, UsbSpeed::High);
        assert_eq!(link.capable, None);
        assert_eq!(link.faster_available(), None);
    }

    #[test]
    fn a_camera_that_declares_nothing_beyond_usb2_declares_nothing() {
        let link = link_from(SPEED_HIGH, Some(0)).unwrap();
        assert_eq!(link.capable, None);
    }
}
