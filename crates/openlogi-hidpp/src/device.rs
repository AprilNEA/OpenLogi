//! Implements peripheral devices connected to HID++ channels.

use std::{any::TypeId, collections::HashMap, sync::Arc};

use thiserror::Error;
use tracing::trace;

use crate::{
    channel::{ChannelError, HidppChannel},
    feature::{
        self, CreatableFeature, Feature,
        feature_set::{FeatureInformation, FeatureSetFeature},
        root::RootFeature,
    },
    protocol::{self, ProtocolVersion, v20::Hidpp20Error},
};

/// Represents a single HID++ device connected to a [`HidppChannel`].
///
/// This is used only for peripheral devices and not receivers.
#[derive(Clone)]
pub struct Device {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The initialized implementation of features the device supports.
    features: HashMap<TypeId, Arc<dyn Feature>>,

    /// The index of the device on the HID++ channel.
    pub device_index: u8,

    /// The supported protocol version reported by the device.
    pub protocol_version: ProtocolVersion,
}

impl Device {
    /// Tries to initialize a device on a HID++ channel.
    ///
    /// This will automatically ping the device to determine the protocol
    /// version it supports via [`protocol::determine_version`].
    ///
    /// Returns [`DeviceError::DeviceNotFound`] if there is no device with the
    /// specified index connected to the channel.
    ///
    /// Returns [`DeviceError::UnsupportedProtocolVersion`] if the device only
    /// supports [`ProtocolVersion::V10`].
    pub async fn new(chan: Arc<HidppChannel>, device_index: u8) -> Result<Self, DeviceError> {
        let protocol_version = protocol::determine_version(&chan, device_index).await?;

        if protocol_version.is_none() {
            return Err(DeviceError::DeviceNotFound);
        }
        let version = protocol_version.unwrap();

        if version == ProtocolVersion::V10 {
            return Err(DeviceError::UnsupportedProtocolVersion);
        }

        let mut device = Self {
            chan,
            features: HashMap::new(),
            device_index,
            protocol_version: version,
        };

        // Every HID++2.0 device supports the root feature.
        // We implicitly verified that using [`protocol::determine_version`].
        device.add_feature::<RootFeature>(0);

        Ok(device)
    }

    /// A convenience wrapper around [`Self::get_feature`] to obtain the root
    /// feature.
    pub fn root(&self) -> Arc<RootFeature> {
        self.get_feature::<RootFeature>().unwrap()
    }

    /// Adds a new feature implementation to the list of available features.
    /// This will override an existing implementation of the same type.
    /// The caller is responsible for making sure the device actually supports
    /// the feature.
    pub fn add_feature_instance<F: Feature>(&mut self, feature: F) -> Arc<F> {
        let feat_rc: Arc<dyn Feature> = Arc::new(feature);

        self.features
            .insert(TypeId::of::<F>(), Arc::clone(&feat_rc));

        Arc::downcast::<F>(feat_rc).unwrap()
    }

    /// Adds a new feature implementation to the list of available features.
    /// This will override an existing implementation of the same type.
    /// The caller is responsible for making sure the device actually supports
    /// the feature.
    ///
    /// This method uses [`CreatableFeature`] to automatically create an
    /// instance of the feature implementation and adds it using
    /// [`Self::add_feature_instance`].
    pub fn add_feature<F: CreatableFeature>(&mut self, feature_index: u8) -> Arc<F> {
        self.add_feature_instance(F::new(
            Arc::clone(&self.chan),
            self.device_index,
            feature_index,
        ))
    }

    /// Checks whether a specific feature implementation is provided by the
    /// device.
    pub fn provides_feature<F: Feature>(&self) -> bool {
        self.features.contains_key(&TypeId::of::<F>())
    }

    /// Tries to retrieve a feature implementation from the device.
    ///
    /// Returns [`None`] if the requested feature implementation is not
    /// provided.
    pub fn get_feature<F: Feature>(&self) -> Option<Arc<F>> {
        self.features
            .get(&TypeId::of::<F>())
            .cloned()
            .and_then(|feat| Arc::downcast::<F>(feat).ok())
    }

    /// Tries to detect all features supported by the device and add
    /// implementations for them using [`feature::registry::lookup_version`].
    ///
    /// Returns a vector containing all feature IDs supported by the device.
    ///
    /// Returns `Ok(None)` if the [`FeatureSetFeature`] feature, which is
    /// required for feature enumeration, is not supported by the device.
    pub async fn enumerate_features(
        &mut self,
    ) -> Result<Option<Vec<FeatureInformation>>, Hidpp20Error> {
        let Some(feature_set_info) = self.root().get_feature(FeatureSetFeature::ID).await? else {
            return Ok(None);
        };

        let feature_set_feature = self.add_feature::<FeatureSetFeature>(feature_set_info.index);

        let count = feature_set_feature.count().await?;
        trace!(
            index = self.device_index,
            count, "enumerating feature table"
        );
        let mut features = Vec::with_capacity(count as usize);
        for i in 1..=count {
            let info = feature_set_feature.get_feature(i).await?;
            trace!(
                index = self.device_index,
                slot = i,
                id = format_args!("{:#06x}", info.id),
                version = info.version,
                "feature",
            );
            features.push(info);

            if i == feature_set_info.index {
                continue;
            }

            let Some(impls) = feature::registry::lookup_version(info.id, info.version) else {
                continue;
            };

            for feat_impl in impls {
                let (type_id, instance) =
                    (feat_impl.producer)(Arc::clone(&self.chan), self.device_index, i);

                self.features.insert(type_id, instance);
            }
        }

        Ok(Some(features))
    }
}

/// Represents a device-specific error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeviceError {
    /// Indicates that the underlying [`HidppChannel`] returned an error.
    #[error("the HID++ channel returned an error")]
    Channel(#[from] ChannelError),

    /// Indicates that the specified device index points to no device.
    #[error("there is no device with the specified device index")]
    DeviceNotFound,

    /// Indicates that the addressed device does only support HID++1.0.
    #[error("the device does not support HID++2.0 or newer")]
    UnsupportedProtocolVersion,
}
