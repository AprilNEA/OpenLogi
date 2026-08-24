//! HID++ `SmartShift Enhanced` (feature `0x2111`) — wheel ratchet ↔
//! free-spin control with sensitivity threshold.
//!
//! The protocol-level `0x2111` wrapper lives in `openlogi-hidpp`; this module
//! keeps OpenLogi's IPC/config-facing mode and status types.
//!
//! Mode encoding (consistent across 0x2110 / 0x2111):
//! - `wheelMode` `1` = free-spin (no ratchet, infinite scroll), `2` =
//!   ratchet (clicky).
//! - `autoDisengage` `0x01`–`0xFE` = the wheel speed (in 0.25 turn/s steps)
//!   past which a ratchet-mode wheel releases into free-spin — i.e. the
//!   "SmartShift" threshold. `0xFF` keeps the ratchet engaged permanently.

use std::{fmt, num::NonZeroU8};

use az::SaturatingAs;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use nutype::nutype;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// SmartShift mode values understood by the firmware. `Free` = free-spin,
/// `Ratchet` = clicky / smartshift-off. The discriminant is the wire byte;
/// reserved values (`0` / `3` / future) fail [`TryFrom`] and callers fall back
/// to whatever they consider sane.
///
/// Also crosses the agent↔GUI IPC — where serde encodes the variant *index*
/// (Free=0, Ratchet=1), not the `#[repr(u8)]` firmware discriminant — so
/// variant order is wire format and changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive, Serialize, Deserialize,
)]
#[repr(u8)]
pub enum SmartShiftMode {
    /// Wheel is in free-spin mode.
    Free = 1,
    /// Wheel is in ratchet mode.
    Ratchet = 2,
}

impl SmartShiftMode {
    /// The opposite mode — used when toggling SmartShift between free-spin
    /// and ratchet in the write path.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Free => Self::Ratchet,
            Self::Ratchet => Self::Free,
        }
    }
}

// The config file persists the wheel mode in its own representation
// (`crate::config::WheelMode`, kept IPC-free); these conversions are the
// single mapping between the persisted and the wire/firmware form, used by
// the GUI when committing and by the agent when re-applying after a reconnect.
impl From<crate::config::WheelMode> for SmartShiftMode {
    fn from(mode: crate::config::WheelMode) -> Self {
        match mode {
            crate::config::WheelMode::Free => Self::Free,
            crate::config::WheelMode::Ratchet => Self::Ratchet,
        }
    }
}

impl From<SmartShiftMode> for crate::config::WheelMode {
    fn from(mode: SmartShiftMode) -> Self {
        match mode {
            SmartShiftMode::Free => Self::Free,
            SmartShiftMode::Ratchet => Self::Ratchet,
        }
    }
}

/// A SmartShift auto-disengage speed threshold in firmware units of 0.25 turn/s.
///
/// Zero is HID++'s write-only "preserve" sentinel and `0xFF` means permanent
/// ratchet, so neither can inhabit this type.
#[nutype(
    const_fn,
    validate(greater_or_equal = 1, less_or_equal = 254),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct SmartShiftThreshold(u8);

impl SmartShiftThreshold {
    /// Round and clamp a floating-point control value into the firmware range.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        let value = if value.is_nan() { 1.0 } else { value };
        let raw = value.clamp(1.0, 254.0).round().saturating_as::<u8>();
        let Ok(value) = Self::try_new(raw) else {
            unreachable!("clamped SmartShift threshold is always valid");
        };
        value
    }
}

impl From<SmartShiftThreshold> for f32 {
    fn from(threshold: SmartShiftThreshold) -> Self {
        Self::from(threshold.into_inner())
    }
}

/// SmartShift's auto-disengage behavior.
///
/// Its serde representation remains the HID++ byte (`1..=254` for a threshold,
/// `255` for permanent ratchet), preserving the existing IPC and TOML shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartShiftAutoDisengage {
    /// Auto-release the ratchet when wheel speed crosses this threshold.
    Threshold(SmartShiftThreshold),
    /// Keep the ratchet engaged regardless of wheel speed.
    Permanent,
}

impl SmartShiftAutoDisengage {
    /// Whether this setting keeps the ratchet permanently engaged.
    #[must_use]
    pub const fn is_permanent(self) -> bool {
        matches!(self, Self::Permanent)
    }

    /// The speed threshold, or `None` for permanent ratchet.
    #[must_use]
    pub const fn threshold(self) -> Option<SmartShiftThreshold> {
        match self {
            Self::Threshold(threshold) => Some(threshold),
            Self::Permanent => None,
        }
    }
}

impl TryFrom<u8> for SmartShiftAutoDisengage {
    type Error = SmartShiftThresholdError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == u8::MAX {
            Ok(Self::Permanent)
        } else {
            SmartShiftThreshold::try_from(value).map(Self::Threshold)
        }
    }
}

impl From<SmartShiftAutoDisengage> for u8 {
    fn from(auto_disengage: SmartShiftAutoDisengage) -> Self {
        match auto_disengage {
            SmartShiftAutoDisengage::Threshold(threshold) => threshold.into_inner(),
            SmartShiftAutoDisengage::Permanent => Self::MAX,
        }
    }
}

impl From<NonZeroU8> for SmartShiftAutoDisengage {
    fn from(value: NonZeroU8) -> Self {
        if value == NonZeroU8::MAX {
            Self::Permanent
        } else {
            let Ok(threshold) = SmartShiftThreshold::try_new(value.get()) else {
                unreachable!("non-zero SmartShift values below 255 are thresholds");
            };
            Self::Threshold(threshold)
        }
    }
}

impl From<SmartShiftAutoDisengage> for NonZeroU8 {
    fn from(auto_disengage: SmartShiftAutoDisengage) -> Self {
        match auto_disengage {
            SmartShiftAutoDisengage::Threshold(threshold) => {
                let Some(value) = Self::new(threshold.into_inner()) else {
                    unreachable!("SmartShift thresholds are non-zero");
                };
                value
            }
            SmartShiftAutoDisengage::Permanent => Self::MAX,
        }
    }
}

impl fmt::Display for SmartShiftAutoDisengage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        u8::from(*self).fmt(formatter)
    }
}

impl Serialize for SmartShiftAutoDisengage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8((*self).into())
    }
}

impl<'de> Deserialize<'de> for SmartShiftAutoDisengage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A non-zero tunable-torque level reported by SmartShift Enhanced.
///
/// Devices without tunable-torque hardware represent that absence as zero;
/// [`SmartShiftStatus`] exposes it as `None` instead.
#[nutype(
    const_fn,
    validate(greater_or_equal = 1),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct TunableTorque(u8);

impl From<TunableTorque> for NonZeroU8 {
    fn from(torque: TunableTorque) -> Self {
        let Some(value) = Self::new(torque.into_inner()) else {
            unreachable!("tunable torque is non-zero");
        };
        value
    }
}

pub(crate) mod optional_tunable_torque {
    use super::TunableTorque;
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde field serializers must receive the field by reference"
    )]
    pub(crate) fn serialize<S>(
        torque: &Option<TunableTorque>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(torque.map_or(0, TunableTorque::into_inner))
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<TunableTorque>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        if value == 0 {
            Ok(None)
        } else {
            TunableTorque::try_from(value)
                .map(Some)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// Snapshot returned from OpenLogi's SmartShift read helpers.
///
/// Crosses the agent↔GUI IPC (`read_smartshift`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartShiftStatus {
    /// Current wheel mode.
    pub mode: SmartShiftMode,
    /// SmartShift speed threshold or permanent-ratchet behavior.
    pub auto_disengage: SmartShiftAutoDisengage,
    /// Tunable-torque level, or `None` when the device doesn't support it.
    /// Read back and re-sent unchanged so adjusting the mode or threshold
    /// doesn't disturb the wheel's resistance.
    #[serde(with = "optional_tunable_torque")]
    pub tunable_torque: Option<TunableTorque>,
}

impl From<crate::config::SmartShift> for SmartShiftStatus {
    fn from(config: crate::config::SmartShift) -> Self {
        Self {
            mode: config.mode.into(),
            auto_disengage: config.auto_disengage,
            tunable_torque: config.tunable_torque,
        }
    }
}

impl From<SmartShiftStatus> for crate::config::SmartShift {
    fn from(status: SmartShiftStatus) -> Self {
        Self {
            mode: status.mode.into(),
            auto_disengage: status.auto_disengage,
            tunable_torque: status.tunable_torque,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipped_is_an_involution() {
        assert_eq!(SmartShiftMode::Free.flipped(), SmartShiftMode::Ratchet);
        assert_eq!(SmartShiftMode::Ratchet.flipped(), SmartShiftMode::Free);
        assert_eq!(
            SmartShiftMode::Free.flipped().flipped(),
            SmartShiftMode::Free
        );
    }

    #[test]
    fn auto_disengage_reserves_zero_and_models_permanent_ratchet()
    -> Result<(), SmartShiftThresholdError> {
        let Err(_) = SmartShiftAutoDisengage::try_from(0) else {
            panic!("zero is the write-only preserve sentinel");
        };
        assert_eq!(
            SmartShiftAutoDisengage::try_from(16),
            Ok(SmartShiftAutoDisengage::Threshold(
                SmartShiftThreshold::try_new(16)?
            ))
        );
        assert_eq!(
            SmartShiftAutoDisengage::try_from(0xff),
            Ok(SmartShiftAutoDisengage::Permanent)
        );
        Ok(())
    }

    #[test]
    fn floating_thresholds_round_and_saturate_into_the_domain() {
        assert_eq!(u8::from(SmartShiftThreshold::from_rounded(15.6)), 16);
        assert_eq!(u8::from(SmartShiftThreshold::from_rounded(f32::NAN)), 1);
        assert_eq!(
            u8::from(SmartShiftThreshold::from_rounded(f32::NEG_INFINITY)),
            1
        );
        assert_eq!(
            u8::from(SmartShiftThreshold::from_rounded(f32::INFINITY)),
            254
        );
    }
}
