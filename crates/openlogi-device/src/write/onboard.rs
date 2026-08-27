//! HID++ `0x8100` onboard-profile reads and button-table writes.

use std::collections::BTreeMap;
use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        onboard_profiles::{
            self, OnboardProfilesFeature, decode_active_profile_index, parse_profile_directory,
            patch_g402_button, profile_index_is_one_based, read_g402_button, special,
        },
    },
    protocol::v20::Hidpp20Error,
};

pub use hidpp::feature::onboard_profiles::{
    ButtonBinding, OnboardMode, ProfileDirectoryEntry, ProfilesDescription,
};
use openlogi_core::binding::{Action, ButtonId, KeyCombo};
use tracing::{debug, warn};

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

const FEATURE: u16 = OnboardProfilesFeature::ID;

/// Snapshot of onboard profiles for `openlogi diag profiles`.
#[derive(Debug, Clone)]
pub struct OnboardProfilesDump {
    /// Flash geometry from `getProfilesDescription`.
    pub description: ProfilesDescription,
    /// Current onboard/host mode.
    pub mode: OnboardMode,
    /// 0-based active profile after applying any 1-based quirk.
    pub active_profile: u8,
    /// Raw firmware-reported profile index.
    pub active_profile_raw: u8,
    /// User-profile directory (sector 0).
    pub directory: Vec<ProfileDirectoryEntry>,
    /// Decoded button table of the active profile, when readable.
    pub active_buttons: Vec<OnboardButtonSlot>,
}

/// One physical button slot in the active onboard profile.
#[derive(Debug, Clone, Copy)]
pub struct OnboardButtonSlot {
    /// 0-based index in the G402 `buttons[]` array (G1 = 0).
    pub index: u8,
    /// OpenLogi button this slot maps to, when known.
    pub button: Option<ButtonId>,
    /// Raw 4-byte binding.
    pub binding: ButtonBinding,
}

fn classify(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::OnboardProfiles, FEATURE)
}

/// Dump onboard-profile geometry, mode, and the active profile's buttons.
pub async fn dump_onboard_profiles(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardProfilesDump, WriteError> {
    let index = route.device_index();
    let product_id = route_product_id(route);
    with_route(backend, route, move |channel| async move {
        dump_onboard_profiles_on_channel(&channel, index, product_id).await
    })
    .await
}

/// Dump onboard profiles on an already-open channel.
pub async fn dump_onboard_profiles_on(
    shared: &SharedChannel,
) -> Result<OnboardProfilesDump, WriteError> {
    dump_onboard_profiles_on_channel(
        shared.channel(),
        shared.device_index(),
        route_product_id(shared.route()),
    )
    .await
}

async fn dump_onboard_profiles_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    product_id: Option<u16>,
) -> Result<OnboardProfilesDump, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let description = feature.get_profiles_description().await.map_err(classify)?;
    let mode = feature.get_onboard_mode().await.map_err(classify)?;
    let active_profile_raw = feature.get_current_profile_raw().await.map_err(classify)?;
    let one_based = product_id.is_some_and(profile_index_is_one_based);
    let active_profile =
        decode_active_profile_index(active_profile_raw, description.profile_count, one_based);

    let directory = if description.sector_size >= 16 {
        match feature
            .read_sector(
                onboard_profiles::USER_PROFILE_DIRECTORY,
                description.sector_size,
            )
            .await
        {
            Ok(data) => parse_profile_directory(&data, description.profile_count),
            Err(error) => {
                warn!(error = ?error, "onboard profile directory read failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let active_buttons = match directory.get(usize::from(active_profile)) {
        Some(entry) if description.has_g402_button_table() => {
            match feature
                .read_sector(entry.address, description.sector_size)
                .await
            {
                Ok(sector) => decode_active_buttons(&sector, description.button_count),
                Err(error) => {
                    warn!(error = ?error, "active onboard profile read failed");
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };

    Ok(OnboardProfilesDump {
        description,
        mode,
        active_profile,
        active_profile_raw,
        directory,
        active_buttons,
    })
}

fn decode_active_buttons(sector: &[u8], button_count: u8) -> Vec<OnboardButtonSlot> {
    (0..button_count)
        .filter_map(|index| {
            let binding = read_g402_button(sector, usize::from(index))?;
            Some(OnboardButtonSlot {
                index,
                button: g_series_button(index),
                binding,
            })
        })
        .collect()
}

/// Write `bindings` into the active G402-format onboard profile.
///
/// Forces onboard mode (firmware ignores profile writes in host mode), patches
/// every G-series slot we can encode, and rewrites the sector with a fresh CRC.
pub async fn apply_onboard_button_bindings(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    bindings: &BTreeMap<ButtonId, Action>,
) -> Result<(), WriteError> {
    let index = route.device_index();
    let product_id = route_product_id(route);
    let bindings = bindings.clone();
    with_route(backend, route, move |channel| async move {
        apply_onboard_button_bindings_on_channel(&channel, index, product_id, &bindings).await
    })
    .await
}

/// Write onboard button bindings on an already-open channel.
pub async fn apply_onboard_button_bindings_on(
    shared: &SharedChannel,
    bindings: &BTreeMap<ButtonId, Action>,
) -> Result<(), WriteError> {
    apply_onboard_button_bindings_on_channel(
        shared.channel(),
        shared.device_index(),
        route_product_id(shared.route()),
        bindings,
    )
    .await
}

async fn apply_onboard_button_bindings_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    product_id: Option<u16>,
    bindings: &BTreeMap<ButtonId, Action>,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let description = feature.get_profiles_description().await.map_err(classify)?;
    if !description.is_g402_memory() || !description.has_g402_button_table() {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::OnboardProfiles,
            feature_hex: FEATURE,
        });
    }

    let mode = feature.get_onboard_mode().await.map_err(classify)?;
    if mode != OnboardMode::Onboard {
        feature
            .set_onboard_mode(OnboardMode::Onboard)
            .await
            .map_err(classify)?;
    }

    let directory = feature
        .read_sector(
            onboard_profiles::USER_PROFILE_DIRECTORY,
            description.sector_size,
        )
        .await
        .map_err(classify)?;
    let entries = parse_profile_directory(&directory, description.profile_count);
    let raw = feature.get_current_profile_raw().await.map_err(classify)?;
    let active = decode_active_profile_index(
        raw,
        description.profile_count,
        product_id.is_some_and(profile_index_is_one_based),
    );
    let Some(entry) = entries.get(usize::from(active)) else {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::OnboardProfiles,
            feature_hex: FEATURE,
        });
    };

    let mut sector = feature
        .read_sector(entry.address, description.sector_size)
        .await
        .map_err(classify)?;
    let slots = usize::from(description.button_count.min(16));
    for (button, action) in bindings {
        let Some(slot) = g_series_slot(*button) else {
            continue;
        };
        if slot >= slots {
            continue;
        }
        let encoded = encode_onboard_binding(*button, action);
        if !patch_g402_button(&mut sector, slot, encoded) {
            warn!(slot, "onboard button slot out of range — skipped");
        }
    }
    feature
        .write_sector(entry.address, sector, true)
        .await
        .map_err(classify)?;
    debug!(
        index,
        profile = active,
        "wrote onboard profile button table"
    );
    Ok(())
}

fn g_series_slot(button: ButtonId) -> Option<usize> {
    match button {
        ButtonId::LeftClick => Some(0),
        ButtonId::RightClick => Some(1),
        ButtonId::MiddleClick => Some(2),
        ButtonId::Back => Some(3),
        ButtonId::Forward => Some(4),
        ButtonId::DpiToggle => Some(5),
        ButtonId::DpiUp => Some(6),
        ButtonId::DpiDown => Some(7),
        ButtonId::ProfileCycle => Some(8),
        ButtonId::WheelTiltLeft => Some(9),
        ButtonId::WheelTiltRight => Some(10),
        _ => None,
    }
}

fn g_series_button(slot: u8) -> Option<ButtonId> {
    match slot {
        0 => Some(ButtonId::LeftClick),
        1 => Some(ButtonId::RightClick),
        2 => Some(ButtonId::MiddleClick),
        3 => Some(ButtonId::Back),
        4 => Some(ButtonId::Forward),
        5 => Some(ButtonId::DpiToggle),
        6 => Some(ButtonId::DpiUp),
        7 => Some(ButtonId::DpiDown),
        8 => Some(ButtonId::ProfileCycle),
        9 => Some(ButtonId::WheelTiltLeft),
        10 => Some(ButtonId::WheelTiltRight),
        _ => None,
    }
}

fn encode_onboard_binding(button: ButtonId, action: &Action) -> ButtonBinding {
    encode_action(action).unwrap_or_else(|| native_binding(button))
}

/// Map the active onboard profile's decoded slots to OpenLogi actions.
fn actions_from_dump(dump: &OnboardProfilesDump) -> BTreeMap<ButtonId, Action> {
    dump.active_buttons
        .iter()
        .filter_map(|slot| {
            let button = slot.button?;
            let action = decode_action(slot.binding)?;
            Some((button, action))
        })
        .collect()
}

/// Read the active onboard profile's buttons as OpenLogi actions.
pub async fn read_onboard_button_bindings(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<BTreeMap<ButtonId, Action>, WriteError> {
    let dump = dump_onboard_profiles(backend, route).await?;
    Ok(actions_from_dump(&dump))
}

/// Read onboard button bindings on an already-open channel.
pub async fn read_onboard_button_bindings_on(
    shared: &SharedChannel,
) -> Result<BTreeMap<ButtonId, Action>, WriteError> {
    let dump = dump_onboard_profiles_on(shared).await?;
    Ok(actions_from_dump(&dump))
}

fn decode_action(binding: ButtonBinding) -> Option<Action> {
    for action in Action::catalog() {
        if encode_action(&action) == Some(binding) {
            return Some(action);
        }
    }
    let [0x80, 0x02, modifiers, usage] = binding.bytes else {
        return None;
    };
    KeyCombo::from_hid_report(modifiers, usage).map(Action::CustomShortcut)
}

fn encode_action(action: &Action) -> Option<ButtonBinding> {
    Some(match action {
        Action::None => ButtonBinding::disabled(),
        Action::LeftClick => ButtonBinding::hid_mouse(1),
        Action::RightClick => ButtonBinding::hid_mouse(2),
        Action::MiddleClick => ButtonBinding::hid_mouse(3),
        Action::MouseBack => ButtonBinding::hid_mouse(4),
        Action::MouseForward => ButtonBinding::hid_mouse(5),
        Action::CycleDpiPresets => ButtonBinding::special(special::CYCLE_DPI),
        Action::NextDpiPreset => ButtonBinding::special(special::NEXT_DPI),
        Action::PrevDpiPreset => ButtonBinding::special(special::PREV_DPI),
        Action::CycleOnboardProfile => ButtonBinding::special(special::CYCLE_PROFILE),
        Action::HorizontalScrollLeft => ButtonBinding::special(special::TILT_LEFT),
        Action::HorizontalScrollRight => ButtonBinding::special(special::TILT_RIGHT),
        Action::ScrollUp => ButtonBinding::special(special::SCROLL_UP),
        Action::ScrollDown => ButtonBinding::special(special::SCROLL_DOWN),
        Action::BrowserBack => ButtonBinding::hid_consumer(0x0224),
        Action::BrowserForward => ButtonBinding::hid_consumer(0x0225),
        Action::PlayPause => ButtonBinding::hid_consumer(0x00cd),
        Action::NextTrack => ButtonBinding::hid_consumer(0x00b5),
        Action::PrevTrack => ButtonBinding::hid_consumer(0x00b6),
        Action::VolumeUp => ButtonBinding::hid_consumer(0x00e9),
        Action::VolumeDown => ButtonBinding::hid_consumer(0x00ea),
        Action::MuteVolume => ButtonBinding::hid_consumer(0x00e2),
        Action::Copy => hid_shortcut(primary_modifier(), 0x06),
        Action::Paste => hid_shortcut(primary_modifier(), 0x19),
        Action::Cut => hid_shortcut(primary_modifier(), 0x1b),
        Action::Undo => hid_shortcut(primary_modifier(), 0x1d),
        Action::Redo => hid_shortcut(primary_modifier() | 0x02, 0x1d),
        Action::SelectAll => hid_shortcut(primary_modifier(), 0x04),
        Action::Find => hid_shortcut(primary_modifier(), 0x09),
        Action::Save => hid_shortcut(primary_modifier(), 0x16),
        Action::CustomShortcut(combo) | Action::HoldShortcut(combo) => hid_combo(combo),
        _ => return None,
    })
}

fn hid_shortcut(modifiers: u8, usage: u8) -> ButtonBinding {
    ButtonBinding::hid_keyboard(modifiers, usage)
}

/// Left Control on Windows/Linux, Left GUI (Command) on macOS — the chord the
/// OS treats as Copy/Paste/etc. Onboard HID keyboard reports are interpreted
/// by the host, so the modifier must match the platform that reads them.
const fn primary_modifier() -> u8 {
    if cfg!(target_os = "macos") {
        0x08
    } else {
        0x01
    }
}

fn hid_combo(combo: &KeyCombo) -> ButtonBinding {
    let mut modifiers = 0u8;
    if combo.has_control() {
        modifiers |= 0x01;
    }
    if combo.has_shift() {
        modifiers |= 0x02;
    }
    if combo.has_option() {
        modifiers |= 0x04;
    }
    if combo.has_command() {
        modifiers |= 0x08;
    }
    ButtonBinding::hid_keyboard(modifiers, combo.key().code())
}

fn native_binding(button: ButtonId) -> ButtonBinding {
    match button {
        ButtonId::LeftClick => ButtonBinding::hid_mouse(1),
        ButtonId::RightClick => ButtonBinding::hid_mouse(2),
        ButtonId::MiddleClick => ButtonBinding::hid_mouse(3),
        ButtonId::Back => ButtonBinding::hid_mouse(4),
        ButtonId::Forward => ButtonBinding::hid_mouse(5),
        ButtonId::DpiToggle => ButtonBinding::special(special::SHIFT_DPI),
        ButtonId::DpiUp => ButtonBinding::special(special::NEXT_DPI),
        ButtonId::DpiDown => ButtonBinding::special(special::PREV_DPI),
        ButtonId::ProfileCycle => ButtonBinding::special(special::CYCLE_PROFILE),
        ButtonId::WheelTiltLeft => ButtonBinding::special(special::TILT_LEFT),
        ButtonId::WheelTiltRight => ButtonBinding::special(special::TILT_RIGHT),
        _ => ButtonBinding::disabled(),
    }
}

fn route_product_id(route: &DeviceRoute) -> Option<u16> {
    match route {
        DeviceRoute::Direct { product_id, .. } | DeviceRoute::RawHid { product_id, .. } => {
            Some(*product_id)
        }
        DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_action, encode_action, g_series_slot, native_binding};
    use hidpp::feature::onboard_profiles::{ButtonBinding, special};
    use openlogi_core::binding::{Action, ButtonId, KeyCombo};

    #[test]
    fn g_series_slots_match_g_key_numbers() {
        assert_eq!(g_series_slot(ButtonId::LeftClick), Some(0));
        assert_eq!(g_series_slot(ButtonId::DpiUp), Some(6));
        assert_eq!(g_series_slot(ButtonId::WheelTiltRight), Some(10));
        assert_eq!(g_series_slot(ButtonId::GestureButton), None);
    }

    #[test]
    fn encodes_dpi_and_mouse_actions() {
        assert_eq!(
            encode_action(&Action::NextDpiPreset).unwrap(),
            ButtonBinding::special(special::NEXT_DPI)
        );
        assert_eq!(
            encode_action(&Action::LeftClick).unwrap(),
            ButtonBinding::hid_mouse(1)
        );
        assert_eq!(
            encode_action(&Action::Copy).unwrap(),
            ButtonBinding::hid_keyboard(super::primary_modifier(), 0x06)
        );
    }

    #[test]
    fn host_only_actions_fall_back_to_native_firmware() {
        assert_eq!(
            native_binding(ButtonId::DpiToggle),
            ButtonBinding::special(special::SHIFT_DPI)
        );
        assert!(encode_action(&Action::MissionControl).is_none());
    }

    #[test]
    fn encoded_catalog_actions_round_trip() {
        for action in Action::catalog() {
            let Some(encoded) = encode_action(&action) else {
                continue;
            };
            assert_eq!(
                decode_action(encoded),
                Some(action.clone()),
                "round-trip failed for {action:?}"
            );
        }
    }

    #[test]
    fn hid_keyboard_chords_decode_as_custom_shortcuts() {
        let combo: KeyCombo = "Alt+F4".parse().expect("valid shortcut");
        let encoded = encode_action(&Action::CustomShortcut(combo.clone())).expect("encodes");
        assert_eq!(decode_action(encoded), Some(Action::CustomShortcut(combo)));
        assert_eq!(
            decode_action(ButtonBinding::special(special::SHIFT_DPI)),
            None,
            "sniper/DPI-shift has no Action yet and must not become a default"
        );
    }
}
