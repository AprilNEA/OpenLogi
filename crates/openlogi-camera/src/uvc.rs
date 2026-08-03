//! Device-level UVC Processing-Unit controls (brightness/contrast/…) over IOKit.
//!
//! These are *not* AVFoundation settings: they're USB Video Class control
//! transfers to the camera's Processing Unit, so a change lands in the camera's
//! own registers and is seen by every app — Google Meet, Zoom, OBS — not just
//! our preview. This is the same mechanism `uvc-util` and "Webcam Settings" use,
//! and it works while the camera is streaming because the request rides the
//! default control endpoint, which the streaming driver does not own.
//!
//! Flow: match the USB device by vendor/product id (disambiguating on the
//! AVFoundation `unique_id`'s location id when several identical cameras are
//! attached), open it via the IOKit `IOUSBDeviceInterface` plug-in, parse the
//! configuration descriptor for the VideoControl interface number and the
//! Processing-Unit id, then issue UVC `GET_*`/`SET_CUR` requests.

#![expect(
    unsafe_code,
    reason = "IOKit USB (IOUSBDeviceInterface) control-transfer FFI for UVC Processing-Unit controls"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "UVC payloads are bounded 16-bit values copied verbatim"
)]

use std::ffi::{CString, c_void};
use std::ptr;

/// Which UVC entity a control request addresses: the Camera Terminal (lens:
/// zoom/focus/exposure) or the Processing Unit (image: brightness/…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    CameraTerminal,
    Processing,
}

pub use crate::controls::{
    AutoState, AutoToggle, CameraControl, CameraState, ControlError, ControlRange,
};

impl CameraControl {
    fn unit(self) -> Unit {
        match self {
            Self::Zoom | Self::Focus | Self::Exposure => Unit::CameraTerminal,
            _ => Unit::Processing,
        }
    }

    /// UVC control selector (Camera Terminal §A.9.4, Processing Unit §A.9.5).
    #[allow(
        clippy::match_same_arms,
        reason = "Focus (CT) and Tint (PU) share 0x06 by coincidence — they address different units"
    )]
    fn selector(self) -> u16 {
        match self {
            Self::Zoom => 0x0B,         // CT_ZOOM_ABSOLUTE_CONTROL
            Self::Focus => 0x06,        // CT_FOCUS_ABSOLUTE_CONTROL
            Self::Exposure => 0x04,     // CT_EXPOSURE_TIME_ABSOLUTE_CONTROL
            Self::Brightness => 0x02,   // PU_BRIGHTNESS_CONTROL
            Self::Contrast => 0x03,     // PU_CONTRAST_CONTROL
            Self::Saturation => 0x07,   // PU_SATURATION_CONTROL
            Self::Sharpness => 0x08,    // PU_SHARPNESS_CONTROL
            Self::WhiteBalance => 0x0A, // PU_WHITE_BALANCE_TEMPERATURE_CONTROL
            Self::Tint => 0x06,         // PU_HUE_CONTROL
        }
    }

    /// Payload size in bytes (exposure time is a dwExposureTimeAbsolute u32).
    fn len(self) -> usize {
        match self {
            Self::Exposure => 4,
            _ => 2,
        }
    }

    /// Brightness and hue are signed controls; the rest are unsigned.
    fn signed(self) -> bool {
        matches!(self, Self::Brightness | Self::Tint)
    }
}

impl AutoToggle {
    fn unit(self) -> Unit {
        match self {
            Self::Focus | Self::Exposure => Unit::CameraTerminal,
            Self::WhiteBalance => Unit::Processing,
        }
    }

    fn selector(self) -> u16 {
        match self {
            Self::Focus => 0x08,        // CT_FOCUS_AUTO_CONTROL
            Self::Exposure => 0x02,     // CT_AE_MODE_CONTROL
            Self::WhiteBalance => 0x0B, // PU_WHITE_BALANCE_TEMPERATURE_AUTO_CONTROL
        }
    }
}

// ── UVC constants ────────────────────────────────────────────────────────────
const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_MIN: u8 = 0x82;
const UVC_GET_MAX: u8 = 0x83;
const UVC_GET_DEF: u8 = 0x87;
// bmRequestType: class request to an interface recipient. Bit 7 = data direction.
const RT_GET: u8 = 0xA1; // device-to-host | class | interface
const RT_SET: u8 = 0x21; // host-to-device | class | interface

const CC_VIDEO: u8 = 0x0E;
const SC_VIDEOCONTROL: u8 = 0x01;
const DESC_INTERFACE: u8 = 0x04;
const DESC_CS_INTERFACE: u8 = 0x24;
const VC_INPUT_TERMINAL: u8 = 0x02;
const VC_PROCESSING_UNIT: u8 = 0x05;
/// wTerminalType for a camera sensor input terminal (ITT_CAMERA).
const ITT_CAMERA: u16 = 0x0201;

// UVC AE-mode bitmap bits (CT_AE_MODE_CONTROL): everything except fully
// manual counts as "auto" for the toggle.
const AE_MANUAL: u8 = 0x01;
/// Auto modes to try when enabling auto-exposure, most- to least-automatic
/// (full auto, aperture priority, shutter priority) — cameras support subsets.
const AE_AUTO_MODES: [u8; 3] = [0x02, 0x08, 0x04];

const KIO_RETURN_SUCCESS: i32 = 0;

/// Hold the process-wide seize/enumeration lock — see [`crate::USB_QUIESCE`].
fn quiesce() -> std::sync::MutexGuard<'static, ()> {
    crate::USB_QUIESCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Read a control's min/max/default/current straight from the device.
///
/// # Errors
/// [`ControlError::NotFound`] when no USB device matches, [`ControlError::Io`]
/// on an IOKit failure, or [`ControlError::Unsupported`] if the camera NAKs the
/// request.
pub fn control_range(
    unique_id: &str,
    control: CameraControl,
) -> Result<ControlRange, ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    let min = dev.get(control, UVC_GET_MIN)?;
    let max = dev.get(control, UVC_GET_MAX)?;
    let default = dev.get(control, UVC_GET_DEF)?;
    let current = dev.get(control, UVC_GET_CUR).unwrap_or(default);
    Ok(ControlRange {
        min,
        max,
        default,
        current,
    })
}

/// Read every supported control in a single device-open (controls the camera
/// NAKs are skipped). Batching keeps the device-seize count down — important
/// while the camera is streaming.
///
/// # Errors
/// [`ControlError::NotFound`] when no USB device matches.
pub fn control_ranges(unique_id: &str) -> Result<Vec<(CameraControl, ControlRange)>, ControlError> {
    Ok(read_camera_state(unique_id)?.controls)
}

/// Read every supported control range *and* auto-toggle state in a single
/// device-open — what the GUI controls panel builds itself from.
///
/// # Errors
/// [`ControlError::NotFound`] when no USB device matches.
pub fn read_camera_state(unique_id: &str) -> Result<CameraState, ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    let mut state = CameraState::default();
    for control in CameraControl::ALL {
        if let (Ok(min), Ok(max), Ok(default)) = (
            dev.get(control, UVC_GET_MIN),
            dev.get(control, UVC_GET_MAX),
            dev.get(control, UVC_GET_DEF),
        ) {
            let current = dev.get(control, UVC_GET_CUR).unwrap_or(default);
            state.controls.push((
                control,
                ControlRange {
                    min,
                    max,
                    default,
                    current,
                },
            ));
        }
    }
    for toggle in AutoToggle::ALL {
        if let (Ok(current), Ok(default)) = (
            dev.get_auto(toggle, UVC_GET_CUR),
            dev.get_auto(toggle, UVC_GET_DEF),
        ) {
            state.autos.push((toggle, AutoState { current, default }));
        }
    }
    Ok(state)
}

/// Write a control's current value to the device. Persists in the camera's
/// registers, so other apps observe it too.
///
/// # Errors
/// As [`control_range`].
pub fn set_control(
    unique_id: &str,
    control: CameraControl,
    value: i32,
) -> Result<(), ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    dev.set(control, value)
}

/// Switch an auto mode (focus / exposure / white balance) on or off.
///
/// # Errors
/// As [`control_range`].
pub fn set_auto(unique_id: &str, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    dev.set_auto(toggle, on)
}

/// Apply a batch of auto toggles and control values in a single device-open —
/// how profiles and saved-state reapplication write, so the seize count stays
/// at one no matter how many controls change. Autos land first so a manual
/// value isn't rejected by a still-armed auto mode. Every write is attempted
/// (one rejection doesn't abandon the rest), but any failure surfaces so
/// callers never persist or display a batch the hardware didn't take.
///
/// # Errors
/// [`ControlError::NotFound`] when no USB device matches; otherwise the first
/// per-write error after attempting the whole batch.
pub fn apply_settings(
    unique_id: &str,
    autos: &[(AutoToggle, bool)],
    values: &[(CameraControl, i32)],
) -> Result<(), ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    let mut first_err = None;
    for (toggle, on) in autos {
        if let Err(e) = dev.set_auto(*toggle, *on) {
            first_err.get_or_insert(e);
        }
    }
    for (control, value) in values {
        if let Err(e) = dev.set(*control, *value) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

// ── AVFoundation unique-id → USB location id ─────────────────────────────────
// macOS UVC `uniqueID`s are `<location hex><vid %04x><pid %04x>` — but the
// location comes out *unpadded* (a StreamCam on bus 0x01123000 yields
// `0x1123000046d0893`, 15 digits). So the location is everything **except**
// the trailing 8 vid+pid digits; taking a fixed leading 8 would swallow a
// nibble of the vid and shift the location. Only used to pick between two
// identical cameras; matching is primarily by vendor id.
fn location_hint(unique_id: &str) -> Option<u32> {
    let hex = unique_id.strip_prefix("0x").unwrap_or(unique_id);
    let location = hex.get(..hex.len().checked_sub(8)?)?;
    if location.is_empty() {
        return None;
    }
    u32::from_str_radix(location, 16).ok()
}

// ── IOKit / CoreFoundation FFI ───────────────────────────────────────────────
type IoReturn = i32;
type IoService = u32;
type IoIterator = u32;
type CfUuidRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CfUuidBytes {
    bytes: [u8; 16],
}

#[repr(C)]
struct IoUsbDevRequest {
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
    p_data: *mut c_void,
    w_len_done: u32,
}

// IOCFPlugInInterface — we only need the IUnknown head (QueryInterface/Release).
#[repr(C)]
struct PlugInInterface {
    _reserved: *mut c_void,
    query_interface: extern "C" fn(*mut c_void, CfUuidBytes, *mut *mut c_void) -> i32,
    add_ref: extern "C" fn(*mut c_void) -> u32,
    release: extern "C" fn(*mut c_void) -> u32,
}

// IOUSBDeviceInterface vtable (IOUSBLib.h). Slots we don't call are typed as
// opaque pointers so the offsets of the ones we *do* call stay correct.
#[repr(C)]
struct UsbDeviceInterface {
    _reserved: *mut c_void,
    query_interface: extern "C" fn(*mut c_void, CfUuidBytes, *mut *mut c_void) -> i32,
    add_ref: extern "C" fn(*mut c_void) -> u32,
    release: extern "C" fn(*mut c_void) -> u32,
    create_device_async_event_source: *const c_void,
    get_device_async_event_source: *const c_void,
    create_device_async_port: *const c_void,
    get_device_async_port: *const c_void,
    usb_device_open: extern "C" fn(*mut c_void) -> IoReturn,
    usb_device_close: extern "C" fn(*mut c_void) -> IoReturn,
    get_device_class: *const c_void,
    get_device_sub_class: *const c_void,
    get_device_protocol: *const c_void,
    get_device_vendor: extern "C" fn(*mut c_void, *mut u16) -> IoReturn,
    get_device_product: extern "C" fn(*mut c_void, *mut u16) -> IoReturn,
    get_device_release_number: *const c_void,
    get_device_address: *const c_void,
    get_device_bus_power_available: *const c_void,
    get_device_speed: *const c_void,
    get_number_of_configurations: extern "C" fn(*mut c_void, *mut u8) -> IoReturn,
    get_location_id: extern "C" fn(*mut c_void, *mut u32) -> IoReturn,
    get_configuration_descriptor_ptr: extern "C" fn(*mut c_void, u8, *mut *const u8) -> IoReturn,
    get_configuration: *const c_void,
    set_configuration: *const c_void,
    get_bus_frame_number: *const c_void,
    reset_device: *const c_void,
    device_request: extern "C" fn(*mut c_void, *mut IoUsbDevRequest) -> IoReturn,
    device_request_async: *const c_void,
    create_interface_iterator: *const c_void,
    // IOUSBDeviceInterface182 adds OpenSeize: open even while the kernel video
    // driver holds the device for streaming, so controls work during a preview.
    usb_device_open_seize: extern "C" fn(*mut c_void) -> IoReturn,
    // remaining methods unused
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    static kIOMainPortDefault: u32;
    fn IOServiceMatching(name: *const i8) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut IoIterator,
    ) -> IoReturn;
    fn IOIteratorNext(iterator: IoIterator) -> IoService;
    fn IOObjectRelease(object: u32) -> IoReturn;
    fn IOCreatePlugInInterfaceForService(
        service: IoService,
        plugin_type: CfUuidRef,
        interface_type: CfUuidRef,
        plug_in: *mut *mut *mut PlugInInterface,
        score: *mut i32,
    ) -> IoReturn;
    fn IODestroyPlugInInterface(interface: *mut *mut PlugInInterface) -> IoReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDCreateFromUUIDBytes(allocator: *const c_void, bytes: CfUuidBytes) -> CfUuidRef;
    fn CFUUIDGetUUIDBytes(uuid: CfUuidRef) -> CfUuidBytes;
    fn CFRelease(cf: *const c_void);
}

// IOUSBLib UUIDs, as raw bytes (UUID order).
const KIO_USB_DEVICE_USER_CLIENT_TYPE_ID: [u8; 16] = [
    0x9d, 0xc7, 0xb7, 0x80, 0x9e, 0xc0, 0x11, 0xd4, 0xa5, 0x4f, 0x00, 0x0a, 0x27, 0x05, 0x28, 0x61,
];
const KIO_CF_PLUGIN_INTERFACE_ID: [u8; 16] = [
    0xc2, 0x44, 0xe8, 0x58, 0x10, 0x9c, 0x11, 0xd4, 0x91, 0xd4, 0x00, 0x50, 0xe4, 0xc6, 0x42, 0x6f,
];
// kIOUSBDeviceInterfaceID182 — the first version exposing USBDeviceOpenSeize.
const KIO_USB_DEVICE_INTERFACE_ID: [u8; 16] = [
    0x15, 0x2f, 0xc4, 0x96, 0x48, 0x91, 0x11, 0xd5, 0x9d, 0x52, 0x00, 0x0a, 0x27, 0x80, 0x1e, 0x86,
];

/// An opened IOKit USB device interface, with its UVC topology resolved. Closes
/// and releases on drop.
struct UsbDevice {
    dev: *mut *mut UsbDeviceInterface,
    vc_interface: u8,
    /// Processing-Unit id (image controls).
    unit_id: u8,
    /// Camera (input) Terminal id (lens controls); `None` when the descriptor
    /// lists no camera terminal — lens controls then report `Unsupported`.
    terminal_id: Option<u8>,
}

impl UsbDevice {
    /// Find and open the Logitech USB device backing `unique_id`, resolving its
    /// VideoControl interface and Processing-Unit id.
    fn open_for(unique_id: &str) -> Result<Self, ControlError> {
        let want_vid = crate::LOGITECH_VID;
        // The pid is the trailing 4 hex of the uniqueID's id portion; we don't
        // strictly need it for matching (we open every Logitech UVC device and
        // pick the one whose location matches), but parse it as a fallback.
        let want_location = location_hint(unique_id);

        // SAFETY: standard IOKit device enumeration; each retained object is
        // released, and the matching dictionary is consumed by the call.
        unsafe {
            let class = CString::new("IOUSBDevice").map_err(|e| ControlError::Io(e.to_string()))?;
            let matching = IOServiceMatching(class.as_ptr());
            if matching.is_null() {
                return Err(ControlError::Io("IOServiceMatching".into()));
            }
            let mut iter: IoIterator = 0;
            if IOServiceGetMatchingServices(kIOMainPortDefault, matching, &raw mut iter)
                != KIO_RETURN_SUCCESS
            {
                return Err(ControlError::Io("IOServiceGetMatchingServices".into()));
            }

            let mut chosen: Option<Opened> = None;
            // Count Logitech cameras reached on the location-less path. With a
            // parseable location only an exact match opens; without a hint (an
            // unparseable unique id) the first Logitech camera is a best effort
            // that is only safe when it's the *only* one — see the fail-closed
            // check after the loop.
            let mut vendor_candidates = 0usize;
            loop {
                let service = IOIteratorNext(iter);
                if service == 0 {
                    break;
                }
                match Self::try_open(service, want_vid, want_location) {
                    Some(found) => {
                        let exact =
                            want_location.is_some_and(|l| found.matched_location == Some(l));
                        IOObjectRelease(service);
                        if exact {
                            if let Some(prev) = chosen.take() {
                                prev.into_device().close();
                            }
                            chosen = Some(found);
                            break;
                        } else if want_location.is_none() {
                            vendor_candidates += 1;
                            if chosen.is_none() {
                                chosen = Some(found);
                            } else {
                                found.into_device().close();
                            }
                        } else {
                            found.into_device().close();
                        }
                    }
                    None => {
                        IOObjectRelease(service);
                    }
                }
            }
            IOObjectRelease(iter);

            // A location-less match is only unambiguous with exactly one
            // Logitech camera attached; with two (and a unique id we couldn't
            // parse into a USB location) we can't tell them apart, so refuse
            // rather than write the wrong camera's registers.
            if want_location.is_none() && vendor_candidates > 1 {
                if let Some(dev) = chosen.take() {
                    dev.into_device().close();
                }
                return Err(ControlError::Ambiguous);
            }

            chosen
                .map(Opened::into_device)
                .ok_or(ControlError::NotFound)
        }
    }

    /// The entity id addressed for `unit`, or `Unsupported` when the camera's
    /// descriptor lists no camera terminal.
    fn entity(&self, unit: Unit) -> Result<u8, ControlError> {
        match unit {
            Unit::Processing => Ok(self.unit_id),
            Unit::CameraTerminal => self.terminal_id.ok_or(ControlError::Unsupported),
        }
    }

    /// Issue a UVC GET request (`req` = GET_MIN/MAX/DEF/CUR), returning the
    /// control-sized little-endian value, sign-extended per the control.
    fn get(&self, control: CameraControl, req: u8) -> Result<i32, ControlError> {
        let entity = self.entity(control.unit())?;
        let mut buf = [0u8; 4];
        let len = control.len();
        self.transfer(RT_GET, req, control.selector(), entity, &mut buf[..len])?;
        Ok(match (len, control.signed()) {
            (4, _) => i32::try_from(u32::from_le_bytes(buf)).unwrap_or(i32::MAX),
            (_, true) => i32::from(i16::from_le_bytes([buf[0], buf[1]])),
            (_, false) => i32::from(u16::from_le_bytes([buf[0], buf[1]])),
        })
    }

    /// Issue a UVC SET_CUR request with `value` truncated to the control's size.
    fn set(&self, control: CameraControl, value: i32) -> Result<(), ControlError> {
        let entity = self.entity(control.unit())?;
        let mut buf = (value as u32).to_le_bytes();
        let len = control.len();
        self.transfer(
            RT_SET,
            UVC_SET_CUR,
            control.selector(),
            entity,
            &mut buf[..len],
        )
    }

    /// Read an auto toggle (`req` = GET_CUR/GET_DEF) as a boolean. For the
    /// AE-mode bitmap anything but fully-manual counts as auto.
    fn get_auto(&self, toggle: AutoToggle, req: u8) -> Result<bool, ControlError> {
        let entity = self.entity(toggle.unit())?;
        let mut buf = [0u8; 1];
        self.transfer(RT_GET, req, toggle.selector(), entity, &mut buf)?;
        Ok(match toggle {
            AutoToggle::Exposure => buf[0] != AE_MANUAL,
            _ => buf[0] != 0,
        })
    }

    /// Switch an auto toggle. Enabling auto-exposure tries each AE mode the
    /// camera might support, most-automatic first.
    fn set_auto(&self, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
        let entity = self.entity(toggle.unit())?;
        let selector = toggle.selector();
        let candidates: &[u8] = match (toggle, on) {
            (AutoToggle::Exposure, true) => &AE_AUTO_MODES,
            (AutoToggle::Exposure, false) => &[AE_MANUAL],
            (_, true) => &[1],
            (_, false) => &[0],
        };
        let mut last = ControlError::Unsupported;
        for &mode in candidates {
            match self.transfer(RT_SET, UVC_SET_CUR, selector, entity, &mut [mode]) {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn transfer(
        &self,
        request_type: u8,
        request: u8,
        selector: u16,
        entity: u8,
        data: &mut [u8],
    ) -> Result<(), ControlError> {
        let mut req = IoUsbDevRequest {
            bm_request_type: request_type,
            b_request: request,
            w_value: selector << 8,
            w_index: (u16::from(entity) << 8) | u16::from(self.vc_interface),
            w_length: data.len() as u16,
            p_data: data.as_mut_ptr().cast::<c_void>(),
            w_len_done: 0,
        };
        // SAFETY: `self.dev` is a live IOUSBDeviceInterface**; DeviceRequest reads
        // `req` and writes into the `data` buffer it points at.
        let rc = unsafe { ((**self.dev).device_request)(self.dev.cast::<c_void>(), &raw mut req) };
        if rc != KIO_RETURN_SUCCESS {
            return Err(ControlError::Unsupported);
        }
        Ok(())
    }

    fn close(self) {
        // Drop handles the teardown.
        drop(self);
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        // SAFETY: `self.dev` is a live interface we opened; close then release it.
        unsafe {
            let _ = ((**self.dev).usb_device_close)(self.dev.cast::<c_void>());
            ((**self.dev).release)(self.dev.cast::<c_void>());
        }
    }
}

/// A device that matched on vendor id, carrying the location id it reported so
/// the caller can prefer an exact-location match.
struct Opened {
    device: UsbDevice,
    matched_location: Option<u32>,
}

impl Opened {
    fn into_device(self) -> UsbDevice {
        self.device
    }
}

impl UsbDevice {
    /// Try to build an [`Opened`] from an `io_service_t`: query the device
    /// interface, match the vendor id, open it, and resolve its UVC topology.
    unsafe fn try_open(
        service: IoService,
        want_vid: u16,
        _want_location: Option<u32>,
    ) -> Option<Opened> {
        unsafe {
            let user_client = make_uuid(&KIO_USB_DEVICE_USER_CLIENT_TYPE_ID);
            let plugin_type = make_uuid(&KIO_CF_PLUGIN_INTERFACE_ID);
            let mut plugin: *mut *mut PlugInInterface = ptr::null_mut();
            let mut score: i32 = 0;
            let rc = IOCreatePlugInInterfaceForService(
                service,
                user_client,
                plugin_type,
                &raw mut plugin,
                &raw mut score,
            );
            CFRelease(user_client);
            CFRelease(plugin_type);
            if rc != KIO_RETURN_SUCCESS || plugin.is_null() {
                return None;
            }

            let dev_uuid_ref = make_uuid(&KIO_USB_DEVICE_INTERFACE_ID);
            let dev_uuid = CFUUIDGetUUIDBytes(dev_uuid_ref);
            CFRelease(dev_uuid_ref);
            let mut dev_ptr: *mut c_void = ptr::null_mut();
            let qrc =
                ((**plugin).query_interface)(plugin.cast::<c_void>(), dev_uuid, &raw mut dev_ptr);
            IODestroyPlugInInterface(plugin);
            if qrc != 0 || dev_ptr.is_null() {
                return None;
            }
            let dev = dev_ptr.cast::<*mut UsbDeviceInterface>();

            let mut vid: u16 = 0;
            ((**dev).get_device_vendor)(dev.cast::<c_void>(), &raw mut vid);
            if vid != want_vid {
                ((**dev).release)(dev.cast::<c_void>());
                return None;
            }

            let mut location: u32 = 0;
            let loc_ok = ((**dev).get_location_id)(dev.cast::<c_void>(), &raw mut location)
                == KIO_RETURN_SUCCESS;

            // Seize (not plain open) so a control transfer succeeds even while
            // the camera is streaming in this or another app. Callers batch their
            // reads/writes into one open to keep this churn low.
            if ((**dev).usb_device_open_seize)(dev.cast::<c_void>()) != KIO_RETURN_SUCCESS {
                ((**dev).release)(dev.cast::<c_void>());
                return None;
            }

            let Some(topology) = video_control_topology(dev) else {
                let _ = ((**dev).usb_device_close)(dev.cast::<c_void>());
                ((**dev).release)(dev.cast::<c_void>());
                return None;
            };

            Some(Opened {
                device: UsbDevice {
                    dev,
                    vc_interface: topology.vc_interface,
                    unit_id: topology.processing_unit,
                    terminal_id: topology.camera_terminal,
                },
                matched_location: loc_ok.then_some(location),
            })
        }
    }
}

/// The VideoControl entities a control request can address, parsed from the
/// configuration descriptor.
struct VcTopology {
    vc_interface: u8,
    processing_unit: u8,
    camera_terminal: Option<u8>,
}

/// Parse the configuration descriptor for the VideoControl interface number,
/// the Processing-Unit id, and the camera (input) terminal id.
unsafe fn video_control_topology(dev: *mut *mut UsbDeviceInterface) -> Option<VcTopology> {
    unsafe {
        let mut num_configs: u8 = 0;
        ((**dev).get_number_of_configurations)(dev.cast::<c_void>(), &raw mut num_configs);
        for cfg in 0..num_configs {
            let mut desc: *const u8 = ptr::null();
            if ((**dev).get_configuration_descriptor_ptr)(dev.cast::<c_void>(), cfg, &raw mut desc)
                != KIO_RETURN_SUCCESS
                || desc.is_null()
            {
                continue;
            }
            // wTotalLength at offset 2..4 (little-endian).
            let total = u16::from(*desc.add(2)) | (u16::from(*desc.add(3)) << 8);
            if let Some(found) = scan_descriptors(desc, total as usize) {
                return Some(found);
            }
        }
        None
    }
}

/// Walk a configuration descriptor blob, collecting the first VideoControl
/// interface's Processing-Unit and camera-terminal entity ids.
unsafe fn scan_descriptors(base: *const u8, total: usize) -> Option<VcTopology> {
    unsafe {
        let mut off = 0usize;
        let mut vc_interface: Option<u8> = None;
        let mut camera_terminal: Option<u8> = None;
        while off + 2 <= total {
            let len = *base.add(off) as usize;
            if len < 2 || off + len > total {
                break;
            }
            let dtype = *base.add(off + 1);
            if dtype == DESC_INTERFACE && len >= 9 {
                let class = *base.add(off + 5);
                let sub = *base.add(off + 6);
                vc_interface = (class == CC_VIDEO && sub == SC_VIDEOCONTROL)
                    .then(|| *base.add(off + 2))
                    .or(vc_interface);
            } else if dtype == DESC_CS_INTERFACE && len >= 4 && vc_interface.is_some() {
                let subtype = *base.add(off + 2);
                // bUnitID / bTerminalID sit at offset 3 in both descriptors;
                // an input terminal's wTerminalType (offset 4..6) must be the
                // camera sensor — skip composite/other input terminals.
                if subtype == VC_INPUT_TERMINAL && len >= 8 {
                    let ttype =
                        u16::from(*base.add(off + 4)) | (u16::from(*base.add(off + 5)) << 8);
                    if ttype == ITT_CAMERA && camera_terminal.is_none() {
                        camera_terminal = Some(*base.add(off + 3));
                    }
                } else if subtype == VC_PROCESSING_UNIT {
                    return vc_interface.map(|vc| VcTopology {
                        vc_interface: vc,
                        processing_unit: *base.add(off + 3),
                        camera_terminal,
                    });
                }
            }
            off += len;
        }
        None
    }
}

unsafe fn make_uuid(bytes: &[u8; 16]) -> CfUuidRef {
    unsafe { CFUUIDCreateFromUUIDBytes(ptr::null(), CfUuidBytes { bytes: *bytes }) }
}

#[cfg(test)]
mod tests {
    use super::location_hint;

    /// AVFoundation prints the location id unpadded: a StreamCam on bus
    /// 0x01123000 yields a 15-digit id whose leading run is only 7 digits.
    /// Taking a fixed 8 would swallow a vid nibble and shift the location —
    /// which made every control write fail closed with `NotFound` (the bug
    /// the exact-match requirement exposed).
    #[test]
    fn unpadded_location_parses() {
        assert_eq!(location_hint("0x1123000046d0893"), Some(0x0112_3000));
    }

    #[test]
    fn padded_location_parses() {
        assert_eq!(location_hint("0x14110000046d082d"), Some(0x1411_0000));
    }

    #[test]
    fn too_short_ids_yield_no_hint() {
        assert_eq!(location_hint("0x46d0893"), None);
        assert_eq!(location_hint("46d0893"), None);
        assert_eq!(location_hint(""), None);
    }
}
