use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        backlight::{
            BacklightFeature, BacklightMode as FirmwareMode, BacklightStatus as FirmwareStatus,
            SetBacklightConfig,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::backlight::{BacklightMode, BacklightState, BacklightStatus};
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
        set_backlight_enabled_on_channel(&channel, index, enabled).await
    })
    .await
}

async fn set_backlight_enabled_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    enabled: bool,
) -> Result<BacklightState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
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
            duration_not_powered: current.duration_not_powered,
        })
        .await
        .map_err(|e| {
            classify_hidpp_error(e, HidppOperation::WriteBacklight, BacklightFeature::ID)
        })?;
    debug!(index, enabled, "wrote backlight enable");
    read_state(&feature).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    const BACKLIGHT_INDEX: u8 = 0x08;

    /// A `0x1982` keyboard whose config reply carries all four fade-out
    /// durations (bytes 6..14). `setBacklightConfig` stores what it is sent,
    /// mapping request bytes 4..12 onto those four durations as Options+ does.
    fn stateful_backlight(
        config: [u8; 16],
        short_config_reply: bool,
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        let state = Mutex::new(config);
        ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
                return None;
            }
            let mut payload = [0u8; 16];
            let long = match (request[2], request[3] >> 4) {
                (0x00, 0x01) => {
                    payload[0] = 4;
                    false
                }
                (0x00, 0x00) => {
                    if u16::from_be_bytes([request[4], request[5]]) == BacklightFeature::ID {
                        payload[0] = BACKLIGHT_INDEX;
                    }
                    false
                }
                (BACKLIGHT_INDEX, 0x00) => {
                    payload = *state.lock().expect("backlight state");
                    !short_config_reply
                }
                (BACKLIGHT_INDEX, 0x01) => {
                    let mut current = state.lock().expect("backlight state");
                    current[0] = request[4];
                    current[1] = request[5];
                    current[5] = request[7];
                    current[6..14].copy_from_slice(&request[8..16]);
                    true
                }
                (BACKLIGHT_INDEX, 0x02) => {
                    payload[..4].copy_from_slice(&[
                        8,
                        4,
                        u8::from(FirmwareStatus::AlsAutomatic),
                        u8::from(hidpp::feature::backlight::BacklightEffect::Static),
                    ]);
                    true
                }
                _ => return None,
            };
            let mut response = vec![0u8; if long { 20 } else { 7 }];
            response[0] = if long { 0x11 } else { 0x10 };
            response[1..4].copy_from_slice(&request[1..4]);
            let payload_len = response.len() - 4;
            response[4..].copy_from_slice(&payload[..payload_len]);
            Some(response)
        })
    }

    /// Enabled, automatic mode, level 4, durations 0x0001 / 0x0002 / 0x0003
    /// and a non-zero fourth duration 0x0405.
    fn config_with_four_durations() -> [u8; 16] {
        let mut config = [0u8; 16];
        config[0] = 1;
        config[1] = u8::from(FirmwareMode::Automatic) << 3;
        config[5] = 4;
        config[6..14].copy_from_slice(&[0x01, 0, 0x02, 0, 0x03, 0, 0x05, 0x04]);
        config
    }

    fn config_writes(handle: &crate::replay::ReplayChannelHandle) -> Vec<Vec<u8>> {
        handle
            .written_reports()
            .into_iter()
            .filter(|report| {
                report.len() >= 16 && report[2] == BACKLIGHT_INDEX && report[3] >> 4 == 0x01
            })
            .map(|report| report[4..16].to_vec())
            .collect()
    }

    #[tokio::test]
    async fn toggling_keeps_the_fourth_fade_out_duration() -> Result<(), WriteError> {
        let (raw, handle) = stateful_backlight(config_with_four_durations(), false);
        let channel = scripted_channel(raw).await;
        let state = set_backlight_enabled_on_channel(&channel, 0xff, false).await?;
        assert!(!state.enabled);
        assert_eq!(
            config_writes(&handle),
            vec![vec![
                0,
                u8::from(FirmwareMode::Automatic) << 3,
                0xff,
                4,
                0x01,
                0,
                0x02,
                0,
                0x03,
                0,
                // Bytes 10..12: the fourth duration, written back as read.
                0x05,
                0x04,
            ]],
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_short_config_reply_is_never_written_back() {
        // A three-byte reply would zero-fill the level and every duration.
        let (raw, handle) = stateful_backlight(config_with_four_durations(), true);
        let channel = scripted_channel(raw).await;
        assert!(matches!(
            set_backlight_enabled_on_channel(&channel, 0xff, false).await,
            Err(WriteError::UnsupportedResponse { .. })
        ));
        assert!(config_writes(&handle).is_empty());
    }

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
