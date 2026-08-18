//! Device-level UVC controls over DirectShow (Windows).
//!
//! Windows exposes the same UVC controls the macOS backend reaches over raw
//! IOKit USB, but pre-mapped by the OS: `IAMVideoProcAmp` carries the image
//! controls (brightness/contrast/…) and `IAMCameraControl` the lens controls
//! (zoom/focus/exposure), each with per-property auto/manual flags. Writes
//! land in the camera's own registers, so — exactly as on macOS — a change is
//! seen by every app that opens the camera.
//!
//! Enumeration also lives here: the DirectShow video-input category yields
//! each camera's friendly name and its device path, whose embedded
//! `vid_xxxx&pid_xxxx` markers give the USB identity. The device path is the
//! OS capture id ([`Camera::unique_id`]); when it embeds a real USB serial
//! (not a parent-generated instance id) that serial is preferred for the
//! port-stable [`Camera::config_key`].

#![expect(
    unsafe_code,
    reason = "DirectShow COM (device enumeration + IAMVideoProcAmp / IAMCameraControl)"
)]

use std::marker::PhantomData;

use windows::Win32::Media::DirectShow::{IAMCameraControl, IAMVideoProcAmp, IBaseFilter};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IEnumMoniker, IMoniker};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR};
use windows::core::{GUID, Interface, w};

use crate::Camera;
use crate::com_windows::ComApartment;
use crate::controls::{
    AutoState, AutoToggle, CameraControl, CameraState, ControlError, ControlRange,
};

/// CLSID_SystemDeviceEnum — the DirectShow device-category enumerator.
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
/// CLSID_VideoInputDeviceCategory — webcams and other video capture sources.
const CLSID_VIDEO_INPUT_DEVICE_CATEGORY: GUID =
    GUID::from_u128(0x860bb310_5d01_11d0_bd3b_00a0c911ce86);
// VideoProcAmp / CameraControl property ids (strmif.h). Raw values rather
// than the generated enums so the mapping reads like the UVC tables.
const VPA_BRIGHTNESS: i32 = 0;
const VPA_CONTRAST: i32 = 1;
const VPA_HUE: i32 = 2;
const VPA_SATURATION: i32 = 3;
const VPA_SHARPNESS: i32 = 4;
// 5 and 6 are Gamma and ColorEnable — no control maps to them, but the enum is
// contiguous, so WhiteBalance is 7 and not the next id after Sharpness. Counting
// on from the last mapped value is how this read ColorEnable instead.
const VPA_WHITE_BALANCE: i32 = 7;
const CC_ZOOM: i32 = 3;
const CC_EXPOSURE: i32 = 4;
const CC_FOCUS: i32 = 6;
/// `*_Flags_Auto` / `*_Flags_Manual` share values across both interfaces.
const FLAG_AUTO: i32 = 0x1;
const FLAG_MANUAL: i32 = 0x2;

/// Which DirectShow interface carries a control, plus its property id.
#[derive(Clone, Copy)]
enum Prop {
    VideoProcAmp(i32),
    CameraControl(i32),
    Unsupported,
}

impl CameraControl {
    fn prop(self) -> Prop {
        match self {
            Self::Zoom => Prop::CameraControl(CC_ZOOM),
            Self::Focus => Prop::CameraControl(CC_FOCUS),
            Self::Exposure => Prop::CameraControl(CC_EXPOSURE),
            Self::PowerLineFrequency | Self::LowLightCompensation => Prop::Unsupported,
            Self::Brightness => Prop::VideoProcAmp(VPA_BRIGHTNESS),
            Self::Contrast => Prop::VideoProcAmp(VPA_CONTRAST),
            Self::Saturation => Prop::VideoProcAmp(VPA_SATURATION),
            Self::Sharpness => Prop::VideoProcAmp(VPA_SHARPNESS),
            Self::WhiteBalance => Prop::VideoProcAmp(VPA_WHITE_BALANCE),
            Self::Tint => Prop::VideoProcAmp(VPA_HUE),
        }
    }
}

impl AutoToggle {
    /// The property whose auto/manual flag backs this toggle.
    fn prop(self) -> Prop {
        match self {
            Self::Focus => Prop::CameraControl(CC_FOCUS),
            Self::Exposure => Prop::CameraControl(CC_EXPOSURE),
            Self::WhiteBalance => Prop::VideoProcAmp(VPA_WHITE_BALANCE),
        }
    }
}

/// Enumerate every video-input device DirectShow reports, with the USB
/// vendor/product ids parsed out of the device path. Non-USB sources (virtual
/// cameras) carry no `vid_`/`pid_` markers and are dropped.
pub fn enumerate() -> Vec<Camera> {
    let com = ComApartment::enter();
    monikers(&com)
        .map(|monikers| {
            monikers
                .into_iter()
                .filter_map(|m| camera_from_moniker(&m))
                .collect()
        })
        .unwrap_or_default()
}

/// Read a control's min/max/default/current straight from the device.
///
/// # Errors
/// [`ControlError::NotFound`] when no camera matches `unique_id`,
/// [`ControlError::Unsupported`] when the camera lacks the control.
pub fn control_range(
    unique_id: &str,
    control: CameraControl,
) -> Result<ControlRange, ControlError> {
    let com = ComApartment::enter();
    let dev = Device::open(&com, unique_id)?;
    dev.range(control.prop()).map(|(range, _)| range)
}

/// Read every supported control in a single device bind.
///
/// # Errors
/// [`ControlError::NotFound`] when no camera matches `unique_id`.
pub fn control_ranges(unique_id: &str) -> Result<Vec<(CameraControl, ControlRange)>, ControlError> {
    Ok(read_camera_state(unique_id)?.controls)
}

/// Read every supported control range *and* auto-toggle state in a single
/// device bind — what the GUI controls panel builds itself from.
///
/// # Errors
/// [`ControlError::NotFound`] when no camera matches `unique_id`.
pub fn read_camera_state(unique_id: &str) -> Result<CameraState, ControlError> {
    let com = ComApartment::enter();
    let dev = Device::open(&com, unique_id)?;
    let mut state = CameraState::default();
    for control in CameraControl::ALL {
        if let Ok((range, _)) = dev.range(control.prop()) {
            state.controls.push((control, range));
        }
    }
    for toggle in AutoToggle::ALL {
        if let Ok((_, caps)) = dev.range(toggle.prop())
            && caps & FLAG_AUTO != 0
            && let Ok(current) = dev.auto_engaged(toggle.prop())
        {
            // DirectShow reports which modes exist but not a factory default;
            // auto-capable properties ship with auto engaged on every Logitech
            // camera, so that is the reset target.
            state.autos.push((
                toggle,
                AutoState {
                    current,
                    default: true,
                },
            ));
        }
    }
    Ok(state)
}

/// Write a control's current value (switching that property to manual).
///
/// # Errors
/// As [`control_range`].
pub fn set_control(
    unique_id: &str,
    control: CameraControl,
    value: i32,
) -> Result<(), ControlError> {
    let com = ComApartment::enter();
    let dev = Device::open(&com, unique_id)?;
    dev.set(control.prop(), value, FLAG_MANUAL)
}

/// Switch an auto mode (focus / exposure / white balance) on or off.
///
/// # Errors
/// As [`control_range`].
pub fn set_auto(unique_id: &str, toggle: AutoToggle, on: bool) -> Result<(), ControlError> {
    let com = ComApartment::enter();
    let dev = Device::open(&com, unique_id)?;
    dev.set_auto(toggle.prop(), on)
}

/// Apply a batch of auto toggles and control values in a single device bind.
/// Every write is attempted, but any failure surfaces so callers never persist
/// a batch the hardware didn't take.
///
/// # Errors
/// [`ControlError::NotFound`] when no camera matches `unique_id`; otherwise the
/// first per-write error after attempting the whole batch.
pub fn apply_settings(
    unique_id: &str,
    autos: &[(AutoToggle, bool)],
    values: &[(CameraControl, i32)],
) -> Result<(), ControlError> {
    let com = ComApartment::enter();
    let dev = Device::open(&com, unique_id)?;
    let mut first_err = None;
    for (toggle, on) in autos {
        if let Err(e) = dev.set_auto(toggle.prop(), *on) {
            first_err.get_or_insert(e);
        }
    }
    for (control, value) in values {
        if let Err(e) = dev.set(control.prop(), *value, FLAG_MANUAL) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// A camera's bound capture filter, with the two control interfaces it may
/// implement (a camera without lens motors typically lacks `IAMCameraControl`).
///
/// Borrowing the apartment it was bound in is what keeps the interfaces from
/// outliving it: callers enter the apartment first, so this drops first, and
/// any ordering that would release a filter after `CoUninitialize` does not
/// compile.
struct Device<'a> {
    proc_amp: Option<IAMVideoProcAmp>,
    camera_control: Option<IAMCameraControl>,
    _com: PhantomData<&'a ComApartment>,
}

impl<'a> Device<'a> {
    /// Bind the capture filter whose device path equals `unique_id`. Exact
    /// match only — guessing another camera could adjust the wrong hardware.
    fn open(com: &'a ComApartment, unique_id: &str) -> Result<Self, ControlError> {
        let monikers = monikers(com).map_err(|e| ControlError::Io(e.to_string()))?;
        for moniker in monikers {
            if read_property(&moniker, w!("DevicePath")).as_deref() != Some(unique_id) {
                continue;
            }
            // SAFETY: documented moniker → filter bind; the returned interface
            // pointers are reference-counted by the `windows` wrappers.
            let filter: IBaseFilter = unsafe { moniker.BindToObject(None, None) }
                .map_err(|e| ControlError::Io(e.to_string()))?;
            return Ok(Self {
                proc_amp: filter.cast().ok(),
                camera_control: filter.cast().ok(),
                _com: PhantomData,
            });
        }
        Err(ControlError::NotFound)
    }

    /// GetRange for `prop`: the control's bounds plus its capability flags.
    fn range(&self, prop: Prop) -> Result<(ControlRange, i32), ControlError> {
        let (mut min, mut max, mut step, mut default, mut caps) = (0, 0, 0, 0, 0);
        // SAFETY: documented COM calls writing the five out-params.
        unsafe {
            match prop {
                Prop::Unsupported => return Err(ControlError::Unsupported),
                Prop::VideoProcAmp(id) => self
                    .proc_amp
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .GetRange(
                        id,
                        &raw mut min,
                        &raw mut max,
                        &raw mut step,
                        &raw mut default,
                        &raw mut caps,
                    ),
                Prop::CameraControl(id) => self
                    .camera_control
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .GetRange(
                        id,
                        &raw mut min,
                        &raw mut max,
                        &raw mut step,
                        &raw mut default,
                        &raw mut caps,
                    ),
            }
        }
        .map_err(|_| ControlError::Unsupported)?;
        let current = self.get(prop).map_or(default, |(value, _)| value);
        Ok((
            ControlRange {
                min,
                max,
                default,
                current,
                value_mask: None,
            },
            caps,
        ))
    }

    /// Get for `prop`: the current value and its auto/manual flags.
    fn get(&self, prop: Prop) -> Result<(i32, i32), ControlError> {
        let (mut value, mut flags) = (0, 0);
        // SAFETY: documented COM calls writing the two out-params.
        unsafe {
            match prop {
                Prop::Unsupported => return Err(ControlError::Unsupported),
                Prop::VideoProcAmp(id) => self
                    .proc_amp
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .Get(id, &raw mut value, &raw mut flags),
                Prop::CameraControl(id) => self
                    .camera_control
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .Get(id, &raw mut value, &raw mut flags),
            }
        }
        .map_err(|_| ControlError::Unsupported)?;
        Ok((value, flags))
    }

    /// Whether `prop` currently runs in auto mode.
    fn auto_engaged(&self, prop: Prop) -> Result<bool, ControlError> {
        Ok(self.get(prop)?.1 & FLAG_AUTO != 0)
    }

    /// Set `prop` to `value` under `flags` (auto or manual).
    fn set(&self, prop: Prop, value: i32, flags: i32) -> Result<(), ControlError> {
        // SAFETY: documented COM calls; the device validates the value.
        unsafe {
            match prop {
                Prop::Unsupported => return Err(ControlError::Unsupported),
                Prop::VideoProcAmp(id) => self
                    .proc_amp
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .Set(id, value, flags),
                Prop::CameraControl(id) => self
                    .camera_control
                    .as_ref()
                    .ok_or(ControlError::Unsupported)?
                    .Set(id, value, flags),
            }
        }
        .map_err(|_| ControlError::Unsupported)
    }

    /// Engage or release auto mode, keeping the current value in place.
    fn set_auto(&self, prop: Prop, on: bool) -> Result<(), ControlError> {
        let value = self.get(prop).map_or(0, |(v, _)| v);
        self.set(prop, value, if on { FLAG_AUTO } else { FLAG_MANUAL })
    }
}

/// Every video-input moniker DirectShow reports (empty when the category has
/// no devices, which the enumerator signals with `S_FALSE`).
///
/// The monikers are COM objects belonging to the caller's apartment, so the
/// guard is a parameter rather than something entered here: it proves one is
/// live for the call, and leaves the caller — which drops the returned vector
/// well before its own guard — owning the ordering.
fn monikers(_com: &ComApartment) -> windows::core::Result<Vec<IMoniker>> {
    // SAFETY: documented enumerator calls, made under the caller's apartment.
    // The enumerator works under either apartment model, so it does not matter
    // whether that guard entered the MTA or joined an existing STA.
    unsafe {
        let dev_enum: windows::Win32::Media::DirectShow::ICreateDevEnum =
            CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER)?;
        let mut enum_moniker: Option<IEnumMoniker> = None;
        // S_FALSE (an empty category) is Ok with no enumerator — handled below.
        dev_enum.CreateClassEnumerator(
            &CLSID_VIDEO_INPUT_DEVICE_CATEGORY,
            &raw mut enum_moniker,
            0,
        )?;
        let Some(enum_moniker) = enum_moniker else {
            return Ok(Vec::new());
        };
        let mut all = Vec::new();
        loop {
            let mut chunk = [const { None }; 8];
            let mut fetched = 0;
            let hr = enum_moniker.Next(&mut chunk, Some(&raw mut fetched));
            all.extend(chunk.into_iter().take(fetched as usize).flatten());
            if hr.is_err() || fetched == 0 {
                break;
            }
        }
        Ok(all)
    }
}

/// Build a [`Camera`] from one moniker: friendly name + device path, with the
/// USB ids parsed from the path's `vid_xxxx&pid_xxxx` markers.
fn camera_from_moniker(moniker: &IMoniker) -> Option<Camera> {
    let unique_id = read_property(moniker, w!("DevicePath"))?;
    let (vendor_id, product_id) = parse_device_path_ids(&unique_id)?;
    let name = read_property(moniker, w!("FriendlyName")).unwrap_or_else(|| "Camera".into());
    let serial_number = usb_serial_from_device_path(&unique_id);
    Some(Camera {
        name,
        unique_id,
        serial_number,
        vendor_id,
        product_id,
        max_resolution: None,
        max_fps: None,
    })
}

/// USB `iSerialNumber` embedded in a device-interface path, when present.
///
/// Paths look like `\\?\usb#vid_046d&pid_0893&mi_00#SERIAL#{guid}\global`.
/// Windows fabricates a parent-relative instance id (always containing `&`)
/// when the device has no serial — those are port-dependent and rejected.
pub(crate) fn usb_serial_from_device_path(path: &str) -> Option<String> {
    // Split on '#': [prefix, hardware-id, instance, {class-guid}…]
    let instance = path.split('#').nth(2)?.split('\\').next()?.trim();
    if instance.is_empty() || instance.contains('&') {
        return None;
    }
    Some(instance.to_string())
}

/// Read one string property (`FriendlyName` / `DevicePath`) from a moniker's
/// property bag.
fn read_property(moniker: &IMoniker, name: windows::core::PCWSTR) -> Option<String> {
    // SAFETY: documented property-bag reads; the VARIANT is only interpreted
    // as a BSTR when the bag reports that type.
    unsafe {
        let bag: IPropertyBag = moniker.BindToStorage(None, None).ok()?;
        let mut value = VARIANT::default();
        bag.Read(name, &raw mut value, None).ok()?;
        if value.Anonymous.Anonymous.vt != VT_BSTR {
            return None;
        }
        Some(value.Anonymous.Anonymous.Anonymous.bstrVal.to_string())
    }
}

/// Pull the hex USB vendor/product ids out of a device path such as
/// `\\?\usb#vid_046d&pid_0893&mi_00#…`.
fn parse_device_path_ids(path: &str) -> Option<(u16, u16)> {
    let lower = path.to_ascii_lowercase();
    let hex_after = |marker: &str| -> Option<u16> {
        let rest = lower.split(marker).nth(1)?;
        u16::from_str_radix(rest.get(..4)?, 16).ok()
    };
    Some((hex_after("vid_")?, hex_after("pid_")?))
}

#[cfg(test)]
mod tests {
    use super::usb_serial_from_device_path;

    #[test]
    fn real_usb_serial_is_extracted() {
        let path = r"\\?\usb#vid_046d&pid_0893&mi_00#6B123456#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert_eq!(
            usb_serial_from_device_path(path).as_deref(),
            Some("6B123456")
        );
    }

    #[test]
    fn parent_generated_instance_is_rejected() {
        // StreamCam without a firmware serial: Windows fabricates `9&56d9c30&0&0000`.
        let path = r"\\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert_eq!(usb_serial_from_device_path(path), None);
    }
}
