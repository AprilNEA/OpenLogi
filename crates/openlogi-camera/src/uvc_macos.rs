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
//! configuration descriptor for the VideoControl topology (see
//! [`crate::uvc_descriptor`]), then issue UVC `GET_*`/`SET_CUR` requests.
//!
//! The IOKit handles themselves live in [`iokit`], which owns every `unsafe`
//! block in this backend and hands the descriptor up as a plain `&[u8]`.
//! What speed each camera's USB link runs at, as enumeration sees it, is
//! [`link`]'s.

mod iokit;
mod link;
pub(crate) use link::observe_links;

use std::collections::HashMap;
use std::ffi::c_void;

use objc2_core_foundation::{CFNumber, CFString};
use objc2_io_kit::IOUSBDevRequest;

use iokit::{IoObject, SeizedDevice, UsbInterface};

use crate::extension_unit::{BUILT_IN_MICROPHONE, ExtensionControl};
use crate::uvc_descriptor::{VcTopology, scan_descriptors};

/// Which UVC entity a control request addresses: the Camera Terminal (lens:
/// zoom/focus/exposure), the Processing Unit (image: brightness/…), or a
/// vendor Extension Unit (framing: field of view/…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    CameraTerminal,
    Processing,
    Extension(ExtensionControl),
}

pub use crate::controls::{
    AutoState, AutoToggle, CameraControl, CameraState, ControlError, ControlRange,
};

/// The wire type of a control's value: how many bytes it occupies on the bus
/// and how a read is sign-extended. UVC controls have exactly one of these
/// per selector — `len` and `signed` are not independent, so this collapses
/// them into the one combination each control actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Payload {
    /// 1-byte unsigned (menu/enum controls).
    U8,
    /// One half of the 8-byte `CT_PANTILT_ABSOLUTE_CONTROL` pair. Unlike every
    /// other control, pan and tilt share a single selector, so each is
    /// addressed by which word of that payload it occupies.
    PanTilt(Axis),
    /// 2-byte unsigned (most controls).
    U16,
    /// 2-byte signed (brightness, hue).
    I16,
    /// 4-byte unsigned (exposure time, a dwExposureTimeAbsolute).
    U32,
}

impl Payload {
    /// Size in bytes on the wire.
    const fn len(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 => 4,
            Self::PanTilt(_) => PAN_TILT_LEN,
        }
    }
}

/// Which half of the `CT_PANTILT_ABSOLUTE_CONTROL` payload a control addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Pan,
    Tilt,
}

impl Axis {
    /// Byte offset of this axis' `dw` word within the pair.
    const fn offset(self) -> usize {
        match self {
            Self::Pan => 0,
            Self::Tilt => 4,
        }
    }
}

/// Wire size of `CT_PANTILT_ABSOLUTE_CONTROL`: `dwPanAbsolute` followed by
/// `dwTiltAbsolute`, each a little-endian signed 32-bit value (UVC 1.5
/// Â§4.2.2.1.14). It is the widest control payload, so it also sizes the buffer
/// every read uses.
const PAN_TILT_LEN: usize = 8;

/// Read one axis out of a `CT_PANTILT_ABSOLUTE_CONTROL` payload.
fn pan_tilt_half(buf: [u8; PAN_TILT_LEN], axis: Axis) -> i32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&buf[axis.offset()..axis.offset() + 4]);
    i32::from_le_bytes(word)
}

/// Replace one axis in a `CT_PANTILT_ABSOLUTE_CONTROL` payload, leaving the
/// other untouched.
fn patch_pan_tilt(buf: &mut [u8; PAN_TILT_LEN], axis: Axis, value: i32) {
    buf[axis.offset()..axis.offset() + 4].copy_from_slice(&value.to_le_bytes());
}

/// A control's complete UVC wire description: the entity it addresses, its
/// selector, and its payload type.
struct ControlSpec {
    unit: Unit,
    selector: u16,
    payload: Payload,
}

impl CameraControl {
    /// UVC entity, control selector (Camera Terminal §A.9.4, Processing Unit
    /// §A.9.5, or the vendor's Extension Unit table), and wire payload type for
    /// this control.
    const fn spec(self) -> ControlSpec {
        use Payload::{I16, PanTilt, U8, U16, U32};
        use Unit::{CameraTerminal, Processing};
        if let Some(extension) = self.extension() {
            return ControlSpec {
                unit: Unit::Extension(extension),
                selector: extension.selector as u16,
                payload: U8,
            };
        }
        let (unit, selector, payload) = match self {
            Self::Zoom => (CameraTerminal, 0x0B, U16), // CT_ZOOM_ABSOLUTE_CONTROL
            // Both halves of CT_PANTILT_ABSOLUTE_CONTROL.
            Self::Pan => (CameraTerminal, 0x0D, PanTilt(Axis::Pan)),
            Self::Tilt => (CameraTerminal, 0x0D, PanTilt(Axis::Tilt)),
            Self::Focus => (CameraTerminal, 0x06, U16), // CT_FOCUS_ABSOLUTE_CONTROL
            Self::Exposure => (CameraTerminal, 0x04, U32), // CT_EXPOSURE_TIME_ABSOLUTE_CONTROL
            Self::PowerLineFrequency => (Processing, 0x05, U8), // PU_POWER_LINE_FREQUENCY_CONTROL
            Self::LowLightCompensation => (CameraTerminal, 0x03, U8), // CT_AE_PRIORITY_CONTROL
            Self::Brightness => (Processing, 0x02, I16), // PU_BRIGHTNESS_CONTROL
            Self::Contrast => (Processing, 0x03, U16),  // PU_CONTRAST_CONTROL
            Self::Saturation => (Processing, 0x07, U16), // PU_SATURATION_CONTROL
            Self::Sharpness => (Processing, 0x08, U16), // PU_SHARPNESS_CONTROL
            Self::Gain => (Processing, 0x04, U16),      // PU_GAIN_CONTROL
            // PU_BACKLIGHT_COMPENSATION_CONTROL
            Self::BacklightCompensation => (Processing, 0x01, U16),
            Self::WhiteBalance => (Processing, 0x0A, U16), // PU_WHITE_BALANCE_TEMPERATURE_CONTROL
            Self::Tint => (Processing, 0x06, I16),         // PU_HUE_CONTROL
            // `extension()` is `Some` for exactly these, so the early return
            // above already answered them.
            Self::FieldOfView | Self::ShowMode => unreachable!(),
        };
        ControlSpec {
            unit,
            selector,
            payload,
        }
    }
}

/// An auto toggle's complete UVC wire description: the entity it addresses
/// and its selector.
struct ToggleSpec {
    unit: Unit,
    selector: u16,
}

impl AutoToggle {
    /// UVC entity and control selector (Camera Terminal §A.9.4, Processing
    /// Unit §A.9.5) for this auto toggle.
    const fn spec(self) -> ToggleSpec {
        use Unit::{CameraTerminal, Processing};
        let (unit, selector) = match self {
            Self::Focus => (CameraTerminal, 0x08), // CT_FOCUS_AUTO_CONTROL
            Self::Exposure => (CameraTerminal, 0x02), // CT_AE_MODE_CONTROL
            Self::WhiteBalance => (Processing, 0x0B), // PU_WHITE_BALANCE_TEMPERATURE_AUTO_CONTROL
        };
        ToggleSpec { unit, selector }
    }
}

const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_MIN: u8 = 0x82;
const UVC_GET_MAX: u8 = 0x83;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;
const UVC_GET_DEF: u8 = 0x87;
/// `GET_INFO` bits: the control answers `GET_*` and accepts `SET_CUR`.
const INFO_GET_SET: u8 = 0x03;
// bmRequestType: class request to an interface recipient. Bit 7 = data direction.
const RT_GET: u8 = 0xA1; // device-to-host | class | interface
const RT_SET: u8 = 0x21; // host-to-device | class | interface

// UVC AE-mode bitmap bits (CT_AE_MODE_CONTROL): everything except fully
// manual counts as "auto" for the toggle.
const AE_MANUAL: u8 = 0x01;
/// Auto modes to try when enabling auto-exposure, most- to least-automatic
/// (full auto, aperture priority, shutter priority) — cameras support subsets.
const AE_AUTO_MODES: [u8; 3] = [0x02, 0x08, 0x04];

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
    dev.range(control)
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
        if let Ok(range) = dev.range(control) {
            state.controls.push((control, range));
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
    state.microphone = dev
        .extension_range(BUILT_IN_MICROPHONE)
        .ok()
        .map(|range| range.current != 0);
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

/// Switch the camera's built-in microphone on or off.
///
/// The camera stores the switch and acts on it the next time it enumerates;
/// [`reconnect`] makes that happen now.
///
/// # Errors
/// As [`control_range`]; [`ControlError::Unsupported`] for a camera without
/// the switch.
pub fn set_microphone(unique_id: &str, on: bool) -> Result<(), ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    dev.extension_set(BUILT_IN_MICROPHONE, i32::from(on))
}

/// Have the camera drop off the bus and enumerate again, as a replug would.
///
/// macOS lets the process that seized the device ask for this, so it never
/// needs elevation here.
///
/// # Errors
/// [`ControlError::NotFound`] when no USB device matches, or
/// [`ControlError::Io`] when IOKit refuses the request.
pub fn reconnect(unique_id: &str) -> Result<(), ControlError> {
    let _quiesce = quiesce();
    let dev = UsbDevice::open_for(unique_id)?;
    if dev.device.re_enumerate() {
        Ok(())
    } else {
        Err(ControlError::Io("USBDeviceReEnumerate".to_string()))
    }
}

// ── AVFoundation unique-id → USB location id ─────────────────────────────────
// macOS UVC `uniqueID`s are `<location hex><vid %04x><pid %04x>` — but the
// location comes out *unpadded* (a StreamCam on bus 0x01123000 yields
// `0x1123000046d0893`, 15 digits). So the location is everything **except**
// the trailing 8 vid+pid digits; taking a fixed leading 8 would swallow a
// nibble of the vid and shift the location. Only used to pick between two
// identical cameras; matching is primarily by vendor id.
pub(crate) fn location_hint(unique_id: &str) -> Option<u32> {
    let hex = unique_id.strip_prefix("0x").unwrap_or(unique_id);
    let location = hex.get(..hex.len().checked_sub(8)?)?;
    if location.is_empty() {
        return None;
    }
    u32::from_str_radix(location, 16).ok()
}

/// USB `iSerialNumber` for every attached `IOUSBDevice`, keyed by location id.
///
/// Read from the IORegistry only — no device open — so enumeration can prefer
/// the port-stable serial for config keys without racing a control seize.
pub(crate) fn usb_serials_by_location() -> HashMap<u32, String> {
    let serial_key = CFString::from_static_str("USB Serial Number");
    let location_key = CFString::from_static_str("locationID");
    iokit::usb_devices()
        .into_iter()
        .flatten()
        .filter_map(|service| registry_location_and_serial(&service, &serial_key, &location_key))
        .fold(HashMap::new(), |mut serials, (location, serial)| {
            serials.entry(location).or_insert(serial);
            serials
        })
}

/// An `IOUSBDevice` service's location id, without opening the device.
///
/// Prefers the USB device interface (it is what the control path matches on)
/// and falls back to the registry number when the plug-in is busy.
fn service_location(service: &IoObject, location_key: &CFString) -> Option<u32> {
    UsbInterface::open(service)
        .and_then(|interface| interface.location_id())
        .or_else(|| {
            iokit::registry_property(service, location_key)?
                .downcast::<CFNumber>()
                .ok()?
                .as_i32()
                .map(i32::cast_unsigned)
        })
}

/// Location id + USB serial from an `IOUSBDevice` service, without opening it.
fn registry_location_and_serial(
    service: &IoObject,
    serial_key: &CFString,
    location_key: &CFString,
) -> Option<(u32, String)> {
    let location = service_location(service, location_key)?;
    let serial = iokit::registry_property(service, serial_key)?
        .downcast::<CFString>()
        .ok()?
        .to_string();
    (!serial.is_empty()).then_some((location, serial))
}

/// An opened IOKit USB device with its UVC topology resolved. The seize is
/// released when [`iokit::SeizedDevice`] drops.
struct UsbDevice {
    device: SeizedDevice,
    topology: VcTopology,
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

        let services = iokit::usb_devices().map_err(|call| ControlError::Io(call.to_string()))?;
        let mut chosen: Option<Opened> = None;
        // Count Logitech cameras reached on the location-less path. With a
        // parseable location only an exact match opens; without a hint (an
        // unparseable unique id) the first Logitech camera is a best effort
        // that is only safe when it's the *only* one — see the fail-closed
        // check after the loop.
        let mut vendor_candidates = 0usize;
        for service in services {
            let Some(found) = Self::try_open(&service, want_vid) else {
                continue;
            };
            if want_location.is_some_and(|want| found.matched_location == Some(want)) {
                chosen = Some(found);
                break;
            }
            if want_location.is_none() {
                vendor_candidates += 1;
                if chosen.is_none() {
                    chosen = Some(found);
                }
            }
        }

        // A location-less match is only unambiguous with exactly one Logitech
        // camera attached; with two (and a unique id we couldn't parse into a
        // USB location) we can't tell them apart, so refuse rather than write
        // the wrong camera's registers.
        if want_location.is_none() && vendor_candidates > 1 {
            return Err(ControlError::Ambiguous);
        }

        chosen
            .map(Opened::into_device)
            .ok_or(ControlError::NotFound)
    }

    /// Try to build an [`Opened`] from a USB service: query the device
    /// interface, match the vendor id, seize it, and resolve its UVC topology.
    fn try_open(service: &IoObject, want_vid: u16) -> Option<Opened> {
        let interface = UsbInterface::open(service)?;
        if interface.vendor_id()? != want_vid {
            return None;
        }
        let matched_location = interface.location_id();
        let device = interface.seize()?;
        let topology = video_control_topology(&device)?;
        Some(Opened {
            device: Self { device, topology },
            matched_location,
        })
    }

    /// The entity id addressed for `unit`, or `Unsupported` when the camera's
    /// descriptor lists no camera terminal, or no Extension Unit that carries
    /// the control.
    fn entity(&self, unit: Unit) -> Result<u8, ControlError> {
        match unit {
            Unit::Processing => Ok(self.topology.processing_unit),
            Unit::CameraTerminal => self
                .topology
                .camera_terminal
                .ok_or(ControlError::Unsupported),
            Unit::Extension(extension) => self
                .topology
                .extension_unit(extension)
                .ok_or(ControlError::Unsupported),
        }
    }

    /// Read a vendor control's range. The camera must report the one-byte,
    /// readable and writable control the table describes; its MIN/MAX are not
    /// asked, because the table owns the value set.
    fn extension_range(&self, extension: ExtensionControl) -> Result<ControlRange, ControlError> {
        let entity = self.entity(Unit::Extension(extension))?;
        let selector = u16::from(extension.selector);
        let mut len = [0u8; 2];
        self.transfer(RT_GET, UVC_GET_LEN, selector, entity, &mut len)?;
        let mut info = [0u8; 1];
        self.transfer(RT_GET, UVC_GET_INFO, selector, entity, &mut info)?;
        if u16::from_le_bytes(len) != 1 || info[0] & INFO_GET_SET != INFO_GET_SET {
            return Err(ControlError::Unsupported);
        }
        let mut current = [0u8; 1];
        self.transfer(RT_GET, UVC_GET_CUR, selector, entity, &mut current)?;
        extension.range(current[0])
    }

    /// Write `value` to a vendor control, once the camera has shown it carries
    /// the control the table describes.
    fn extension_set(&self, extension: ExtensionControl, value: i32) -> Result<(), ControlError> {
        let mut buf = [extension.encode(value)?];
        self.extension_range(extension)?;
        let entity = self.entity(Unit::Extension(extension))?;
        self.transfer(
            RT_SET,
            UVC_SET_CUR,
            u16::from(extension.selector),
            entity,
            &mut buf,
        )
    }

    /// Read one control's complete range. Boolean AE priority controls need
    /// synthetic bounds because UVC cameras commonly implement only GET_CUR;
    /// without GET_DEF, the live value is the only safe reset target.
    fn range(&self, control: CameraControl) -> Result<ControlRange, ControlError> {
        if let Some(extension) = control.extension() {
            return self.extension_range(extension);
        }
        if control == CameraControl::LowLightCompensation {
            let current = self.get(control, UVC_GET_CUR)?;
            return Ok(ControlRange {
                min: 0,
                max: 1,
                default: self.get(control, UVC_GET_DEF).unwrap_or(current),
                current,
                value_mask: None,
            });
        }
        let min = self.get(control, UVC_GET_MIN)?;
        let max = self.get(control, UVC_GET_MAX)?;
        let default = self.get(control, UVC_GET_DEF)?;
        let current = self.get(control, UVC_GET_CUR).unwrap_or(default);
        Ok(ControlRange {
            min,
            max,
            default,
            current,
            value_mask: None,
        })
    }

    /// Issue a UVC GET request (`req` = GET_MIN/MAX/DEF/CUR), returning the
    /// control-sized little-endian value, sign-extended per the control.
    fn get(&self, control: CameraControl, req: u8) -> Result<i32, ControlError> {
        let ControlSpec {
            unit,
            selector,
            payload,
        } = control.spec();
        let entity = self.entity(unit)?;
        let mut buf = [0u8; PAN_TILT_LEN];
        self.transfer(RT_GET, req, selector, entity, &mut buf[..payload.len()])?;
        Ok(match payload {
            Payload::U8 => i32::from(buf[0]),
            Payload::U32 => i32::try_from(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
                .unwrap_or(i32::MAX),
            Payload::PanTilt(axis) => pan_tilt_half(buf, axis),
            Payload::I16 => i32::from(i16::from_le_bytes([buf[0], buf[1]])),
            Payload::U16 => i32::from(u16::from_le_bytes([buf[0], buf[1]])),
        })
    }

    /// Issue a UVC SET_CUR request with `value` truncated to the control's size.
    fn set(&self, control: CameraControl, value: i32) -> Result<(), ControlError> {
        let ControlSpec {
            unit,
            selector,
            payload,
        } = control.spec();
        let entity = self.entity(unit)?;
        if let Unit::Extension(extension) = unit {
            return self.extension_set(extension, value);
        }
        if let Payload::PanTilt(axis) = payload {
            // The two axes share one control, so a write must carry both words:
            // read the live pair, replace this axis, send it back. A failed read
            // is surfaced rather than written over with a zeroed sibling axis,
            // which would recentre the axis the caller never asked about.
            let mut buf = [0u8; PAN_TILT_LEN];
            self.transfer(RT_GET, UVC_GET_CUR, selector, entity, &mut buf)?;
            patch_pan_tilt(&mut buf, axis, value);
            return self.transfer(RT_SET, UVC_SET_CUR, selector, entity, &mut buf);
        }
        let mut buf = value.cast_unsigned().to_le_bytes();
        self.transfer(
            RT_SET,
            UVC_SET_CUR,
            selector,
            entity,
            &mut buf[..payload.len()],
        )
    }

    /// Read an auto toggle (`req` = GET_CUR/GET_DEF) as a boolean. For the
    /// AE-mode bitmap anything but fully-manual counts as auto.
    fn get_auto(&self, toggle: AutoToggle, req: u8) -> Result<bool, ControlError> {
        let ToggleSpec { unit, selector } = toggle.spec();
        let entity = self.entity(unit)?;
        let mut buf = [0u8; 1];
        self.transfer(RT_GET, req, selector, entity, &mut buf)?;
        Ok(match toggle {
            AutoToggle::Exposure => buf[0] != AE_MANUAL,
            _ => buf[0] != 0,
        })
    }

    /// Switch an auto toggle. Enabling auto-exposure tries each AE mode the
    /// camera might support, most-automatic first.
    fn set_auto(&self, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
        let ToggleSpec { unit, selector } = toggle.spec();
        let entity = self.entity(unit)?;
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
        let mut req = IOUSBDevRequest {
            bmRequestType: request_type,
            bRequest: request,
            wValue: selector << 8,
            wIndex: (u16::from(entity) << 8) | u16::from(self.topology.vc_interface),
            #[expect(
                clippy::cast_possible_truncation,
                reason = "`data` is a UVC control payload — at most the 4 bytes a `ControlSpec` declares"
            )]
            wLength: data.len() as u16,
            pData: data.as_mut_ptr().cast::<c_void>(),
            wLenDone: 0,
        };
        if self.device.control_request(&mut req) {
            Ok(())
        } else {
            Err(ControlError::Unsupported)
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

/// Parse the device's configuration descriptors for its VideoControl topology.
fn video_control_topology(device: &SeizedDevice) -> Option<VcTopology> {
    (0..device.configuration_count()?)
        .filter_map(|index| device.configuration_descriptor(index))
        .find_map(scan_descriptors)
}

#[cfg(test)]
mod tests {
    use super::{
        Axis, CameraControl, PAN_TILT_LEN, Payload, Unit, location_hint, pan_tilt_half,
        patch_pan_tilt,
    };

    /// `dwPanAbsolute` = 1, `dwTiltAbsolute` = 2.
    const PAN_TILT_SAMPLE: [u8; PAN_TILT_LEN] = [1, 0, 0, 0, 2, 0, 0, 0];

    #[test]
    fn flicker_and_low_light_use_standard_uvc_controls() {
        let flicker = CameraControl::PowerLineFrequency.spec();
        assert_eq!(flicker.unit, Unit::Processing);
        assert_eq!(flicker.selector, 0x05);
        assert_eq!(flicker.payload, Payload::U8);

        let low_light = CameraControl::LowLightCompensation.spec();
        assert_eq!(low_light.unit, Unit::CameraTerminal);
        assert_eq!(low_light.selector, 0x03);
        assert_eq!(low_light.payload, Payload::U8);
    }

    /// Pan and tilt are two halves of one selector, not two controls.
    #[test]
    fn pan_and_tilt_share_the_pantilt_selector() {
        let pan = CameraControl::Pan.spec();
        let tilt = CameraControl::Tilt.spec();
        assert_eq!(pan.unit, Unit::CameraTerminal);
        assert_eq!(pan.selector, 0x0D);
        assert_eq!(tilt.selector, pan.selector);
        assert_eq!(pan.payload, Payload::PanTilt(Axis::Pan));
        assert_eq!(tilt.payload, Payload::PanTilt(Axis::Tilt));
        assert_eq!(pan.payload.len(), PAN_TILT_LEN);
    }

    #[test]
    fn pan_reads_the_first_word() {
        assert_eq!(pan_tilt_half(PAN_TILT_SAMPLE, Axis::Pan), 1);
    }

    #[test]
    fn tilt_reads_the_second_word() {
        assert_eq!(pan_tilt_half(PAN_TILT_SAMPLE, Axis::Tilt), 2);
    }

    /// Both axes are signed: a camera centred at 0 reports negative pan for
    /// left, so a sign-extension slip would fold the whole left half of the
    /// range onto huge positive values.
    #[test]
    fn negative_values_sign_extend() {
        let mut buf = [0u8; PAN_TILT_LEN];
        buf[..4].copy_from_slice(&(-36000i32).to_le_bytes());
        buf[4..].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(pan_tilt_half(buf, Axis::Pan), -36000);
        assert_eq!(pan_tilt_half(buf, Axis::Tilt), -1);
    }

    /// The invariant behind the read-modify-write in `Device::set`.
    #[test]
    fn patching_pan_preserves_tilt() {
        let mut buf = PAN_TILT_SAMPLE;
        patch_pan_tilt(&mut buf, Axis::Pan, -7);
        assert_eq!(pan_tilt_half(buf, Axis::Pan), -7);
        assert_eq!(
            pan_tilt_half(buf, Axis::Tilt),
            2,
            "tilt survives a pan write"
        );
    }

    #[test]
    fn patching_tilt_preserves_pan() {
        let mut buf = PAN_TILT_SAMPLE;
        patch_pan_tilt(&mut buf, Axis::Tilt, i32::MIN);
        assert_eq!(pan_tilt_half(buf, Axis::Tilt), i32::MIN);
        assert_eq!(
            pan_tilt_half(buf, Axis::Pan),
            1,
            "pan survives a tilt write"
        );
    }

    #[test]
    fn patch_then_read_round_trips_each_axis() {
        let mut buf = [0u8; PAN_TILT_LEN];
        for value in [i32::MIN, -1, 0, 1, i32::MAX] {
            patch_pan_tilt(&mut buf, Axis::Pan, value);
            patch_pan_tilt(&mut buf, Axis::Tilt, value);
            assert_eq!(pan_tilt_half(buf, Axis::Pan), value);
            assert_eq!(pan_tilt_half(buf, Axis::Tilt), value);
        }
    }

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
