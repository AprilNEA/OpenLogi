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
//!
//! The IOKit handles themselves live in [`iokit`], which owns every `unsafe`
//! block in this backend and hands the descriptor up as a plain `&[u8]`.

mod iokit;

use std::collections::HashMap;
use std::ffi::c_void;

use objc2_core_foundation::{CFNumber, CFString};
use objc2_io_kit::IOUSBDevRequest;

use iokit::{IoObject, SeizedDevice, UsbInterface};

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

/// The wire type of a control's value: how many bytes it occupies on the bus
/// and how a read is sign-extended. UVC controls have exactly one of these
/// per selector — `len` and `signed` are not independent, so this collapses
/// them into the one combination each control actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Payload {
    /// 1-byte unsigned (menu/enum controls).
    U8,
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
        }
    }
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
    /// §A.9.5), and wire payload type for this control.
    const fn spec(self) -> ControlSpec {
        use Payload::{I16, U8, U16, U32};
        use Unit::{CameraTerminal, Processing};
        let (unit, selector, payload) = match self {
            Self::Zoom => (CameraTerminal, 0x0B, U16), // CT_ZOOM_ABSOLUTE_CONTROL
            Self::Focus => (CameraTerminal, 0x06, U16), // CT_FOCUS_ABSOLUTE_CONTROL
            Self::Exposure => (CameraTerminal, 0x04, U32), // CT_EXPOSURE_TIME_ABSOLUTE_CONTROL
            Self::PowerLineFrequency => (Processing, 0x05, U8), // PU_POWER_LINE_FREQUENCY_CONTROL
            Self::LowLightCompensation => (CameraTerminal, 0x03, U8), // CT_AE_PRIORITY_CONTROL
            Self::Brightness => (Processing, 0x02, I16), // PU_BRIGHTNESS_CONTROL
            Self::Contrast => (Processing, 0x03, U16), // PU_CONTRAST_CONTROL
            Self::Saturation => (Processing, 0x07, U16), // PU_SATURATION_CONTROL
            Self::Sharpness => (Processing, 0x08, U16), // PU_SHARPNESS_CONTROL
            Self::WhiteBalance => (Processing, 0x0A, U16), // PU_WHITE_BALANCE_TEMPERATURE_CONTROL
            Self::Tint => (Processing, 0x06, I16),     // PU_HUE_CONTROL
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

/// Location id + USB serial from an `IOUSBDevice` service, without opening it.
fn registry_location_and_serial(
    service: &IoObject,
    serial_key: &CFString,
    location_key: &CFString,
) -> Option<(u32, String)> {
    // Prefer the USB device interface for the location (it is what the control
    // path matches on); fall back to the registry number when the plug-in is
    // busy.
    let location = UsbInterface::open(service)
        .and_then(|interface| interface.location_id())
        .or_else(|| {
            iokit::registry_property(service, location_key)?
                .downcast::<CFNumber>()
                .ok()?
                .as_i32()
                .map(i32::cast_unsigned)
        })?;
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
            device: Self {
                device,
                vc_interface: topology.vc_interface,
                unit_id: topology.processing_unit,
                terminal_id: topology.camera_terminal,
            },
            matched_location,
        })
    }

    /// The entity id addressed for `unit`, or `Unsupported` when the camera's
    /// descriptor lists no camera terminal.
    fn entity(&self, unit: Unit) -> Result<u8, ControlError> {
        match unit {
            Unit::Processing => Ok(self.unit_id),
            Unit::CameraTerminal => self.terminal_id.ok_or(ControlError::Unsupported),
        }
    }

    /// Read one control's complete range. Boolean AE priority controls need
    /// synthetic bounds because UVC cameras commonly implement only GET_CUR;
    /// without GET_DEF, the live value is the only safe reset target.
    fn range(&self, control: CameraControl) -> Result<ControlRange, ControlError> {
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
        let mut buf = [0u8; 4];
        self.transfer(RT_GET, req, selector, entity, &mut buf[..payload.len()])?;
        Ok(match payload {
            Payload::U8 => i32::from(buf[0]),
            Payload::U32 => i32::try_from(u32::from_le_bytes(buf)).unwrap_or(i32::MAX),
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
            wIndex: (u16::from(entity) << 8) | u16::from(self.vc_interface),
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

/// The VideoControl entities a control request can address, parsed from the
/// configuration descriptor.
#[derive(Debug, PartialEq, Eq)]
struct VcTopology {
    vc_interface: u8,
    processing_unit: u8,
    camera_terminal: Option<u8>,
}

/// Parse the device's configuration descriptors for the VideoControl interface
/// number, the Processing-Unit id, and the camera (input) terminal id.
fn video_control_topology(device: &SeizedDevice) -> Option<VcTopology> {
    (0..device.configuration_count()?)
        .filter_map(|index| device.configuration_descriptor(index))
        .find_map(scan_descriptors)
}

/// The VideoControl interface the descriptor walk is currently inside, and the
/// entities seen in it so far.
///
/// A class-specific descriptor belongs to the interface it follows, so this is
/// dropped on leaving the block: VideoStreaming reuses descriptor type `0x24`
/// with its own subtype numbering, in which `0x05` is a frame descriptor rather
/// than a Processing Unit and `0x02` an output header rather than an input
/// terminal. Tracking the block — instead of a bare "have we seen a
/// VideoControl interface" flag — is what keeps a frame index from being read
/// as a Processing-Unit id on a camera whose VideoControl block has none.
struct VcBlock {
    interface: u8,
    camera_terminal: Option<u8>,
}

/// Walk a configuration-descriptor blob, collecting the first VideoControl
/// interface's Processing-Unit and camera-terminal entity ids.
///
/// The walk stops at the first descriptor whose `bLength` is nonsense or would
/// run past the blob, so a malformed descriptor truncates the scan rather than
/// misreading the bytes after it.
fn scan_descriptors(blob: &[u8]) -> Option<VcTopology> {
    let mut rest = blob;
    let mut block: Option<VcBlock> = None;
    while rest.len() >= 2 {
        let len = usize::from(rest[0]);
        let dtype = rest[1];
        if len < 2 || len > rest.len() {
            break;
        }
        let (descriptor, tail) = rest.split_at(len);
        rest = tail;

        if dtype == DESC_INTERFACE {
            // bInterfaceNumber, bInterfaceClass and bInterfaceSubClass sit at
            // offsets 2, 5 and 6 of an interface descriptor.
            block = match (descriptor.get(2), descriptor.get(5), descriptor.get(6)) {
                (Some(&interface), Some(&class), Some(&subclass))
                    if class == CC_VIDEO && subclass == SC_VIDEOCONTROL =>
                {
                    Some(VcBlock {
                        interface,
                        camera_terminal: None,
                    })
                }
                _ => None,
            };
        } else if dtype == DESC_CS_INTERFACE
            && let Some(block) = block.as_mut()
            && let (Some(&subtype), Some(&entity)) = (descriptor.get(2), descriptor.get(3))
        {
            // bUnitID / bTerminalID sit at offset 3 in both descriptors; an
            // input terminal's wTerminalType (offsets 4..6) must be the camera
            // sensor — skip composite/other input terminals.
            if subtype == VC_INPUT_TERMINAL && descriptor.len() >= 8 {
                let terminal_type = u16::from(descriptor[4]) | (u16::from(descriptor[5]) << 8);
                if terminal_type == ITT_CAMERA && block.camera_terminal.is_none() {
                    block.camera_terminal = Some(entity);
                }
            } else if subtype == VC_PROCESSING_UNIT {
                return Some(VcTopology {
                    vc_interface: block.interface,
                    processing_unit: entity,
                    camera_terminal: block.camera_terminal,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        CameraControl, ITT_CAMERA, Payload, Unit, VcTopology, location_hint, scan_descriptors,
    };

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

    /// A 9-byte interface descriptor with the given number/class/subclass.
    fn interface(number: u8, class: u8, subclass: u8) -> Vec<u8> {
        vec![9, 0x04, number, 0, 0, class, subclass, 0, 0]
    }

    /// An 8-byte VC_INPUT_TERMINAL descriptor for `entity`.
    fn input_terminal(entity: u8, terminal_type: u16) -> Vec<u8> {
        let [type_lo, type_hi] = terminal_type.to_le_bytes();
        vec![8, 0x24, 0x02, entity, type_lo, type_hi, 0, 0]
    }

    /// A minimal VC_PROCESSING_UNIT descriptor for `entity`.
    fn processing_unit(entity: u8) -> Vec<u8> {
        vec![4, 0x24, 0x05, entity]
    }

    #[test]
    fn finds_the_processing_unit_behind_a_videocontrol_interface() {
        let blob: Vec<u8> = [
            vec![9, 0x02, 0, 0, 0, 0, 0, 0, 0], // configuration header
            interface(3, 0x0E, 0x01),           // VideoControl
            input_terminal(1, ITT_CAMERA),
            processing_unit(2),
        ]
        .concat();
        assert_eq!(
            scan_descriptors(&blob),
            Some(VcTopology {
                vc_interface: 3,
                processing_unit: 2,
                camera_terminal: Some(1),
            })
        );
    }

    /// Composite/other input terminals are not the camera sensor, so lens
    /// controls must report unsupported rather than address the wrong entity.
    #[test]
    fn a_non_camera_input_terminal_leaves_lens_controls_unsupported() {
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01),
            input_terminal(1, 0x0401), // ITT_MEDIA_TRANSPORT_INPUT
            processing_unit(5),
        ]
        .concat();
        assert_eq!(
            scan_descriptors(&blob),
            Some(VcTopology {
                vc_interface: 0,
                processing_unit: 5,
                camera_terminal: None,
            })
        );
    }

    /// Class-specific descriptors before any VideoControl interface belong to
    /// some other function and must not be read as UVC entities.
    #[test]
    fn class_descriptors_outside_a_videocontrol_interface_are_ignored() {
        let blob: Vec<u8> = [
            interface(0, 0x01, 0x01), // audio
            processing_unit(9),
        ]
        .concat();
        assert_eq!(scan_descriptors(&blob), None);
    }

    /// …and neither do the ones *after* it. VideoStreaming reuses descriptor
    /// type 0x24 with its own subtype numbering, where 0x05 is
    /// VS_FRAME_UNCOMPRESSED rather than VC_PROCESSING_UNIT. A camera whose
    /// VideoControl block has no Processing Unit must report none, not the
    /// first frame descriptor's bFrameIndex — which would send every image
    /// control to whatever entity happens to share that id.
    #[test]
    fn a_videostreaming_frame_descriptor_is_not_a_processing_unit() {
        // A real VS_FRAME_UNCOMPRESSED: 30 bytes, descriptor type 0x24 like a
        // VideoControl unit, subtype 0x05, and bFrameIndex sitting exactly
        // where a unit keeps its bUnitID.
        let mut vs_frame = vec![0u8; 30];
        vs_frame[0] = 30;
        vs_frame[1] = 0x24;
        vs_frame[2] = 0x05;
        vs_frame[3] = 1;
        let blob: Vec<u8> = [
            interface(0, 0x0E, 0x01), // VideoControl — no processing unit
            input_terminal(1, ITT_CAMERA),
            interface(1, 0x0E, 0x02), // VideoStreaming
            vs_frame,
        ]
        .concat();
        assert_eq!(scan_descriptors(&blob), None);
    }

    /// A descriptor whose bLength overruns the blob truncates the walk instead
    /// of reading past it — and a zero length must not loop forever.
    #[test]
    fn malformed_lengths_stop_the_walk() {
        let overrun: Vec<u8> = [interface(0, 0x0E, 0x01), vec![64, 0x24, 0x05, 7]].concat();
        assert_eq!(scan_descriptors(&overrun), None);

        let zero_length: Vec<u8> = [interface(0, 0x0E, 0x01), vec![0, 0x24]].concat();
        assert_eq!(scan_descriptors(&zero_length), None);
    }
}
