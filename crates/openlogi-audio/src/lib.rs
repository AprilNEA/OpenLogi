//! Logitech USB microphones, driven through the host's audio stack.
//!
//! A Blue Yeti is a USB Audio Class device. Its settings — mic gain, mute,
//! headphone volume, mic monitoring — are UAC feature-unit controls, and the
//! host's class driver already owns them: CoreAudio on macOS publishes each
//! one as a device property. Going through that stack instead of sending
//! control requests to the device keeps the system volume UI, other apps, and
//! the OS's own per-device level memory in step with what OpenLogi sets.
//!
//! [`microphones`] finds the recognized microphones and reads every control;
//! [`apply`] changes one control and reads the microphone back. Both block on
//! the audio server, so async callers run them on a blocking thread.
//!
//! macOS is the only backend so far. Elsewhere [`microphones`] finds nothing
//! and [`apply`] answers [`MicrophoneError::PlatformUnsupported`].

#[cfg(any(target_os = "macos", test))]
mod identity;

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreAudio's property API is C: raw property buffers and a CFString out-pointer"
)]
#[path = "macos.rs"]
mod platform;
#[cfg(not(target_os = "macos"))]
#[path = "unsupported.rs"]
mod platform;

/// Whether this platform has a microphone backend. Without one,
/// [`microphones`] never finds anything, so a caller can skip polling.
pub const BACKEND_AVAILABLE: bool = cfg!(target_os = "macos");

pub use openlogi_core::microphone::{
    GainStage, Microphone, MicrophoneChange, MicrophoneControls, MicrophoneError, MilliDecibels,
};

/// Every recognized microphone connected now, with its controls read.
///
/// # Errors
///
/// [`MicrophoneError::Rejected`] when the audio server could not list its
/// devices at all. A microphone whose controls cannot be read is still listed,
/// with those controls absent.
pub fn microphones() -> Result<Vec<Microphone>, MicrophoneError> {
    platform::microphones()
}

/// Apply `change` to the microphone `id` names, then read it back.
///
/// # Errors
///
/// [`MicrophoneError::NotConnected`] when no connected microphone has that
/// identity, [`MicrophoneError::NoSuchControl`] when it lacks the control,
/// [`MicrophoneError::Rejected`] when the audio server refuses the write, and
/// [`MicrophoneError::PlatformUnsupported`] off macOS.
pub fn apply(id: &str, change: MicrophoneChange) -> Result<Microphone, MicrophoneError> {
    platform::apply(id, change)
}

/// The feature-unit controls OpenLogi drives, one per [`MicrophoneControls`]
/// field.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Gain,
    Muted,
    HeadphoneVolume,
    HeadphonesMuted,
    Monitoring,
    MonitoringLevel,
}

/// What a [`MicrophoneChange`] writes to its control.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlValue {
    Level(MilliDecibels),
    Switch(bool),
}

#[cfg(any(target_os = "macos", test))]
fn control_for(change: MicrophoneChange) -> (Control, ControlValue) {
    match change {
        MicrophoneChange::Gain(level) => (Control::Gain, ControlValue::Level(level)),
        MicrophoneChange::Muted(on) => (Control::Muted, ControlValue::Switch(on)),
        MicrophoneChange::HeadphoneVolume(level) => {
            (Control::HeadphoneVolume, ControlValue::Level(level))
        }
        MicrophoneChange::HeadphonesMuted(on) => {
            (Control::HeadphonesMuted, ControlValue::Switch(on))
        }
        MicrophoneChange::Monitoring(on) => (Control::Monitoring, ControlValue::Switch(on)),
        MicrophoneChange::MonitoringLevel(level) => {
            (Control::MonitoringLevel, ControlValue::Level(level))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Control, ControlValue, MicrophoneChange, MilliDecibels, control_for};

    #[test]
    fn every_change_targets_its_own_control() {
        let level = MilliDecibels::new(-6_000);
        let cases = [
            (
                MicrophoneChange::Gain(level),
                Control::Gain,
                ControlValue::Level(level),
            ),
            (
                MicrophoneChange::Muted(true),
                Control::Muted,
                ControlValue::Switch(true),
            ),
            (
                MicrophoneChange::HeadphoneVolume(level),
                Control::HeadphoneVolume,
                ControlValue::Level(level),
            ),
            (
                MicrophoneChange::HeadphonesMuted(false),
                Control::HeadphonesMuted,
                ControlValue::Switch(false),
            ),
            (
                MicrophoneChange::Monitoring(true),
                Control::Monitoring,
                ControlValue::Switch(true),
            ),
            (
                MicrophoneChange::MonitoringLevel(level),
                Control::MonitoringLevel,
                ControlValue::Level(level),
            ),
        ];
        for (change, control, value) in cases {
            assert_eq!(control_for(change), (control, value), "{change:?}");
        }
    }
}
