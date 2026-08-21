//! Implements peripheral devices connected to HID++ channels.

use std::{any::TypeId, collections::HashMap, sync::Arc, time::Duration};

use futures::{FutureExt, select};
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

    /// Cached handle to the root feature. [`Self::new`] always installs one
    /// before returning, so [`Self::root`] can hand it back directly instead
    /// of going through the generic (and fallible) `features` lookup.
    root: Arc<RootFeature>,

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
        let Some(version) = protocol::determine_version(&chan, device_index).await? else {
            return Err(DeviceError::DeviceNotFound);
        };

        if version == ProtocolVersion::V10 {
            return Err(DeviceError::UnsupportedProtocolVersion);
        }

        // Every HID++2.0 device supports the root feature.
        // We implicitly verified that using [`protocol::determine_version`].
        let mut features: HashMap<TypeId, Arc<dyn Feature>> = HashMap::new();
        let root = insert_feature(
            &mut features,
            RootFeature::new(Arc::clone(&chan), device_index, 0),
        );

        Ok(Self {
            chan,
            root,
            features,
            device_index,
            protocol_version: version,
        })
    }

    /// A convenience wrapper around [`Self::get_feature`] to obtain the root
    /// feature.
    #[must_use]
    pub fn root(&self) -> Arc<RootFeature> {
        Arc::clone(&self.root)
    }

    /// Adds a new feature implementation to the list of available features.
    /// This will override an existing implementation of the same type.
    /// The caller is responsible for making sure the device actually supports
    /// the feature.
    pub fn add_feature_instance<F: Feature>(&mut self, feature: F) -> Arc<F> {
        insert_feature(&mut self.features, feature)
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
    #[must_use]
    pub fn provides_feature<F: Feature>(&self) -> bool {
        self.features.contains_key(&TypeId::of::<F>())
    }

    /// Tries to retrieve a feature implementation from the device.
    ///
    /// Returns [`None`] if the requested feature implementation is not
    /// provided.
    #[must_use]
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
            let info = read_feature_entry(&feature_set_feature, self.device_index, i).await?;
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

/// Inserts a feature implementation into a device's feature map, returning a
/// concretely-typed handle to it.
///
/// Building the `Arc<F>` once and coercing a clone to `Arc<dyn Feature>` for
/// storage avoids an erase-then-downcast round trip through the map, so the
/// returned handle can never fail to be `F`.
fn insert_feature<F: Feature>(
    features: &mut HashMap<TypeId, Arc<dyn Feature>>,
    feature: F,
) -> Arc<F> {
    let feat_rc = Arc::new(feature);
    features.insert(TypeId::of::<F>(), Arc::clone(&feat_rc) as Arc<dyn Feature>);
    feat_rc
}

/// Per-attempt deadline for one feature-table read during enumeration.
///
/// The channel's default [`crate::channel::SEND_RESPONSE_TIMEOUT`] (5s) is
/// longer than the budget most callers give the whole walk, so one dropped
/// report used to consume the caller's entire probe budget and abort
/// enumeration. A HID++ round trip that is going to answer answers in tens of
/// milliseconds; past this the report is lost, and re-asking beats waiting.
const FEATURE_READ_ATTEMPT: Duration = Duration::from_millis(700);

/// Attempts per feature-table entry before enumeration gives up on it.
///
/// Bluetooth-direct links drop individual reports while the table itself stays
/// stable, so a lost entry is worth re-asking for rather than discarding a walk
/// that may already be thirty entries deep.
const FEATURE_READ_ATTEMPTS: u8 = 4;

/// Pause between attempts, letting the link drain before re-asking.
const FEATURE_READ_BACKOFF: Duration = Duration::from_millis(120);

/// Reads one feature-table entry, re-asking under a short per-attempt deadline
/// when the link drops the report.
///
/// A feature-level refusal ([`Hidpp20Error::Feature`]) or an unsupported
/// response returns immediately: the device answered, so re-asking cannot
/// change the answer. Only transport failures are retried.
async fn read_feature_entry(
    feature_set: &FeatureSetFeature,
    device_index: u8,
    index: u8,
) -> Result<FeatureInformation, Hidpp20Error> {
    let mut last_error = None;
    for attempt in 1..=FEATURE_READ_ATTEMPTS {
        let mut read = std::pin::pin!(feature_set.get_feature(index).fuse());
        let outcome = select! {
            result = read => Some(result),
            () = futures_timer::Delay::new(FEATURE_READ_ATTEMPT).fuse() => None,
        };
        match outcome {
            Some(Ok(info)) => return Ok(info),
            Some(Err(e @ (Hidpp20Error::Feature(_) | Hidpp20Error::UnsupportedResponse))) => {
                return Err(e);
            }
            Some(Err(e)) => last_error = Some(e),
            None => trace!(
                index = device_index,
                slot = index,
                attempt,
                "feature-table read timed out — re-asking"
            ),
        }
        if attempt < FEATURE_READ_ATTEMPTS {
            futures_timer::Delay::new(FEATURE_READ_BACKOFF).await;
        }
    }
    Err(last_error.unwrap_or(Hidpp20Error::Channel(ChannelError::Timeout)))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        channel::tests::{MockRawHidChannel, channel_with_reader},
        feature::{CreatableFeature as _, feature_set::FeatureSetFeature},
        protocol::v20::Hidpp20Error,
    };

    use super::{FEATURE_READ_ATTEMPTS, read_feature_entry};

    /// An entry whose report is lost is re-asked rather than abandoned. Aborting
    /// on the first lost report is what made Bluetooth-direct enumeration give
    /// up mid-table, which callers then misread as "not a peripheral".
    #[test]
    fn lost_feature_entry_is_retried_before_giving_up() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            // The mock answers nothing, so every attempt runs to its deadline.
            let feature_set = FeatureSetFeature::new(Arc::clone(&channel), 0xff, 0x01);

            let err = read_feature_entry(&feature_set, 0xff, 1).await.unwrap_err();

            assert!(
                matches!(err, Hidpp20Error::Channel(_)),
                "an unanswered entry surfaces as a transport failure, got {err:?}"
            );
            assert_eq!(
                handle.written_reports().len(),
                usize::from(FEATURE_READ_ATTEMPTS),
                "every attempt should reach the wire"
            );
        });
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
