//! The G700's button bindings: the thirteen slots of the profile it plays,
//! read and, when the configuration asks for a change, rewritten.
//!
//! What goes where is `openlogi_core::binding::flash`'s decision; this
//! module reads the profiles that decision needs, converts their bindings to
//! and from [`FlashBinding`], and writes the one page that changes. A write
//! is a page rewrite of a few seconds during which the mouse plays its
//! factory profile, so it must run to its end: the caller runs this outside
//! any request deadline.

use std::collections::BTreeSet;

use hidpp::register::{
    current_profile::{CurrentProfile, read_current_profile},
    onboard_memory::{
        directory::{DIRECTORY_PAGE, DirectoryEntry},
        g700::{
            self, BUTTON_COUNT, Binding, ButtonMask, G700Profile, KeyModifiers, MacroPointer,
            Special,
        },
    },
};
use openlogi_core::binding::{
    FlashBinding, FlashFunction, FlashPlan, G700, SlotIntent, flash::plan_flash_bindings,
};
use tracing::info;

use super::G700Link;
use super::configure::{G700Error, G700Register, read, read_trusted};
use crate::SharedChannel;
use crate::write::WriteError;

/// What a [`sync_g700_bindings_on`] found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G700BindingsSync {
    /// The directory index of the profile the mouse plays; `None` while it
    /// plays its factory profile, which has no page to hold bindings, so
    /// nothing was read or written.
    pub profile: Option<u8>,
    /// Where the played profile's bindings went, as they are on the mouse
    /// now: written when [`Self::written`], else as read.
    pub plan: FlashPlan,
    /// Whether the profile page was rewritten.
    pub written: bool,
}

/// Bring the bindings of the profile the G700 behind `shared` plays in line
/// with `intents`, one per slot of [`G700`], and report where they went.
///
/// The played profile and every other profile the directory lists are
/// read, the latter for the buttons they press, which an agent button must
/// not be; the page is written only if a binding changes. The G700's
/// register phase is held throughout. A write cannot be abandoned midway
/// without leaving the mouse on its factory profile, so the caller must let
/// this run to its end.
///
/// [`WriteError::DeviceNotFound`] (as [`G700Error::Device`]) when `shared`
/// reaches no G700.
pub async fn sync_g700_bindings_on(
    shared: &SharedChannel,
    intents: &[SlotIntent],
) -> Result<G700BindingsSync, G700Error> {
    let link = G700Link::on_shared(shared)
        .await?
        .ok_or(G700Error::Device(WriteError::DeviceNotFound))?;
    sync_on(&link, intents).await
}

async fn sync_on(link: &G700Link, intents: &[SlotIntent]) -> Result<G700BindingsSync, G700Error> {
    let (chan, index) = (link.chan(), link.device_index());
    let register = G700Register::CurrentProfile;
    let current = read_current_profile(chan, index)
        .await
        .map_err(|source| G700Error::RegisterRead { register, source })?;
    let directory = g700::parse_directory(&read(chan, index, DIRECTORY_PAGE).await?);
    let entries = directory.entries();
    let Some(played) = played_entry(current, entries) else {
        return Ok(G700BindingsSync {
            profile: None,
            plan: FlashPlan::default(),
            written: false,
        });
    };
    let entry = entries[played];
    let base = read_trusted(chan, index, entry.page).await?;
    let mut profile = G700Profile::parse(&base);
    let held: Vec<FlashBinding> = profile.bindings.iter().map(|b| flash_binding(*b)).collect();
    let mut reserved = BTreeSet::new();
    for other in entries.iter().filter(|other| other.page != entry.page) {
        let page = read(chan, index, other.page).await?;
        reserved.extend(
            G700Profile::parse(&page)
                .bindings
                .iter()
                .flat_map(|binding| flash_binding(*binding).buttons()),
        );
    }
    let plan = plan_flash_bindings(&G700, &held, &reserved, intents);
    let written = plan.bindings != held;
    if written {
        for (slot, binding) in profile.bindings.iter_mut().zip(&plan.bindings) {
            *slot = wire_binding(*binding);
        }
        info!(
            page = entry.page,
            profile = played,
            "writing the G700's button bindings"
        );
        g700::memory_writer(chan, index)
            .write_page(entry.page, &profile.to_page())
            .await?;
    }
    Ok(G700BindingsSync {
        profile: u8::try_from(played).ok(),
        plan,
        written,
    })
}

/// The directory position of the profile `current` names; `None` for the
/// factory profile or an address the directory does not list.
fn played_entry(current: CurrentProfile, entries: &[DirectoryEntry]) -> Option<usize> {
    match current {
        CurrentProfile::Index(index) => {
            let index = usize::from(index);
            (index < entries.len()).then_some(index)
        }
        CurrentProfile::Address { page, offset } => entries
            .iter()
            .position(|entry| entry.page == page && entry.offset == offset),
        CurrentProfile::Factory => None,
    }
}

const _: () = assert!(BUTTON_COUNT == 13, "the G700 layout has thirteen slots");

/// A profile binding as [`FlashBinding`]. Lossless: [`wire_binding`] turns
/// it back into the binding it came from.
fn flash_binding(binding: Binding) -> FlashBinding {
    match binding {
        Binding::MouseButtons(ButtonMask(mask)) => FlashBinding::MouseButtons(mask),
        Binding::Key { modifiers, usage } => FlashBinding::Key {
            modifiers: modifiers.bits(),
            usage,
        },
        Binding::Special(special) => FlashBinding::Function(match special {
            Special::WheelLeft => FlashFunction::WheelLeft,
            Special::WheelRight => FlashFunction::WheelRight,
            Special::BatteryLevel => FlashFunction::BatteryLevel,
            Special::ResolutionUp => FlashFunction::ResolutionUp,
            Special::ResolutionCycleUp => FlashFunction::ResolutionCycleUp,
            Special::ResolutionDown => FlashFunction::ResolutionDown,
            Special::ResolutionCycleDown => FlashFunction::ResolutionCycleDown,
            Special::ProfileUp => FlashFunction::ProfileUp,
            Special::ProfileCycleUp => FlashFunction::ProfileCycleUp,
            Special::ProfileDown => FlashFunction::ProfileDown,
            Special::ProfileCycleDown => FlashFunction::ProfileCycleDown,
            Special::Unknown(code) => FlashFunction::Other(code),
        }),
        Binding::Consumer(usage) => FlashBinding::Consumer(usage),
        Binding::Disabled => FlashBinding::Disabled,
        Binding::Macro(MacroPointer { page, word_offset }) => {
            FlashBinding::Macro { page, word_offset }
        }
        Binding::Unknown(bytes) => FlashBinding::Raw(bytes),
    }
}

/// The profile binding a [`FlashBinding`] stands for.
fn wire_binding(binding: FlashBinding) -> Binding {
    match binding {
        FlashBinding::MouseButtons(mask) => Binding::MouseButtons(ButtonMask(mask)),
        FlashBinding::Key { modifiers, usage } => Binding::Key {
            modifiers: KeyModifiers::from_bits_retain(modifiers),
            usage,
        },
        FlashBinding::Function(function) => Binding::Special(match function {
            FlashFunction::WheelLeft => Special::WheelLeft,
            FlashFunction::WheelRight => Special::WheelRight,
            FlashFunction::BatteryLevel => Special::BatteryLevel,
            FlashFunction::ResolutionUp => Special::ResolutionUp,
            FlashFunction::ResolutionCycleUp => Special::ResolutionCycleUp,
            FlashFunction::ResolutionDown => Special::ResolutionDown,
            FlashFunction::ResolutionCycleDown => Special::ResolutionCycleDown,
            FlashFunction::ProfileUp => Special::ProfileUp,
            FlashFunction::ProfileCycleUp => Special::ProfileCycleUp,
            FlashFunction::ProfileDown => Special::ProfileDown,
            FlashFunction::ProfileCycleDown => Special::ProfileCycleDown,
            FlashFunction::Other(code) => Special::from(code),
        }),
        FlashBinding::Consumer(usage) => Binding::Consumer(usage),
        FlashBinding::Disabled => Binding::Disabled,
        FlashBinding::Macro { page, word_offset } => {
            Binding::Macro(MacroPointer { page, word_offset })
        }
        FlashBinding::Raw(bytes) => Binding::Unknown(bytes),
    }
}

#[cfg(test)]
mod tests;
