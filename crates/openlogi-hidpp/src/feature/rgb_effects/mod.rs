//! Implements the `RgbEffects` feature (ID `0x8071`, version 4), the modern
//! per-cluster RGB effect engine (successor to
//! [`ColorLedEffects`](super::color_led_effects), `0x8070`).
//!
//! A device groups its LEDs into *clusters*, each supporting a set of *effects*.
//! [`get_device_info`](RgbEffectsFeature::get_device_info),
//! [`get_cluster_info`](RgbEffectsFeature::get_cluster_info) and
//! [`get_effect_info`](RgbEffectsFeature::get_effect_info) decode the three
//! general-info modes of the polymorphic `getInfo` function; effects are applied
//! with [`set_rgb_cluster_effect`](RgbEffectsFeature::set_rgb_cluster_effect).
//!
//! Software must first take control with
//! [`set_sw_control`](RgbEffectsFeature::set_sw_control) before applying effects
//! or power modes, or those calls return a "not allowed" error.
//!
//! All multi-byte fields in this feature are big-endian.

pub mod event;
pub mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use event::RgbEffectsEvent;
pub use types::{
    ActivityEventType, CLUSTER_EFFECT_PARAM_COUNT, DisplayPersistencyCapabilities,
    EventsNotificationFlags, LED_BIN_PARAM_COUNT, LedBinIndex, ONBOARD_INFO_PARAM_COUNT,
    PowerModeTarget, RgbClusterInfo, RgbDeviceInfo, RgbEffectInfo, RgbExtCapabilities,
    RgbNvCapabilities, RgbNvConfig, RgbPersistence, RgbPowerMode, RgbPowerModeConfig, RgbSwControl,
    SlotInfoType, SwControlFlags,
};

use self::types::{ALL_CLUSTERS, ALL_EFFECTS, GetOrSet, be16};
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

/// `typeOfInfo` value selecting general info in `getInfo`.
const TYPE_GENERAL_INFO: u8 = 0x00;
/// `typeOfInfo` value selecting onboard-stored effect info in `getInfo`.
const TYPE_ONBOARD_EFFECT: u8 = 0x01;
/// `getOrSet` value requesting a backup read in `manageRgbLedBinInfo`.
const GET_BACKUP: u8 = 0x02;
/// Bit offset of the power-mode target in the `setRgbClusterEffect` flags byte.
const POWER_TARGET_SHIFT: u8 = 2;

/// Implements the `RgbEffects` / `0x8071` feature.
pub struct RgbEffectsFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<RgbEffectsEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for RgbEffectsFeature {
    const ID: u16 = 0x8071;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                if let Some(event) = event::decode_event(func.to_lo(), &payload) {
                    emitter.emit(event);
                }
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for RgbEffectsFeature {}

impl EmittingFeature<RgbEffectsEvent> for RgbEffectsFeature {
    fn listen(&self) -> async_channel::Receiver<RgbEffectsEvent> {
        self.emitter.create_receiver()
    }
}

impl RgbEffectsFeature {
    /// Retrieves device-level RGB information (`getInfo` device mode).
    pub async fn get_device_info(&self) -> Result<RgbDeviceInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [ALL_CLUSTERS, ALL_EFFECTS, TYPE_GENERAL_INFO])
            .await?
            .extend_payload();
        Ok(RgbDeviceInfo::from_payload(&payload))
    }

    /// Retrieves cluster-level information for `cluster_index` (`getInfo` cluster
    /// mode).
    pub async fn get_cluster_info(
        &self,
        cluster_index: u8,
    ) -> Result<RgbClusterInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [cluster_index, ALL_EFFECTS, TYPE_GENERAL_INFO])
            .await?
            .extend_payload();
        Ok(RgbClusterInfo::from_payload(&payload))
    }

    /// Retrieves effect-level information for an effect of a cluster (`getInfo`
    /// effect mode).
    pub async fn get_effect_info(
        &self,
        cluster_index: u8,
        cluster_effect_index: u8,
    ) -> Result<RgbEffectInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [cluster_index, cluster_effect_index, TYPE_GENERAL_INFO])
            .await?
            .extend_payload();
        Ok(RgbEffectInfo::from_payload(&payload))
    }

    /// Retrieves raw information about an onboard-stored effect slot.
    ///
    /// The returned parameters' meaning depends on `slot_info_type` (see the
    /// feature spec): e.g. slot state, defaults, UUID bytes, or effect-name
    /// characters.
    pub async fn get_onboard_effect_info(
        &self,
        cluster_index: u8,
        cluster_effect_index: u8,
        slot: u8,
        slot_info_type: SlotInfoType,
    ) -> Result<[u8; ONBOARD_INFO_PARAM_COUNT], Hidpp20Error> {
        let mut args = [0; 16];
        args[..5].copy_from_slice(&[
            cluster_index,
            cluster_effect_index,
            TYPE_ONBOARD_EFFECT,
            slot,
            slot_info_type.into(),
        ]);
        let payload = self.endpoint.call_long(0, args).await?.extend_payload();
        let mut params = [0; ONBOARD_INFO_PARAM_COUNT];
        params.copy_from_slice(&payload[3..3 + ONBOARD_INFO_PARAM_COUNT]);
        Ok(params)
    }

    /// Applies effect `cluster_effect_index` to `cluster_index`.
    ///
    /// `params` are effect-specific (discoverable via [`Self::get_effect_info`]).
    /// `persistence` controls volatile/non-volatile storage and `power_mode`
    /// selects which power mode the effect applies to. Requires software control
    /// (see [`Self::set_sw_control`]).
    pub async fn set_rgb_cluster_effect(
        &self,
        cluster_index: u8,
        cluster_effect_index: u8,
        params: [u8; CLUSTER_EFFECT_PARAM_COUNT],
        persistence: RgbPersistence,
        power_mode: PowerModeTarget,
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = cluster_index;
        args[1] = cluster_effect_index;
        args[2..2 + CLUSTER_EFFECT_PARAM_COUNT].copy_from_slice(&params);
        args[12] = persistence.bits() | (u8::from(power_mode) << POWER_TARGET_SHIFT);
        self.endpoint.call_long(1, args).await?;
        Ok(())
    }

    /// Sets the multi-LED pattern of `cluster_index`.
    pub async fn set_multi_led_cluster_pattern(
        &self,
        cluster_index: u8,
        pattern: u8,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [cluster_index, pattern, 0]).await?;
        Ok(())
    }

    /// Reads one non-volatile configuration `capability`.
    pub async fn get_nv_config(
        &self,
        capability: RgbNvCapabilities,
    ) -> Result<RgbNvConfig, Hidpp20Error> {
        let [cap_hi, cap_lo] = capability.bits().to_be_bytes();
        let payload = self
            .endpoint
            .call(3, [GetOrSet::Get.into(), cap_hi, cap_lo])
            .await?
            .extend_payload();
        Ok(RgbNvConfig {
            capability: RgbNvCapabilities::from_bits_retain(be16(&payload, 1)),
            state: payload[3],
            param1: payload[4],
            param2: payload[5],
        })
    }

    /// Writes one non-volatile configuration entry (to EEPROM).
    pub async fn set_nv_config(
        &self,
        capability: RgbNvCapabilities,
        state: u8,
        param1: u8,
        param2: u8,
    ) -> Result<(), Hidpp20Error> {
        let [cap_hi, cap_lo] = capability.bits().to_be_bytes();
        let mut args = [0; 16];
        args[..6].copy_from_slice(&[GetOrSet::Set.into(), cap_hi, cap_lo, state, param1, param2]);
        self.endpoint.call_long(3, args).await?;
        Ok(())
    }

    /// Reads raw manufacturing LED bin parameters.
    ///
    /// `backup` reads the backup copy instead of the active one.
    pub async fn get_led_bin_info(
        &self,
        cluster_index: u8,
        led_bin_index: LedBinIndex,
        backup: bool,
    ) -> Result<[u8; LED_BIN_PARAM_COUNT], Hidpp20Error> {
        let get_or_set = if backup {
            GET_BACKUP
        } else {
            GetOrSet::Get.into()
        };
        let payload = self
            .endpoint
            .call(4, [get_or_set, cluster_index, led_bin_index.into()])
            .await?
            .extend_payload();
        let mut params = [0; LED_BIN_PARAM_COUNT];
        params.copy_from_slice(&payload[3..3 + LED_BIN_PARAM_COUNT]);
        Ok(params)
    }

    /// Stores raw manufacturing LED bin parameters.
    pub async fn set_led_bin_info(
        &self,
        cluster_index: u8,
        led_bin_index: LedBinIndex,
        params: [u8; LED_BIN_PARAM_COUNT],
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = GetOrSet::Set.into();
        args[1] = cluster_index;
        args[2] = led_bin_index.into();
        args[3..3 + LED_BIN_PARAM_COUNT].copy_from_slice(&params);
        self.endpoint.call_long(4, args).await?;
        Ok(())
    }

    /// Retrieves the software-control and event-notification flags.
    pub async fn get_sw_control(&self) -> Result<RgbSwControl, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(5, [GetOrSet::Get.into(), 0, 0])
            .await?
            .extend_payload();
        Ok(RgbSwControl {
            control: SwControlFlags::from_bits_retain(payload[1]),
            events: EventsNotificationFlags::from_bits_retain(payload[2]),
        })
    }

    /// Sets the software-control and event-notification flags.
    pub async fn set_sw_control(
        &self,
        control: SwControlFlags,
        events: EventsNotificationFlags,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(5, [GetOrSet::Set.into(), control.bits(), events.bits()])
            .await?;
        Ok(())
    }

    /// Applies an effect-sync `drift_value` (milliseconds) correction.
    ///
    /// A `cluster_index` of `0xff` targets all clusters.
    pub async fn set_effect_sync_correction(
        &self,
        cluster_index: u8,
        drift_value: i16,
    ) -> Result<(), Hidpp20Error> {
        let [drift_hi, drift_lo] = drift_value.to_be_bytes();
        let mut args = [0; 16];
        args[..4].copy_from_slice(&[cluster_index, 0, drift_hi, drift_lo]);
        self.endpoint.call_long(6, args).await?;
        Ok(())
    }

    /// Retrieves the RGB power-mode configuration.
    pub async fn get_power_mode_config(&self) -> Result<RgbPowerModeConfig, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(7, [GetOrSet::Get.into(), 0, 0])
            .await?
            .extend_payload();
        Ok(RgbPowerModeConfig::from_payload(&payload))
    }

    /// Writes the RGB power-mode configuration.
    pub async fn set_power_mode_config(
        &self,
        config: RgbPowerModeConfig,
    ) -> Result<(), Hidpp20Error> {
        let [flags_hi, flags_lo] = config.flags.to_be_bytes();
        let [psave_hi, psave_lo] = config.no_activity_timeout_to_power_save.to_be_bytes();
        let [off_hi, off_lo] = config.no_activity_timeout_to_off.to_be_bytes();
        let mut args = [0; 16];
        args[..7].copy_from_slice(&[
            GetOrSet::Set.into(),
            flags_hi,
            flags_lo,
            psave_hi,
            psave_lo,
            off_hi,
            off_lo,
        ]);
        self.endpoint.call_long(7, args).await?;
        Ok(())
    }

    /// Retrieves the current RGB power mode.
    pub async fn get_power_mode(&self) -> Result<RgbPowerMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(8, [GetOrSet::Get.into(), 0, 0])
            .await?
            .extend_payload();
        RgbPowerMode::try_from(payload[1]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the RGB power mode. Requires software control of power modes (see
    /// [`Self::set_sw_control`]).
    pub async fn set_power_mode(&self, mode: RgbPowerMode) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(8, [GetOrSet::Set.into(), mode.into(), 0])
            .await?;
        Ok(())
    }

    /// Shuts down the RGB system.
    ///
    /// Requires [`RgbExtCapabilities::SHUTDOWN`].
    pub async fn shutdown(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(9, [0; 3]).await?;
        Ok(())
    }
}
