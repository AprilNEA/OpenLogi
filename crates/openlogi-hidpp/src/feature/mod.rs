//! Specific device feature implementations.

use std::{any::Any, sync::Arc};

use crate::channel::HidppChannel;

pub mod device_friendly_name;
pub mod device_information;
pub mod device_type_and_name;
pub mod feature_set;
pub mod hires_wheel;
pub mod registry;
pub mod root;
pub mod smartshift;
pub mod thumbwheel;
pub mod unified_battery;
pub mod wireless_device_status;

/// Represents a concrete implementation of a HID++2.0 device feature.
pub trait Feature: Any + Send + Sync {}

/// Represents a [`Feature`] that can be instantiated automatically.
pub trait CreatableFeature: Feature {
    /// The protocol ID of the implemented feature.
    const ID: u16;

    /// The version of the feature the implementation starts to support.
    const STARTING_VERSION: u8;

    /// Creates a new instance of the feature implementation.
    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self;
}

/// Represents a [`Feature`] that emits events of type `T`.
pub trait EmittingFeature<T>: Feature {
    /// Creates a receiver that is being notified whenever a new event of type
    /// `T` is emitted by the feature.
    fn listen(&self) -> async_channel::Receiver<T>;
}

/// A bitfield describing some properties of a feature.
///
/// Documentation is taken from <https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view>.
#[derive(Clone, Copy, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FeatureType {
    /// An obsolete feature is a feature that has been replaced by a newer one,
    /// but is advertised in order for older SWs to still be able to support the
    /// feature (in case the old SW does not know yet the newer one).
    pub obsolete: bool,

    /// A SW hidden feature is a feature that should not be known/managed/used
    /// by end user configuration SW. The host should ignore this type of
    /// features.
    pub hidden: bool,

    /// A hidden feature that has been disabled for user software. Used for
    /// internal testing and manufacturing.
    pub engineering: bool,

    /// A manufacturing feature that can be permanently deactivated. It is
    /// usually also hidden and engineering.
    ///
    /// This field was added in feature version 2 and will be `false` for all
    /// older versions.
    pub manufacturing_deactivatable: bool,

    /// A compliance feature that can be permanently deactivated. It is usually
    /// also hidden and engineering.
    ///
    /// This field was added in feature version 2 and will be `false` for all
    /// older versions.
    pub compliance_deactivatable: bool,
}

impl From<u8> for FeatureType {
    fn from(value: u8) -> Self {
        Self {
            obsolete: value & (1 << 7) != 0,
            hidden: value & (1 << 6) != 0,
            engineering: value & (1 << 5) != 0,
            manufacturing_deactivatable: value & (1 << 4) != 0,
            compliance_deactivatable: value & (1 << 3) != 0,
        }
    }
}

impl From<FeatureType> for u8 {
    fn from(value: FeatureType) -> Self {
        let mut raw = 0;

        if value.obsolete {
            raw |= 1 << 7
        }
        if value.hidden {
            raw |= 1 << 6
        }
        if value.engineering {
            raw |= 1 << 5
        }
        if value.manufacturing_deactivatable {
            raw |= 1 << 4
        }
        if value.compliance_deactivatable {
            raw |= 1 << 3
        }

        raw
    }
}
