//! The default output device's volume, read through CoreAudio.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyElement, AudioObjectPropertyScope, AudioObjectPropertySelector,
    kAudioDevicePropertyVolumeScalar, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};

/// Read one fixed-size property of `object`. `T` must be the property's own
/// data type: `AudioObjectID` for a device, `f32` for a volume scalar.
fn property<T: Copy + Default>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> Option<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    };
    let mut value = T::default();
    let expected = u32::try_from(size_of::<T>()).ok()?;
    let mut size = expected;
    // SAFETY: `address`, `size`, and `value` live on this frame for the whole
    // call, `size` holds the byte size of `value`, and the HAL writes at most
    // that many bytes into it. No qualifier is passed, so its pointer is null
    // with a size of 0.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    (status == 0 && size == expected).then_some(value)
}

/// The default output device's volume, from 0.0 to 1.0, or `None` when it
/// has no volume control the HAL reports.
///
/// The main element answers on most devices; one with only per-channel
/// controls answers on its first channel.
pub(crate) fn output_volume() -> Option<f32> {
    let system = AudioObjectID::try_from(kAudioObjectSystemObject).ok()?;
    let device: AudioObjectID = property(
        system,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    )?;
    [kAudioObjectPropertyElementMain, 1]
        .into_iter()
        .find_map(|element| {
            property::<f32>(
                device,
                kAudioDevicePropertyVolumeScalar,
                kAudioObjectPropertyScopeOutput,
                element,
            )
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_reported_volume_is_a_fraction() {
        // A host without an output device (a CI runner) reports none.
        if let Some(volume) = super::output_volume() {
            assert!((0.0..=1.0).contains(&volume), "volume {volume}");
        }
    }
}
