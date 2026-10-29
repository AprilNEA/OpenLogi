//! The CoreAudio backend.
//!
//! CoreAudio lists one audio device per engine, so a Yeti is two devices: one
//! with the capture controls (gain, mute, play-through monitoring) and one with
//! the headphone output's volume and mute. They are grouped back into one
//! microphone by their device-UID stem (see [`crate::identity`]), and each
//! control is looked up on whichever device of the group carries it.
//!
//! A control is a property on its scope's main element, or — under Apple's own
//! driver, which publishes a feature unit's per-channel volume as it is — one
//! property per channel. Reads use the first element, writes go to them all.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectID, AudioObjectIsPropertySettable, AudioObjectPropertyAddress,
    AudioObjectPropertyElement, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID, kAudioDevicePropertyModelUID,
    kAudioDevicePropertyMute, kAudioDevicePropertyPlayThru,
    kAudioDevicePropertyPlayThruVolumeDecibels, kAudioDevicePropertyPlayThruVolumeRangeDecibels,
    kAudioDevicePropertyVolumeDecibels, kAudioDevicePropertyVolumeRangeDecibels,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_audio_types::AudioValueRange;
use objc2_core_foundation::{CFRetained, CFString};
use openlogi_core::microphone::{
    GainStage, Microphone, MicrophoneChange, MicrophoneControls, MicrophoneError, MilliDecibels,
};
use openlogi_device_registry::microphone::{MicrophoneDescriptor, find_microphone};
use tracing::debug;

use crate::identity::{device_stem, usb_ids_from_model_uid};
use crate::{Control, ControlValue, control_for};

/// An `OSStatus`: CoreAudio's error code, `0` on success.
type Status = i32;

/// The most channels probed for a per-channel control. A stereo microphone
/// has two; this leaves room without probing forever.
const MAX_CHANNELS: AudioObjectPropertyElement = 8;

pub(crate) fn microphones() -> Result<Vec<Microphone>, MicrophoneError> {
    Ok(groups()
        .map_err(MicrophoneError::Rejected)?
        .iter()
        .map(Group::read)
        .collect())
}

pub(crate) fn apply(id: &str, change: MicrophoneChange) -> Result<Microphone, MicrophoneError> {
    let groups = groups().map_err(MicrophoneError::Rejected)?;
    let group = groups
        .iter()
        .find(|group| group.id == id)
        .ok_or(MicrophoneError::NotConnected)?;
    let (control, value) = control_for(change);
    let located = group
        .locate(control)
        .ok_or(MicrophoneError::NoSuchControl)?;
    located.write(value).map_err(MicrophoneError::Rejected)?;
    Ok(group.read())
}

/// The CoreAudio devices of one recognized microphone.
struct Group {
    id: String,
    descriptor: &'static MicrophoneDescriptor,
    devices: Vec<AudioObjectID>,
}

impl Group {
    fn read(&self) -> Microphone {
        Microphone {
            id: self.id.clone(),
            vendor_id: self.descriptor.vendor_id,
            product_id: self.descriptor.product_id,
            name: self.descriptor.name.to_owned(),
            registry_model_id: Some(self.descriptor.registry_model_id.to_owned()),
            controls: MicrophoneControls {
                gain: self.level(Control::Gain),
                muted: self.switch(Control::Muted),
                headphone_volume: self.level(Control::HeadphoneVolume),
                headphones_muted: self.switch(Control::HeadphonesMuted),
                monitoring: self.switch(Control::Monitoring),
                monitoring_level: self.level(Control::MonitoringLevel),
            },
        }
    }

    fn level(&self, control: Control) -> Option<GainStage> {
        self.locate(control)?.read_level()
    }

    fn switch(&self, control: Control) -> Option<bool> {
        self.locate(control)?.read_switch()
    }

    /// The first device of the group that carries `control` as a settable
    /// property, and the elements it is published on.
    fn locate(&self, control: Control) -> Option<Located> {
        let (selector, scope) = selectors(control);
        self.devices.iter().find_map(|&device| {
            let main = address(selector, scope, kAudioObjectPropertyElementMain);
            let elements: Vec<AudioObjectPropertyElement> = if has(device, main) {
                vec![kAudioObjectPropertyElementMain]
            } else {
                (1..=MAX_CHANNELS)
                    .filter(|&element| has(device, address(selector, scope, element)))
                    .collect()
            };
            let writable = !elements.is_empty()
                && elements
                    .iter()
                    .all(|&element| settable(device, address(selector, scope, element)));
            writable.then_some(Located {
                device,
                control,
                elements,
            })
        })
    }
}

/// A control found on one device of a group.
struct Located {
    device: AudioObjectID,
    control: Control,
    elements: Vec<AudioObjectPropertyElement>,
}

impl Located {
    fn address(&self, selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
        let (_, scope) = selectors(self.control);
        address(selector, scope, self.elements[0])
    }

    fn read_level(&self) -> Option<GainStage> {
        let (selector, _) = selectors(self.control);
        let range_selector = range_selector(self.control)?;
        let decibels = read(self.device, self.address(selector), 0.0_f32).ok()?;
        let empty = AudioValueRange {
            mMinimum: 0.0,
            mMaximum: 0.0,
        };
        let range = read(self.device, self.address(range_selector), empty).ok()?;
        Some(GainStage {
            level: MilliDecibels::from_decibels(f64::from(decibels))?,
            min: MilliDecibels::from_decibels(range.mMinimum)?,
            max: MilliDecibels::from_decibels(range.mMaximum)?,
        })
    }

    fn read_switch(&self) -> Option<bool> {
        let (selector, _) = selectors(self.control);
        let value = read(self.device, self.address(selector), 0_u32).ok()?;
        Some(value != 0)
    }

    fn write(&self, value: ControlValue) -> Result<(), Status> {
        let (selector, scope) = selectors(self.control);
        match value {
            ControlValue::Level(level) => {
                let level = self.read_level().map_or(level, |stage| stage.clamp(level));
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a gain in dB fits f32, CoreAudio's own type for it"
                )]
                let decibels = level.decibels() as f32;
                for &element in &self.elements {
                    write(self.device, address(selector, scope, element), decibels)?;
                }
            }
            ControlValue::Switch(on) => {
                for &element in &self.elements {
                    write(
                        self.device,
                        address(selector, scope, element),
                        u32::from(on),
                    )?;
                }
            }
        }
        debug!(device = self.device, control = ?self.control, ?value, "microphone control written");
        Ok(())
    }
}

/// The value property of `control` and the scope it lives in.
fn selectors(control: Control) -> (AudioObjectPropertySelector, AudioObjectPropertyScope) {
    match control {
        Control::Gain => (
            kAudioDevicePropertyVolumeDecibels,
            kAudioObjectPropertyScopeInput,
        ),
        Control::Muted => (kAudioDevicePropertyMute, kAudioObjectPropertyScopeInput),
        Control::HeadphoneVolume => (
            kAudioDevicePropertyVolumeDecibels,
            kAudioObjectPropertyScopeOutput,
        ),
        Control::HeadphonesMuted => (kAudioDevicePropertyMute, kAudioObjectPropertyScopeOutput),
        Control::Monitoring => (kAudioDevicePropertyPlayThru, kAudioObjectPropertyScopeInput),
        Control::MonitoringLevel => (
            kAudioDevicePropertyPlayThruVolumeDecibels,
            kAudioObjectPropertyScopeInput,
        ),
    }
}

/// The dB range property of a level control.
fn range_selector(control: Control) -> Option<AudioObjectPropertySelector> {
    match control {
        Control::Gain | Control::HeadphoneVolume => Some(kAudioDevicePropertyVolumeRangeDecibels),
        Control::MonitoringLevel => Some(kAudioDevicePropertyPlayThruVolumeRangeDecibels),
        Control::Muted | Control::HeadphonesMuted | Control::Monitoring => None,
    }
}

/// The recognized microphones among CoreAudio's devices, in a stable order.
fn groups() -> Result<Vec<Group>, Status> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for device in devices()? {
        let Some(model) = read_string(device, kAudioDevicePropertyModelUID) else {
            continue;
        };
        let Some(descriptor) = usb_ids_from_model_uid(&model)
            .and_then(|(vendor, product)| find_microphone(vendor, product))
        else {
            continue;
        };
        let Some(uid) = read_string(device, kAudioDevicePropertyDeviceUID) else {
            continue;
        };
        let id = device_stem(&uid).to_owned();
        groups
            .entry(id.clone())
            .or_insert_with(|| Group {
                id,
                descriptor,
                devices: Vec::new(),
            })
            .devices
            .push(device);
    }
    Ok(groups.into_values().collect())
}

/// Every audio device CoreAudio knows.
fn devices() -> Result<Vec<AudioObjectID>, Status> {
    let system = kAudioObjectSystemObject.cast_unsigned();
    let address = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut size = 0_u32;
    // SAFETY: `address` and `size` are valid for the call; no qualifier is
    // passed.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Err(status);
    }
    let capacity = usize::try_from(size).unwrap_or(0) / size_of::<AudioObjectID>();
    let mut devices: Vec<AudioObjectID> = vec![0; capacity];
    if devices.is_empty() {
        return Ok(devices);
    }
    // SAFETY: `devices` holds `size` bytes, the size CoreAudio just reported;
    // CoreAudio writes at most that many and updates `size` to what it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(devices.as_mut_ptr()).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    devices.truncate(usize::try_from(size).unwrap_or(0) / size_of::<AudioObjectID>());
    Ok(devices)
}

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

fn has(device: AudioObjectID, address: AudioObjectPropertyAddress) -> bool {
    // SAFETY: `address` is a valid property address for the call.
    unsafe { AudioObjectHasProperty(device, NonNull::from(&address)) }
}

fn settable(device: AudioObjectID, address: AudioObjectPropertyAddress) -> bool {
    let mut settable = 0_u8;
    // SAFETY: `address` and the one-byte `Boolean` out-parameter are valid.
    let status = unsafe {
        AudioObjectIsPropertySettable(
            device,
            NonNull::from(&address),
            NonNull::from(&mut settable),
        )
    };
    status == 0 && settable != 0
}

/// Read a fixed-size property value — `f32` dB, `u32` switch, or a range —
/// into `value`.
fn read<T: Copy>(
    device: AudioObjectID,
    address: AudioObjectPropertyAddress,
    value: T,
) -> Result<T, Status> {
    let mut value = value;
    let expected = u32::try_from(size_of::<T>()).unwrap_or(u32::MAX);
    let mut size = expected;
    // SAFETY: `value` is a live `T` of `size` bytes; CoreAudio writes at most
    // `size` bytes and reports how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    if size != expected {
        return Err(-1);
    }
    Ok(value)
}

fn write<T: Copy>(
    device: AudioObjectID,
    address: AudioObjectPropertyAddress,
    value: T,
) -> Result<(), Status> {
    let mut value = value;
    let size = u32::try_from(size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: `value` is a live `T` of `size` bytes, which CoreAudio only
    // reads for the duration of the call.
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            size,
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Read a global `CFString` property, which CoreAudio returns retained.
fn read_string(device: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<String> {
    let address = address(
        selector,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut string: *const CFString = ptr::null();
    let mut size = u32::try_from(size_of::<*const CFString>()).unwrap_or(u32::MAX);
    // SAFETY: `string` is a pointer-sized out-parameter for the `CFStringRef`
    // this property holds.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut string).cast::<c_void>(),
        )
    };
    if status != 0 {
        return None;
    }
    let string = NonNull::new(string.cast_mut())?;
    // SAFETY: CoreAudio hands string properties to the caller under the Create
    // rule, so this takes over its one retain.
    let string = unsafe { CFRetained::from_raw(string) };
    Some(string.to_string())
}
