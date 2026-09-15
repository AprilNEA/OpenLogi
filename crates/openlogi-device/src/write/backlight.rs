use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        backlight::{
            BacklightEffect as FirmwareEffect, BacklightFeature, BacklightMode as FirmwareMode,
            BacklightStatus as FirmwareStatus, SetBacklightConfig,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::backlight::{BacklightEffect, BacklightMode, BacklightState, BacklightStatus};
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Map the fork's `0x1982` mode onto OpenLogi's [`BacklightMode`]. The source
/// enum is `#[non_exhaustive]`; an unmodelled future variant maps to
/// [`BacklightMode::None`], which callers treat as "firmware picks".
fn mode_from_firmware(mode: FirmwareMode) -> BacklightMode {
    match mode {
        FirmwareMode::Automatic => BacklightMode::Automatic,
        FirmwareMode::TemporaryManual => BacklightMode::TemporaryManual,
        FirmwareMode::PermanentManual => BacklightMode::PermanentManual,
        _ => BacklightMode::None,
    }
}

/// The inverse of [`mode_from_firmware`], used when writing a config back.
///
/// [`BacklightMode::TemporaryManual`] is the mode the *keyboard* enters when
/// the user presses its backlight keys; `setBacklightConfig` cannot write it,
/// so it is sent as [`FirmwareMode::Automatic`] — the firmware's own fallback
/// once software takes over the level.
fn mode_to_firmware(mode: BacklightMode) -> FirmwareMode {
    match mode {
        BacklightMode::None => FirmwareMode::None,
        BacklightMode::Automatic | BacklightMode::TemporaryManual => FirmwareMode::Automatic,
        BacklightMode::PermanentManual => FirmwareMode::PermanentManual,
    }
}

/// Map the fork's `0x1982` effect onto OpenLogi's [`BacklightEffect`].
///
/// Unlike [`mode_from_firmware`]/[`status_from_firmware`], this one is
/// fallible rather than defaulting an unmodelled variant to a guess:
/// `set_backlight_effect`'s read-back is compared against the *requested*
/// effect to confirm the write applied, so silently collapsing an unknown
/// future variant to [`BacklightEffect::Static`] could make that comparison
/// report success for the wrong effect. Errors the same way an unrecognized
/// wire byte already does one layer down (`get_backlight_info`'s own
/// `TryFromPrimitive` decode).
fn effect_from_firmware(effect: FirmwareEffect) -> Result<BacklightEffect, WriteError> {
    match effect {
        FirmwareEffect::Static => Ok(BacklightEffect::Static),
        FirmwareEffect::None => Ok(BacklightEffect::None),
        FirmwareEffect::Breathing => Ok(BacklightEffect::Breathing),
        FirmwareEffect::Contrast => Ok(BacklightEffect::Contrast),
        FirmwareEffect::Reaction => Ok(BacklightEffect::Reaction),
        FirmwareEffect::Random => Ok(BacklightEffect::Random),
        FirmwareEffect::Waves => Ok(BacklightEffect::Waves),
        _ => Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadBacklight,
            feature_hex: BacklightFeature::ID,
        }),
    }
}

/// The inverse of [`effect_from_firmware`], used when writing a config back.
fn effect_to_firmware(effect: BacklightEffect) -> FirmwareEffect {
    match effect {
        BacklightEffect::Static => FirmwareEffect::Static,
        BacklightEffect::None => FirmwareEffect::None,
        BacklightEffect::Breathing => FirmwareEffect::Breathing,
        BacklightEffect::Contrast => FirmwareEffect::Contrast,
        BacklightEffect::Reaction => FirmwareEffect::Reaction,
        BacklightEffect::Random => FirmwareEffect::Random,
        BacklightEffect::Waves => FirmwareEffect::Waves,
    }
}

/// Map the fork's `0x1982` status onto OpenLogi's [`BacklightStatus`]. The
/// source enum is `#[non_exhaustive]`; an unmodelled future variant maps to
/// [`BacklightStatus::AlsAutomatic`], the firmware's out-of-box behaviour.
fn status_from_firmware(status: FirmwareStatus) -> BacklightStatus {
    match status {
        FirmwareStatus::DisabledBySoftware => BacklightStatus::DisabledBySoftware,
        FirmwareStatus::DisabledByCriticalBattery => BacklightStatus::DisabledByCriticalBattery,
        FirmwareStatus::AlsSaturated => BacklightStatus::AlsSaturated,
        FirmwareStatus::TemporaryManual => BacklightStatus::TemporaryManual,
        FirmwareStatus::PermanentManual => BacklightStatus::PermanentManual,
        _ => BacklightStatus::AlsAutomatic,
    }
}

/// Read `getBacklightConfig` + `getBacklightInfo` and merge them into a
/// [`BacklightState`].
async fn read_state(feature: &BacklightFeature) -> Result<BacklightState, WriteError> {
    let config = feature.get_backlight_config().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
    })?;
    let info = feature.get_backlight_info().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
    })?;
    Ok(BacklightState {
        enabled: config.enabled,
        mode: mode_from_firmware(config.mode),
        status: status_from_firmware(info.status),
        current_level: info.current_level,
        nb_levels: info.nb_levels,
    })
}

/// Read the current backlight state of the keyboard on `route`.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x1982` — RGB
/// keyboards (`0x8070` / `0x8080`) and every mouse fall in that group.
pub async fn get_backlight(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<BacklightState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_backlight_on_channel(&channel, index).await
    })
    .await
}

/// Read the current backlight state on an already-open [`SharedChannel`].
pub async fn get_backlight_on(shared: &SharedChannel) -> Result<BacklightState, WriteError> {
    get_backlight_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_backlight_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<BacklightState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<BacklightFeature>(&mut device).await?;
    read_state(&feature).await
}

/// Enable or disable the backlight on `route`, and return the read-back state.
///
/// Disabling sets the firmware's own master switch: the LEDs stay dark
/// regardless of the ambient-light and proximity sensors, and the device
/// reports [`BacklightStatus::DisabledBySoftware`]. The write goes to
/// non-volatile memory, so it survives reconnects, host switches, and power
/// cycles — nothing needs to re-apply it.
///
/// The effect, brightness level, and fade-out durations are read first and
/// written back unchanged, so they return with a later `enabled = true`.
///
/// The mode survives too, except from [`BacklightMode::TemporaryManual`] — the
/// state the keyboard enters on its own when the user presses its backlight
/// keys. `setBacklightConfig` cannot write that mode, so it lands in
/// [`BacklightMode::Automatic`] and the level goes back under ambient-light
/// control. Promoting it to [`BacklightMode::PermanentManual`] would hold the
/// level but make a deliberately temporary adjustment permanent, so the
/// firmware's own fallback wins instead. Read the mode first and tell the user
/// when this applies.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x1982`.
pub async fn set_backlight_enabled(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    enabled: bool,
) -> Result<BacklightState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<BacklightFeature>(&mut device).await?;

        let current = feature.get_backlight_config().await.map_err(|e| {
            classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
        })?;

        feature
            .set_backlight_config(SetBacklightConfig {
                enabled,
                options: current.options,
                mode: mode_to_firmware(mode_from_firmware(current.mode)),
                // `None` sends the 0xff "do not change" sentinel, keeping
                // whichever effect the device already runs.
                effect: None,
                current_level: current.current_level,
                duration_hands_out: current.duration_hands_out,
                duration_hands_in: current.duration_hands_in,
                duration_powered: current.duration_powered,
            })
            .await
            .map_err(|e| {
                classify_hidpp_error(e, HidppOperation::WriteBacklight, BacklightFeature::ID)
            })?;

        debug!(index, enabled, "wrote backlight enable");
        read_state(&feature).await
    })
    .await
}

/// Read the backlight effect currently running on `route`.
///
/// Not part of [`get_backlight`]'s [`BacklightState`] (see that type's own
/// doc comment on why) — a separate `getBacklightInfo` round trip.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x1982`.
pub async fn get_backlight_effect(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<BacklightEffect, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<BacklightFeature>(&mut device).await?;
        let info = feature.get_backlight_info().await.map_err(|e| {
            classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
        })?;
        effect_from_firmware(info.effect)
    })
    .await
}

/// Set a predefined backlight effect on `route`, and return the read-back
/// state. Persistent (`setBacklightConfig`, non-volatile) — survives
/// reconnects, host switches, and power cycles, matching
/// [`set_backlight_enabled`]. Every other field (enabled, mode, level,
/// fade-out durations) is read first and written back unchanged.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x1982`.
pub async fn set_backlight_effect(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    effect: BacklightEffect,
) -> Result<BacklightState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<BacklightFeature>(&mut device).await?;

        let current = feature.get_backlight_config().await.map_err(|e| {
            classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
        })?;

        feature
            .set_backlight_config(SetBacklightConfig {
                enabled: current.enabled,
                options: current.options,
                mode: mode_to_firmware(mode_from_firmware(current.mode)),
                effect: Some(effect_to_firmware(effect)),
                current_level: current.current_level,
                duration_hands_out: current.duration_hands_out,
                duration_hands_in: current.duration_hands_in,
                duration_powered: current.duration_powered,
            })
            .await
            .map_err(|e| {
                classify_hidpp_error(e, HidppOperation::WriteBacklight, BacklightFeature::ID)
            })?;

        debug!(index, ?effect, "wrote backlight effect");
        read_state(&feature).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_modes_round_trip_through_openlogi_modes() {
        assert_eq!(
            mode_from_firmware(FirmwareMode::Automatic),
            BacklightMode::Automatic
        );
        assert_eq!(
            mode_from_firmware(FirmwareMode::PermanentManual),
            BacklightMode::PermanentManual
        );
        assert_eq!(mode_from_firmware(FirmwareMode::None), BacklightMode::None);
    }

    #[test]
    fn temporary_manual_is_downgraded_because_software_cannot_write_it() {
        assert_eq!(
            mode_from_firmware(FirmwareMode::TemporaryManual),
            BacklightMode::TemporaryManual
        );
        assert_eq!(
            mode_to_firmware(BacklightMode::TemporaryManual),
            FirmwareMode::Automatic
        );
    }

    #[test]
    fn writable_modes_survive_the_read_write_round_trip() {
        for mode in [FirmwareMode::None, FirmwareMode::PermanentManual] {
            assert_eq!(mode_to_firmware(mode_from_firmware(mode)), mode);
        }
    }

    #[test]
    fn firmware_effects_round_trip_through_openlogi_effects() {
        assert_eq!(
            effect_from_firmware(FirmwareEffect::Breathing).expect("known effect"),
            BacklightEffect::Breathing
        );
        assert_eq!(
            effect_from_firmware(FirmwareEffect::Waves).expect("known effect"),
            BacklightEffect::Waves
        );
        assert_eq!(
            effect_from_firmware(FirmwareEffect::Static).expect("known effect"),
            BacklightEffect::Static
        );
    }

    #[test]
    fn every_effect_survives_the_read_write_round_trip() {
        for effect in [
            FirmwareEffect::Static,
            FirmwareEffect::None,
            FirmwareEffect::Breathing,
            FirmwareEffect::Contrast,
            FirmwareEffect::Reaction,
            FirmwareEffect::Random,
            FirmwareEffect::Waves,
        ] {
            let mapped = effect_from_firmware(effect).expect("every known variant maps");
            assert_eq!(effect_to_firmware(mapped), effect);
        }
    }

    #[test]
    fn software_disable_status_is_mapped() {
        assert_eq!(
            status_from_firmware(FirmwareStatus::DisabledBySoftware),
            BacklightStatus::DisabledBySoftware
        );
        assert_eq!(
            status_from_firmware(FirmwareStatus::AlsSaturated),
            BacklightStatus::AlsSaturated
        );
    }
}
