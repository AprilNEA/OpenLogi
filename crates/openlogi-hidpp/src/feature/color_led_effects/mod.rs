//! Implements the `ColorLedEffects` feature (ID `0x8070`, version 7), the
//! per-zone RGB effect engine used by Logitech keyboards and mice.
//!
//! Each device exposes one or more LED *zones*; each zone supports a set of
//! *effects* (fixed color, breathing, color wave, …). An effect is applied with
//! [`set_zone_effect`](ColorLedEffectsFeature::set_zone_effect), whose ten
//! parameter bytes have effect-specific meaning, and can be stored volatilely or
//! in EEPROM via [`Persistence`].
//!
//! All multi-byte fields in this feature are big-endian.

pub mod event;
pub mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use event::ColorLedEffectsEvent;
pub use types::{
    ColorLedInfo, CyclingDirection, EffectId, EffectSettings, ExtCapabilities, LedBinIndex,
    LedBinInfo, LocationEffect, NvCapabilities, NvCapabilityState, NvConfig, Persistence,
    PersistenceSource, PersistencyCapabilities, Rgb, SwControl, SwControlState,
    ZONE_EFFECT_PARAM_COUNT, ZoneEffect, ZoneEffectInfo, ZoneInfo,
};

use self::types::be16;
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Implements the `ColorLedEffects` / `0x8070` feature.
pub struct ColorLedEffectsFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<ColorLedEffectsEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for ColorLedEffectsFeature {
    const ID: u16 = 0x8070;
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

impl Feature for ColorLedEffectsFeature {}

impl EmittingFeature<ColorLedEffectsEvent> for ColorLedEffectsFeature {
    fn listen(&self) -> async_channel::Receiver<ColorLedEffectsEvent> {
        self.emitter.create_receiver()
    }
}

impl ColorLedEffectsFeature {
    /// Retrieves the zone count and capability bitmasks.
    pub async fn get_info(&self) -> Result<ColorLedInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ColorLedInfo::from_payload(&payload))
    }

    /// Retrieves information about `zone_index`.
    pub async fn get_zone_info(&self, zone_index: u8) -> Result<ZoneInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [zone_index, 0, 0])
            .await?
            .extend_payload();
        ZoneInfo::from_payload(&payload)
    }

    /// Retrieves information about effect `zone_effect_index` of `zone_index`.
    pub async fn get_zone_effect_info(
        &self,
        zone_index: u8,
        zone_effect_index: u8,
    ) -> Result<ZoneEffectInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [zone_index, zone_effect_index, 0])
            .await?
            .extend_payload();
        ZoneEffectInfo::from_payload(&payload)
    }

    /// Applies effect `zone_effect_index` to `zone_index` with effect-specific
    /// `params`.
    ///
    /// The meaning of each parameter byte depends on the effect's
    /// [`EffectId`] (discoverable with [`Self::get_zone_effect_info`]). For
    /// example, the [`EffectId::FixedColor`] effect uses the first three
    /// parameters as red, green and blue.
    pub async fn set_zone_effect(
        &self,
        zone_index: u8,
        zone_effect_index: u8,
        params: [u8; ZONE_EFFECT_PARAM_COUNT],
        persistence: Persistence,
    ) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = zone_index;
        args[1] = zone_effect_index;
        args[2..2 + ZONE_EFFECT_PARAM_COUNT].copy_from_slice(&params);
        args[12] = persistence.into();
        self.endpoint.call_long(3, args).await?;
        Ok(())
    }

    /// Reads one non-volatile configuration `capability`.
    ///
    /// Exactly one [`NvCapabilities`] bit must be set.
    pub async fn get_nv_config(
        &self,
        capability: NvCapabilities,
    ) -> Result<NvConfig, Hidpp20Error> {
        validate_single_nv_capability(capability)?;
        let [cap_hi, cap_lo] = capability.bits().to_be_bytes();
        let payload = self
            .endpoint
            .call(4, [cap_hi, cap_lo, 0])
            .await?
            .extend_payload();
        Ok(NvConfig {
            capability: NvCapabilities::from_bits_retain(be16(&payload, 0)),
            state: NvCapabilityState::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            param1: payload[3],
            param2: payload[4],
        })
    }

    /// Writes one non-volatile configuration entry (to EEPROM, so use sparingly).
    pub async fn set_nv_config(
        &self,
        capability: NvCapabilities,
        state: NvCapabilityState,
        param1: u8,
        param2: u8,
    ) -> Result<(), Hidpp20Error> {
        validate_single_nv_capability(capability)?;
        let [cap_hi, cap_lo] = capability.bits().to_be_bytes();
        let mut args = [0; 16];
        args[..5].copy_from_slice(&[cap_hi, cap_lo, state.into(), param1, param2]);
        self.endpoint.call_long(5, args).await?;
        Ok(())
    }

    /// Reads manufacturing LED bin information.
    pub async fn get_led_bin_info(
        &self,
        zone_index: u8,
        led_bin_index: LedBinIndex,
    ) -> Result<LedBinInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(6, [zone_index, led_bin_index.into(), 0])
            .await?
            .extend_payload();
        LedBinInfo::from_payload(&payload)
    }

    /// Retrieves whether firmware or software owns the LEDs.
    pub async fn get_sw_control(&self) -> Result<SwControlState, Hidpp20Error> {
        let payload = self.endpoint.call(7, [0; 3]).await?.extend_payload();
        Ok(SwControlState {
            control: SwControl::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            sync_events: payload[1] != 0,
        })
    }

    /// Takes or releases software control of the LEDs.
    ///
    /// `sync_events` enables the [`ColorLedEffectsEvent::SyncEffect`] event. This
    /// is not stored in EEPROM.
    pub async fn set_sw_control(
        &self,
        control: SwControl,
        sync_events: bool,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(8, [control.into(), u8::from(sync_events), 0])
            .await?;
        Ok(())
    }

    /// Reads the effect settings of `zone_index`.
    ///
    /// Not supported when [`ExtCapabilities::NO_GET_EFFECT_SETTINGS`] is set.
    pub async fn get_effect_settings(
        &self,
        zone_index: u8,
        source: PersistenceSource,
    ) -> Result<EffectSettings, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(9, [zone_index, source.into(), 0])
            .await?
            .extend_payload();
        Ok(EffectSettings::from_payload(&payload))
    }

    /// Clears the effect settings of `zone_index`, reverting it to the default
    /// mode.
    pub async fn clear_effect_settings(&self, zone_index: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.call(10, [zone_index, 0, 0]).await?;
        Ok(())
    }

    /// Sets the color-cycling direction.
    pub async fn set_cycling_direction(
        &self,
        direction: CyclingDirection,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call(11, [direction.into(), 0, 0]).await?;
        Ok(())
    }

    /// Retrieves the color currently displayed by `zone_index`.
    pub async fn get_current_color(&self, zone_index: u8) -> Result<Rgb, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(12, [zone_index, 0, 0])
            .await?
            .extend_payload();
        Ok(Rgb {
            red: payload[1],
            green: payload[2],
            blue: payload[3],
        })
    }

    /// Synchronizes effect timing across devices by applying a `drift_value`
    /// correction (milliseconds).
    ///
    /// Valid only while sync events are enabled. A `zone_index` of `0xff` targets
    /// all zones.
    pub async fn synchronize_effect(
        &self,
        zone_index: u8,
        drift_value: i16,
    ) -> Result<(), Hidpp20Error> {
        let [drift_hi, drift_lo] = drift_value.to_be_bytes();
        let mut args = [0; 16];
        args[..4].copy_from_slice(&[zone_index, 0, drift_hi, drift_lo]);
        self.endpoint.call_long(13, args).await?;
        Ok(())
    }

    /// Retrieves the currently configured effect of `zone_index`.
    ///
    /// Requires [`ExtCapabilities::GET_ZONE_EFFECT`].
    pub async fn get_zone_effect(
        &self,
        zone_index: u8,
        source: PersistenceSource,
    ) -> Result<ZoneEffect, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(14, [zone_index, source.into(), 0])
            .await?
            .extend_payload();
        Ok(ZoneEffect::from_payload(&payload))
    }

    /// Stores manufacturing LED bin information and returns the device's echo.
    ///
    /// Requires [`ExtCapabilities::SET_LED_BIN_INFO`].
    pub async fn set_led_bin_info(&self, info: &LedBinInfo) -> Result<LedBinInfo, Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = info.zone_index;
        args[1] = info.led_bin_index.into();
        args[2..4].copy_from_slice(&info.red.to_be_bytes());
        args[4..6].copy_from_slice(&info.green.to_be_bytes());
        args[6..8].copy_from_slice(&info.blue.to_be_bytes());
        args[8..10].copy_from_slice(&info.white.to_be_bytes());
        let payload = self.endpoint.call_long(15, args).await?.extend_payload();
        LedBinInfo::from_payload(&payload)
    }
}

fn validate_single_nv_capability(capability: NvCapabilities) -> Result<(), Hidpp20Error> {
    if capability.bits().count_ones() != 1 {
        return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
    }
    Ok(())
}
