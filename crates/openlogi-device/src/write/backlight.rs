use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        backlight::{
            BacklightConfig, BacklightFeature, BacklightInfo, BacklightMode as FirmwareMode,
            BacklightOptions, BacklightStatus as FirmwareStatus, SetBacklightConfig,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::backlight::{
    BacklightChange, BacklightControls, BacklightMode, BacklightState, BacklightStatus,
};
use crate::channel::route::DeviceRoute;

use super::{
    HidppOperation, WriteError, classify_hidpp_error, open_feature_with_version, with_route,
};

/// The first `0x1982` version on which software picks the level.
///
/// Solaar's `Backlight2Level` setting requires it. Logi Options+ 2.8 does not
/// check the version in `_set_backlight_level`, so this is the conservative
/// choice: an older keyboard is refused a level rather than sent one it may
/// not apply.
const LEVEL_VERSION: u8 = 3;

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

/// What software may change, from the `getBacklightConfig` capability bits,
/// the runtime feature version and the level count.
fn controls_from(options: BacklightOptions, version: u8, nb_levels: u8) -> BacklightControls {
    let manual = options.contains(BacklightOptions::PERM_MANUAL_SUPPORTED);
    BacklightControls {
        // A switch between the sensor and a held level needs both.
        automatic: manual && options.contains(BacklightOptions::AUTO_MODE_SUPPORTED),
        level: manual && version >= LEVEL_VERSION && nb_levels > 1,
        power_save: options.contains(BacklightOptions::PWR_SAVE_SUPPORTED),
    }
}

/// Read `getBacklightConfig` + `getBacklightInfo`.
async fn read_raw(
    feature: &BacklightFeature,
) -> Result<(BacklightConfig, BacklightInfo), WriteError> {
    let config = feature.get_backlight_config().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
    })?;
    let info = feature.get_backlight_info().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadBacklight, BacklightFeature::ID)
    })?;
    Ok((config, info))
}

/// Merge the two replies into a [`BacklightState`].
fn state_from(config: &BacklightConfig, info: &BacklightInfo, version: u8) -> BacklightState {
    BacklightState {
        enabled: config.enabled,
        mode: mode_from_firmware(config.mode),
        status: status_from_firmware(info.status),
        current_level: info.current_level,
        nb_levels: info.nb_levels,
        power_save: config.options.contains(BacklightOptions::PWR_SAVE),
        controls: controls_from(config.options, version, info.nb_levels),
    }
}

/// The level `change` makes the keyboard hold, when software chooses it.
///
/// [`BacklightChange::Level`] names its own. [`BacklightChange::Automatic`]
/// `(false)` freezes the level the keyboard shows now, so turning the sensor
/// off changes nothing visible and never guesses at the stored level, which
/// may be a deliberate `0`. Below feature version 3 software does not choose
/// the level, so the stored one is written back as read and not checked.
fn held_level(
    change: BacklightChange,
    info: &BacklightInfo,
    controls: BacklightControls,
) -> Option<u8> {
    match change {
        BacklightChange::Level(level) => Some(level),
        BacklightChange::Automatic(false) if controls.level => Some(info.current_level),
        _ => None,
    }
}

/// The `setBacklightConfig` request that applies `change` to `config`,
/// holding `held` (see [`held_level`]).
///
/// Everything `change` does not touch is written back as read: the effect
/// (sent as "do not change"), the level, the fade-out durations and the
/// other options. The mode survives too, except from
/// [`BacklightMode::TemporaryManual`], which `setBacklightConfig` cannot
/// write; see [`update_backlight`].
fn request_for(
    config: &BacklightConfig,
    change: BacklightChange,
    held: Option<u8>,
) -> SetBacklightConfig {
    let mut request = SetBacklightConfig {
        enabled: config.enabled,
        options: config.options,
        mode: mode_to_firmware(mode_from_firmware(config.mode)),
        // `None` sends the 0xff "do not change" sentinel, keeping whichever
        // effect the device already runs.
        effect: None,
        current_level: config.current_level,
        duration_hands_out: config.duration_hands_out,
        duration_hands_in: config.duration_hands_in,
        duration_powered: config.duration_powered,
        duration_not_powered: config.duration_not_powered,
    };
    match change {
        BacklightChange::Enabled(enabled) => request.enabled = enabled,
        BacklightChange::Automatic(true) => request.mode = FirmwareMode::Automatic,
        BacklightChange::Automatic(false) | BacklightChange::Level(_) => {
            request.mode = FirmwareMode::PermanentManual;
            if let Some(level) = held {
                request.current_level = level;
            }
        }
        BacklightChange::PowerSave(on) => request.options.set(BacklightOptions::PWR_SAVE, on),
    }
    request
}

/// Whether the configuration read back after a write carries `change`,
/// holding `held`.
fn carries(config: &BacklightConfig, change: BacklightChange, held: Option<u8>) -> bool {
    let mode = mode_from_firmware(config.mode);
    match change {
        BacklightChange::Enabled(enabled) => config.enabled == enabled,
        BacklightChange::Automatic(true) => {
            matches!(
                mode,
                BacklightMode::Automatic | BacklightMode::TemporaryManual
            )
        }
        BacklightChange::Automatic(false) | BacklightChange::Level(_) => {
            mode == BacklightMode::PermanentManual
                && held.is_none_or(|level| config.current_level == level)
        }
        BacklightChange::PowerSave(on) => config.options.contains(BacklightOptions::PWR_SAVE) == on,
    }
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
    let (feature, version) = open_feature_with_version::<BacklightFeature>(&mut device).await?;
    let (config, info) = read_raw(&feature).await?;
    Ok(state_from(&config, &info, version))
}

/// Apply one [`BacklightChange`] to the keyboard on `route`, and return the
/// read-back state.
///
/// A read-modify-write of the whole `setBacklightConfig` into non-volatile
/// memory, as Logi Options+ does: the change survives reconnects, host
/// switches and power cycles, so nothing needs to re-apply it. Whatever
/// `change` does not touch is written back as read — the effect, the level,
/// the fade-out durations and the other options — so turning the backlight
/// off and on again returns it as it was. Disabling sets the firmware's own
/// master switch: the LEDs stay dark whatever the sensors see, and the
/// keyboard reports [`BacklightStatus::DisabledBySoftware`].
///
/// The mode survives too, except from [`BacklightMode::TemporaryManual`] —
/// the state the keyboard enters on its own when the user presses its
/// backlight keys. `setBacklightConfig` cannot write that mode, so it lands
/// in [`BacklightMode::Automatic`] and the level goes back under ambient-light
/// control. Promoting it to [`BacklightMode::PermanentManual`] would hold the
/// level but make a deliberately temporary adjustment permanent, so the
/// firmware's own fallback wins instead. Read the mode first and tell the user
/// when this applies.
///
/// # Errors
///
/// - `FeatureUnsupported` when the device does not expose HID++ `0x1982` —
///   RGB keyboards (`0x8070` / `0x8080`) and every mouse fall in that group.
/// - `BacklightChangeUnsupported` when a fresh read shows the keyboard does
///   not offer `change` (see [`BacklightState::accepts`]); nothing is written.
/// - `BacklightNotApplied` when the keyboard acknowledges the write but the
///   configuration read back afterwards does not carry `change`.
pub async fn update_backlight(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: BacklightChange,
) -> Result<BacklightState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        update_backlight_on_channel(&channel, index, change).await
    })
    .await
}

/// [`update_backlight`] on an already-open [`SharedChannel`].
pub async fn update_backlight_on(
    shared: &SharedChannel,
    change: BacklightChange,
) -> Result<BacklightState, WriteError> {
    update_backlight_on_channel(shared.channel(), shared.device_index(), change).await
}

async fn update_backlight_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    change: BacklightChange,
) -> Result<BacklightState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let (feature, version) = open_feature_with_version::<BacklightFeature>(&mut device).await?;
    let (config, info) = read_raw(&feature).await?;
    let before = state_from(&config, &info, version);
    if !before.accepts(change) {
        return Err(WriteError::BacklightChangeUnsupported { change });
    }
    let held = held_level(change, &info, before.controls);
    feature
        .set_backlight_config(request_for(&config, change, held))
        .await
        .map_err(|e| {
            classify_hidpp_error(e, HidppOperation::WriteBacklight, BacklightFeature::ID)
        })?;
    debug!(index, %change, "wrote backlight config");
    let (config, info) = read_raw(&feature).await?;
    if !carries(&config, change, held) {
        return Err(WriteError::BacklightNotApplied { change });
    }
    Ok(state_from(&config, &info, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    const BACKLIGHT_INDEX: u8 = 0x08;
    /// `getBacklightInfo`: eight levels, the sensor currently showing 4.
    const NB_LEVELS: u8 = 8;
    const SHOWN_LEVEL: u8 = 4;

    /// A scripted `0x1982` keyboard.
    #[derive(Clone, Copy)]
    struct Keyboard {
        /// The `getBacklightConfig` payload; `setBacklightConfig` stores into
        /// it as firmware does.
        config: [u8; 16],
        /// The feature version the root `getFeature` reports.
        version: u8,
        /// Answer `getBacklightConfig` with a short report.
        short_config_reply: bool,
        /// Acknowledge `setBacklightConfig` but keep the old level.
        ignores_level: bool,
    }

    impl Keyboard {
        /// Enabled, automatic mode, level 4, durations 0x0001 / 0x0002 /
        /// 0x0003 and a non-zero fourth duration 0x0405; version 3 with the
        /// sensor, a held level and battery saving supported, saving off.
        fn v3() -> Self {
            let mut config = [0u8; 16];
            config[0] = 1;
            config[1] = u8::from(FirmwareMode::Automatic) << 3;
            config[2] = capability_byte(
                BacklightOptions::AUTO_MODE_SUPPORTED
                    | BacklightOptions::PERM_MANUAL_SUPPORTED
                    | BacklightOptions::PWR_SAVE_SUPPORTED,
            );
            config[5] = 4;
            config[6..14].copy_from_slice(&[0x01, 0, 0x02, 0, 0x03, 0, 0x05, 0x04]);
            Self {
                config,
                version: 3,
                short_config_reply: false,
                ignores_level: false,
            }
        }

        fn mode(mut self, mode: FirmwareMode) -> Self {
            self.config[1] = (self.config[1] & !(0b11 << 3)) | (u8::from(mode) << 3);
            self
        }

        fn level(mut self, level: u8) -> Self {
            self.config[5] = level;
            self
        }

        fn options_low(mut self, options: BacklightOptions) -> Self {
            self.config[1] |= options.bits().to_le_bytes()[0];
            self
        }

        fn capabilities(mut self, options: BacklightOptions) -> Self {
            self.config[2] = capability_byte(options);
            self
        }
    }

    /// The high (capability) byte of the config's options field.
    fn capability_byte(options: BacklightOptions) -> u8 {
        options.bits().to_le_bytes()[1]
    }

    fn stateful_backlight(
        keyboard: Keyboard,
    ) -> (ScriptedRawHidChannel, crate::replay::ReplayChannelHandle) {
        let state = Mutex::new(keyboard.config);
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
                        payload[2] = keyboard.version;
                    }
                    false
                }
                (BACKLIGHT_INDEX, 0x00) => {
                    payload = *state.lock().expect("backlight state");
                    !keyboard.short_config_reply
                }
                (BACKLIGHT_INDEX, 0x01) => {
                    let mut current = state.lock().expect("backlight state");
                    current[0] = request[4];
                    current[1] = request[5];
                    if !keyboard.ignores_level {
                        current[5] = request[7];
                    }
                    current[6..14].copy_from_slice(&request[8..16]);
                    true
                }
                (BACKLIGHT_INDEX, 0x02) => {
                    payload[..4].copy_from_slice(&[
                        NB_LEVELS,
                        SHOWN_LEVEL,
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

    /// Apply `change` to `keyboard`; the result and every config written.
    async fn update(
        keyboard: Keyboard,
        change: BacklightChange,
    ) -> (Result<BacklightState, WriteError>, Vec<Vec<u8>>) {
        let (raw, handle) = stateful_backlight(keyboard);
        let channel = scripted_channel(raw).await;
        let result = update_backlight_on_channel(&channel, 0xff, change).await;
        (result, config_writes(&handle))
    }

    /// The `setBacklightConfig` arguments for `keyboard`'s durations with
    /// these leading bytes; the effect is always "do not change".
    fn written(enabled: u8, options_byte: u8, level: u8) -> Vec<u8> {
        vec![
            enabled,
            options_byte,
            0xff,
            level,
            0x01,
            0,
            0x02,
            0,
            0x03,
            0,
            // Bytes 10..12: the fourth duration, written back as read.
            0x05,
            0x04,
        ]
    }

    const AUTOMATIC: u8 = 1 << 3;
    const PERMANENT: u8 = 3 << 3;

    #[tokio::test]
    async fn toggling_keeps_the_fourth_fade_out_duration() -> Result<(), WriteError> {
        let (state, writes) = update(Keyboard::v3(), BacklightChange::Enabled(false)).await;
        assert!(!state?.enabled);
        assert_eq!(writes, vec![written(0, AUTOMATIC, 4)]);
        Ok(())
    }

    #[tokio::test]
    async fn a_short_config_reply_is_never_written_back() {
        // A three-byte reply would zero-fill the level and every duration.
        let keyboard = Keyboard {
            short_config_reply: true,
            ..Keyboard::v3()
        };
        let (state, writes) = update(keyboard, BacklightChange::Enabled(false)).await;
        assert!(matches!(state, Err(WriteError::UnsupportedResponse { .. })));
        assert!(writes.is_empty());
    }

    #[tokio::test]
    async fn a_level_is_held_in_permanent_manual_mode() -> Result<(), WriteError> {
        let (state, writes) = update(Keyboard::v3(), BacklightChange::Level(2)).await;
        let state = state?;
        assert_eq!(writes, vec![written(1, PERMANENT, 2)]);
        assert_eq!(state.mode, BacklightMode::PermanentManual);
        assert!(state.controls.level);
        Ok(())
    }

    #[tokio::test]
    async fn turning_the_sensor_off_freezes_the_level_shown_now() -> Result<(), WriteError> {
        // Whatever level is stored — one software set, or a deliberate 0 —
        // the keyboard keeps showing what the sensor chose.
        for stored in [6, 0] {
            let keyboard = Keyboard::v3().level(stored);
            let (state, writes) = update(keyboard, BacklightChange::Automatic(false)).await;
            assert_eq!(
                state?.mode,
                BacklightMode::PermanentManual,
                "stored {stored}"
            );
            assert_eq!(
                writes,
                vec![written(1, PERMANENT, SHOWN_LEVEL)],
                "stored {stored}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn before_version_3_turning_the_sensor_off_keeps_the_stored_level()
    -> Result<(), WriteError> {
        // Software does not choose the level there, so the stored one goes
        // back as read and a keyboard that ignores the byte still succeeds.
        let keyboard = Keyboard {
            version: 2,
            ignores_level: true,
            ..Keyboard::v3().level(6)
        };
        let (state, writes) = update(keyboard, BacklightChange::Automatic(false)).await;
        assert_eq!(state?.mode, BacklightMode::PermanentManual);
        assert_eq!(writes, vec![written(1, PERMANENT, 6)]);
        Ok(())
    }

    #[tokio::test]
    async fn turning_the_sensor_on_keeps_the_stored_level() -> Result<(), WriteError> {
        let keyboard = Keyboard::v3().mode(FirmwareMode::PermanentManual).level(2);
        let (state, writes) = update(keyboard, BacklightChange::Automatic(true)).await;
        assert!(state?.is_automatic());
        assert_eq!(writes, vec![written(1, AUTOMATIC, 2)]);
        Ok(())
    }

    #[tokio::test]
    async fn battery_saving_flips_only_its_own_option() -> Result<(), WriteError> {
        let wow = BacklightOptions::WOW.bits().to_le_bytes()[0];
        let saving = BacklightOptions::PWR_SAVE.bits().to_le_bytes()[0];
        let keyboard = Keyboard::v3().options_low(BacklightOptions::WOW);
        let (state, writes) = update(keyboard, BacklightChange::PowerSave(true)).await;
        assert!(state?.power_save);
        assert_eq!(writes, vec![written(1, AUTOMATIC | wow | saving, 4)]);

        let keyboard =
            Keyboard::v3().options_low(BacklightOptions::WOW | BacklightOptions::PWR_SAVE);
        let (state, writes) = update(keyboard, BacklightChange::PowerSave(false)).await;
        assert!(!state?.power_save);
        assert_eq!(writes, vec![written(1, AUTOMATIC | wow, 4)]);
        Ok(())
    }

    #[tokio::test]
    async fn a_change_the_keyboard_does_not_offer_writes_nothing() {
        let before_v3 = Keyboard {
            version: 2,
            ..Keyboard::v3()
        };
        let no_saving = Keyboard::v3().capabilities(
            BacklightOptions::AUTO_MODE_SUPPORTED | BacklightOptions::PERM_MANUAL_SUPPORTED,
        );
        let no_sensor = Keyboard::v3().capabilities(BacklightOptions::PERM_MANUAL_SUPPORTED);
        for (keyboard, change) in [
            (before_v3, BacklightChange::Level(2)),
            (Keyboard::v3(), BacklightChange::Level(NB_LEVELS)),
            (no_saving, BacklightChange::PowerSave(true)),
            (no_sensor, BacklightChange::Automatic(true)),
        ] {
            let (state, writes) = update(keyboard, change).await;
            assert!(
                matches!(state, Err(WriteError::BacklightChangeUnsupported { change: c }) if c == change),
                "{change}: {state:?}"
            );
            assert!(writes.is_empty(), "{change}");
        }
    }

    #[tokio::test]
    async fn a_held_level_the_keyboard_did_not_keep_is_reported() {
        let keyboard = Keyboard {
            ignores_level: true,
            ..Keyboard::v3().level(6)
        };
        for change in [BacklightChange::Level(2), BacklightChange::Automatic(false)] {
            let (state, writes) = update(keyboard, change).await;
            assert!(
                matches!(state, Err(WriteError::BacklightNotApplied { change: c }) if c == change),
                "{change}: {state:?}"
            );
            assert_eq!(writes.len(), 1, "{change}");
        }
    }

    #[test]
    fn a_level_needs_a_held_level_version_3_and_a_choice() {
        let manual = BacklightOptions::PERM_MANUAL_SUPPORTED;
        assert!(controls_from(manual, 3, 2).level);
        assert!(!controls_from(manual, 2, 8).level);
        assert!(!controls_from(manual, 3, 1).level);
        assert!(!controls_from(BacklightOptions::AUTO_MODE_SUPPORTED, 3, 8).level);
        let all = controls_from(
            BacklightOptions::AUTO_MODE_SUPPORTED
                | BacklightOptions::PERM_MANUAL_SUPPORTED
                | BacklightOptions::PWR_SAVE_SUPPORTED,
            3,
            8,
        );
        assert_eq!(
            all,
            BacklightControls {
                automatic: true,
                level: true,
                power_save: true,
            }
        );
        // The automatic switch needs both the sensor and a held level.
        assert!(!controls_from(BacklightOptions::AUTO_MODE_SUPPORTED, 3, 8).automatic);
        assert!(!controls_from(manual, 3, 8).automatic);
        assert_eq!(
            controls_from(BacklightOptions::empty(), 3, 8),
            BacklightControls::default()
        );
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
