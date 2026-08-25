//! Decodes the active onboard profile's button bindings into the wire-safe
//! [`OnboardProfileBindings`] DTO — the orchestration and human-readable
//! rendering that sits above the raw `0x8100` reads in
//! `super::diagnostics`. Byte-layout citations live in
//! `hidpp::feature::onboard_profiles`; this file only turns already-decoded
//! [`ButtonBinding`]s into text.

use std::sync::Arc;

use hidpp::channel::HidppChannel;
use hidpp::feature::CreatableFeature;
use hidpp::feature::onboard_profiles::{
    ButtonBinding, OnboardProfilesFeature, SpecialFunction, parse_profile_directory,
};
use openlogi_core::hid::{OnboardProfileBinding, OnboardProfileBindings};

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::diagnostics::open_onboard_profiles;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, with_route};

/// Byte offset where profile sector 1's button-binding table was observed to
/// start on a G502 X LIGHTSPEED and a G502 LIGHTSPEED — an empirical
/// observation, not a cited protocol constant (see the CLI diagnostic and
/// `hidpp::feature::onboard_profiles` module docs for the full caveat). This
/// is the same assumption `openlogi diag onboard-profiles --sector` makes.
const OBSERVED_BUTTON_TABLE_OFFSET: usize = 32;

/// Safety cap on how many button-binding entries to decode, so a wrong
/// offset guess on an unfamiliar device produces a bounded amount of
/// (possibly nonsensical) output instead of walking the whole sector.
const MAX_DECODED_ENTRIES: usize = 32;

/// Reads the device's currently active onboard profile and decodes its
/// button-binding table into human-readable descriptions.
///
/// Returns `active_profile: None` (with no bindings) when the device
/// reports no active profile — onboard mode is off, e.g. the device is
/// currently driven by G Hub instead of running standalone.
pub async fn dump_onboard_profile_bindings(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardProfileBindings, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read_active_profile_bindings(&channel, index).await
    })
    .await
}

/// Same, on an already-open [`SharedChannel`] — the fast path the agent uses
/// so this shares the capture session's channel instead of opening a second
/// one to the same device.
pub async fn read_onboard_profile_bindings_on(
    shared: &SharedChannel,
) -> Result<OnboardProfileBindings, WriteError> {
    read_active_profile_bindings(shared.channel(), shared.device_index()).await
}

async fn read_active_profile_bindings(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<OnboardProfileBindings, WriteError> {
    let onboard_profiles = open_onboard_profiles(channel, index).await?;
    let classify = |e| {
        classify_hidpp_error(
            e,
            HidppOperation::OnboardProfiles,
            OnboardProfilesFeature::ID,
        )
    };

    let info = onboard_profiles.get_info().await.map_err(classify)?;
    let active = onboard_profiles
        .get_current_profile()
        .await
        .map_err(classify)?;
    if active == 0 {
        return Ok(OnboardProfileBindings {
            active_profile: None,
            bindings: Vec::new(),
        });
    }

    let directory = onboard_profiles
        .read_sector(0, info.sector_size)
        .await
        .map_err(classify)?;
    let Some(entry) = parse_profile_directory(&directory)
        .into_iter()
        .find(|e| e.address == u16::from(active))
    else {
        // The active index doesn't match any directory entry — report the
        // index anyway so the caller can see something is active, just not
        // which sector it lives in.
        return Ok(OnboardProfileBindings {
            active_profile: Some(active),
            bindings: Vec::new(),
        });
    };

    let profile = onboard_profiles
        .read_sector(entry.address, info.sector_size)
        .await
        .map_err(classify)?;

    Ok(OnboardProfileBindings {
        active_profile: Some(active),
        bindings: decode_button_bindings(&profile),
    })
}

/// Walks the button-binding table from [`OBSERVED_BUTTON_TABLE_OFFSET`],
/// stopping at the first disabled slot, an unrecognized entry, or
/// [`MAX_DECODED_ENTRIES`] — whichever comes first.
fn decode_button_bindings(profile_sector: &[u8]) -> Vec<OnboardProfileBinding> {
    let Some(table) = profile_sector.get(OBSERVED_BUTTON_TABLE_OFFSET..) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    let (chunks, _) = table.as_chunks::<4>();
    for (slot, &entry) in chunks.iter().take(MAX_DECODED_ENTRIES).enumerate() {
        match ButtonBinding::parse(entry) {
            Ok(ButtonBinding::Disabled) | Err(_) => break,
            #[expect(
                clippy::cast_possible_truncation,
                reason = "slot < MAX_DECODED_ENTRIES (32)"
            )]
            Ok(binding) => bindings.push(OnboardProfileBinding {
                slot: slot as u8,
                description: describe_binding(binding),
            }),
        }
    }
    bindings
}

fn describe_binding(binding: ButtonBinding) -> String {
    match binding {
        ButtonBinding::Mouse { buttons } => {
            if buttons.is_power_of_two() {
                format!("Mouse button {}", buttons.trailing_zeros() + 1)
            } else {
                format!("Mouse buttons (mask {buttons:#06x})")
            }
        }
        ButtonBinding::Keyboard {
            modifier_flags,
            key,
        } => format!("Keyboard: {}", describe_keyboard(modifier_flags, key)),
        ButtonBinding::ConsumerControl { usage } => {
            format!("Consumer control (usage {usage:#06x})")
        }
        ButtonBinding::Special(special) => format!("Special: {}", describe_special(special)),
        ButtonBinding::Macro { page, offset } => format!("Macro (page {page}, offset {offset})"),
        ButtonBinding::Disabled => "Disabled".to_string(),
        // ButtonBinding is #[non_exhaustive] in `hidpp`; a future variant
        // this crate doesn't know about yet still renders instead of
        // failing to compile.
        _ => "Unrecognized binding".to_string(),
    }
}

/// Renders a standard USB HID modifier byte + keyboard usage id. The
/// modifier bit layout and the usage ids below are the USB-IF HID Usage
/// Tables' Boot Keyboard convention — a public standard, not
/// Logitech-specific reverse engineering. Coverage is best-effort: common
/// letters, digits, and navigation/function keys are named, everything else
/// falls back to its raw usage id.
fn describe_keyboard(modifier_flags: u8, key: u8) -> String {
    let mut modifiers = Vec::new();
    for (bit, name) in [
        (0, "LCtrl"),
        (1, "LShift"),
        (2, "LAlt"),
        (3, "LGui"),
        (4, "RCtrl"),
        (5, "RShift"),
        (6, "RAlt"),
        (7, "RGui"),
    ] {
        if modifier_flags & (1 << bit) != 0 {
            modifiers.push(name);
        }
    }

    let key_name = match key {
        0x04..=0x1d => {
            let letter = (b'a' + (key - 0x04)) as char;
            return finish_keyboard_name(&modifiers, &letter.to_string());
        }
        0x1e..=0x26 => {
            let digit = (b'1' + (key - 0x1e)) as char;
            return finish_keyboard_name(&modifiers, &digit.to_string());
        }
        0x27 => "0",
        0x28 => "Enter",
        0x29 => "Escape",
        0x2a => "Backspace",
        0x2b => "Tab",
        0x2c => "Space",
        0x3a..=0x45 => {
            let n = key - 0x39;
            return finish_keyboard_name(&modifiers, &format!("F{n}"));
        }
        0x4f => "Right",
        0x50 => "Left",
        0x51 => "Down",
        0x52 => "Up",
        _ => return finish_keyboard_name(&modifiers, &format!("usage {key:#04x}")),
    };
    finish_keyboard_name(&modifiers, key_name)
}

fn finish_keyboard_name(modifiers: &[&str], key_name: &str) -> String {
    if modifiers.is_empty() {
        key_name.to_string()
    } else {
        format!("{}+{}", modifiers.join("+"), key_name)
    }
}

fn describe_special(special: SpecialFunction) -> &'static str {
    match special {
        SpecialFunction::Noop => "No-op",
        SpecialFunction::TiltLeft => "Wheel tilt left",
        SpecialFunction::TiltRight => "Wheel tilt right",
        SpecialFunction::NextDpi => "Next DPI stage",
        SpecialFunction::PrevDpi => "Previous DPI stage",
        SpecialFunction::CycleDpi => "Cycle DPI stages",
        SpecialFunction::DefaultDpi => "Default DPI",
        SpecialFunction::ShiftDpi => "DPI shift (sniper button)",
        SpecialFunction::NextProfile => "Next profile",
        SpecialFunction::PrevProfile => "Previous profile",
        SpecialFunction::CycleProfile => "Cycle profiles",
        SpecialFunction::GShift => "G-Shift (secondary layer)",
        SpecialFunction::BatteryIndicator => "Battery indicator",
        SpecialFunction::EnableProfile => "Enable profile",
        SpecialFunction::PerformanceSwitch => "Performance switch",
        SpecialFunction::Host => "Switch host",
        SpecialFunction::ScrollDown => "Scroll down",
        SpecialFunction::ScrollUp => "Scroll up",
        // SpecialFunction is #[non_exhaustive] in `hidpp`.
        _ => "Unrecognized special function",
    }
}
