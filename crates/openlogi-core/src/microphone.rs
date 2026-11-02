//! USB microphones whose settings the agent drives through the host's audio
//! stack: the USB Audio Class feature-unit controls behind mic gain, mute,
//! headphone volume and mic monitoring.
//!
//! These types cross the agent↔GUI IPC inside the agent snapshot, so field and
//! variant order is wire format (see `crates/openlogi-ipc/AGENTS.md`).

use serde::{Deserialize, Serialize};

/// A level in thousandths of a decibel. Whole numbers keep the state
/// comparable, so an unchanged reading publishes nothing; the finest step a
/// UAC 1.0 device reports is 1/256 dB, which this resolves to within 4 mdB.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct MilliDecibels(i32);

impl MilliDecibels {
    /// The level `millidecibels` thousandths of a decibel.
    #[must_use]
    pub const fn new(millidecibels: i32) -> Self {
        Self(millidecibels)
    }

    /// The level in thousandths of a decibel.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    /// The level nearest `decibels`, or `None` for a value that is not finite
    /// or lies beyond what a device could report (±2,000,000 dB).
    #[must_use]
    pub fn from_decibels(decibels: f64) -> Option<Self> {
        let millidecibels = (decibels * 1000.0).round();
        if !millidecibels.is_finite() || millidecibels.abs() > f64::from(i32::MAX) {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "rounded and range-checked against i32 above"
        )]
        Some(Self(millidecibels as i32))
    }

    /// The level in decibels.
    #[must_use]
    pub fn decibels(self) -> f64 {
        f64::from(self.0) / 1000.0
    }
}

/// One adjustable gain stage of a microphone, as the device reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GainStage {
    /// The current level.
    pub level: MilliDecibels,
    /// The lowest level the device accepts.
    pub min: MilliDecibels,
    /// The highest level the device accepts.
    pub max: MilliDecibels,
}

impl GainStage {
    /// `level` clamped to the range the device accepts.
    #[must_use]
    pub fn clamp(&self, level: MilliDecibels) -> MilliDecibels {
        level.clamp(self.min, self.max.max(self.min))
    }
}

/// The controls a microphone exposes and what each reads now. A control the
/// device does not have is `None`, and the GUI shows no widget for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicrophoneControls {
    /// Microphone gain: the capture feature unit's volume.
    pub gain: Option<GainStage>,
    /// Whether the microphone is muted: the capture feature unit's mute,
    /// which the mute button on the device toggles too.
    pub muted: Option<bool>,
    /// Headphone volume: the playback feature unit's volume.
    pub headphone_volume: Option<GainStage>,
    /// Whether the headphone output is muted.
    pub headphones_muted: Option<bool>,
    /// Whether the microphone is mixed into the headphone output (mic
    /// monitoring, called sidetone in G HUB).
    pub monitoring: Option<bool>,
    /// How loud the microphone is in the headphone output.
    pub monitoring_level: Option<GainStage>,
}

/// One connected USB microphone and the state of its controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Microphone {
    /// The host audio stack's identity for the device. It names the same
    /// physical microphone across reconnects where the host derives it from
    /// the USB serial, and it is what [`MicrophoneChange`]s are addressed to.
    pub id: String,
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Marketed product name from the device registry.
    pub name: String,
    /// Model ID in the OpenLogi asset registry, for the device image.
    pub registry_model_id: Option<String>,
    /// The controls and what they read now.
    pub controls: MicrophoneControls,
}

/// One change to a microphone control.
///
/// Variants are append-only: this enum crosses the IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicrophoneChange {
    /// Set the microphone gain.
    Gain(MilliDecibels),
    /// Mute or unmute the microphone.
    Muted(bool),
    /// Set the headphone volume.
    HeadphoneVolume(MilliDecibels),
    /// Mute or unmute the headphone output.
    HeadphonesMuted(bool),
    /// Turn mic monitoring on or off.
    Monitoring(bool),
    /// Set the mic monitoring level.
    MonitoringLevel(MilliDecibels),
}

/// Why a [`MicrophoneChange`] was not applied.
///
/// Variants are append-only: this enum crosses the IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum MicrophoneError {
    /// No connected microphone has this identity.
    #[error("the microphone is not connected")]
    NotConnected,
    /// The microphone has no such control.
    #[error("the microphone has no such control")]
    NoSuchControl,
    /// OpenLogi cannot drive microphone controls on this platform yet.
    #[error("microphone controls are not supported on this platform yet")]
    PlatformUnsupported,
    /// The host audio stack refused the change, with its status code.
    #[error("the audio system refused the change (status {0})")]
    Rejected(i32),
}

#[cfg(test)]
mod tests {
    use super::{GainStage, MilliDecibels};

    #[test]
    fn decibels_round_to_the_nearest_thousandth() {
        assert_eq!(
            MilliDecibels::from_decibels(-17.625),
            Some(MilliDecibels::new(-17_625))
        );
        assert_eq!(
            MilliDecibels::from_decibels(-0.473_684_3),
            Some(MilliDecibels::new(-474))
        );
        assert!((MilliDecibels::new(21_000).decibels() - 21.0).abs() < f64::EPSILON);
    }

    #[test]
    fn non_finite_or_absurd_decibels_are_rejected() {
        assert_eq!(MilliDecibels::from_decibels(f64::NAN), None);
        assert_eq!(MilliDecibels::from_decibels(f64::INFINITY), None);
        assert_eq!(MilliDecibels::from_decibels(-3.0e6), None);
    }

    #[test]
    fn a_level_clamps_to_the_device_range() {
        let stage = GainStage {
            level: MilliDecibels::new(0),
            min: MilliDecibels::new(-12_000),
            max: MilliDecibels::new(21_000),
        };

        assert_eq!(
            stage.clamp(MilliDecibels::new(30_000)),
            MilliDecibels::new(21_000)
        );
        assert_eq!(
            stage.clamp(MilliDecibels::new(-40_000)),
            MilliDecibels::new(-12_000)
        );
        assert_eq!(
            stage.clamp(MilliDecibels::new(500)),
            MilliDecibels::new(500)
        );
    }

    #[test]
    fn an_inverted_range_clamps_to_its_minimum() {
        let stage = GainStage {
            level: MilliDecibels::new(0),
            min: MilliDecibels::new(5_000),
            max: MilliDecibels::new(-5_000),
        };

        assert_eq!(
            stage.clamp(MilliDecibels::new(0)),
            MilliDecibels::new(5_000)
        );
    }
}
