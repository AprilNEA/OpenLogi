//! Button bindings a mouse keeps in its onboard flash.
//!
//! A mouse such as the Logitech G700 stores what each of its buttons does in
//! every onboard profile: press mouse buttons, a key chord or a consumer
//! usage, run a firmware function, nothing, or a macro. Those bindings work
//! with no OpenLogi running, so an action the mouse can express is written
//! there ([`FlashBinding::of_action`]). Any other action gets a mouse button
//! of [`AGENT_BUTTONS`] that nothing else on the profile presses: the mouse
//! reports it, and the agent's OS hook catches it on that mouse and runs the
//! action ([`plan_flash_bindings`]).
//!
//! This module is the one owner of both decisions. The device layer converts
//! [`FlashBinding`] to and from the profile's bytes and nothing more; the
//! agent asks [`flash_intents`] what the configuration wants of each slot and
//! the planner where it goes; the settings app asks [`FlashBinding::of_action`]
//! whether an action is stored on the mouse.

use std::collections::{BTreeMap, BTreeSet};

use nutype::nutype;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::action::Action;
use super::button::ButtonId;
use super::gaming::{GamingLayout, Slot};
use super::key_combo::{KeyCombo, KeyboardUsage, ModifierHost};
use super::value::Binding;
use crate::config::Config;

/// A mouse button as the HID report numbers it, from 1 (the primary button)
/// to 16, the last a 16-bit button field carries.
#[nutype(
    const_fn,
    validate(with = validate_button_number, error = MouseButtonNumberError),
    derive(
        Clone,
        Copy,
        Debug,
        Display,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Hash,
        TryFrom,
        Into,
        Serialize,
        Deserialize
    ),
)]
pub struct MouseButtonNumber(u8);

/// A mouse button number outside `1..=16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("mouse button {0} is not between 1 and 16")]
pub struct MouseButtonNumberError(pub u8);

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "nutype custom validators receive a reference to the wrapped value"
)]
const fn validate_button_number(value: &u8) -> Result<(), MouseButtonNumberError> {
    if matches!(value, 1..=16) {
        Ok(())
    } else {
        Err(MouseButtonNumberError(*value))
    }
}

impl MouseButtonNumber {
    /// The number, `1..=16`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.into_inner()
    }

    /// The bit of this button in a 16-bit button mask.
    #[must_use]
    pub const fn mask(self) -> u16 {
        1 << (self.into_inner() - 1)
    }

    /// Every button a 16-bit mask presses, ascending.
    pub fn in_mask(mask: u16) -> impl Iterator<Item = Self> {
        (1..=16u8)
            .filter(move |n| mask & (1 << (n - 1)) != 0)
            .filter_map(|n| Self::try_new(n).ok())
    }
}

/// The mouse buttons the agent gives a flash slot whose action the mouse
/// cannot run: past the eight buttons operating systems name (left, right,
/// middle, back, forward and the three after), so no application treats
/// them as anything, yet inside the report's sixteen. Taken from the top
/// down, as tools that set the extra buttons by hand tend to start from 9.
pub const AGENT_BUTTONS: std::ops::RangeInclusive<u8> = 9..=16;

/// Whether `button` is one [`AGENT_BUTTONS`] holds.
#[must_use]
pub fn is_agent_button(button: MouseButtonNumber) -> bool {
    AGENT_BUTTONS.contains(&button.get())
}

/// Whether OS input from `vendor_id:product_id` comes from a mouse whose
/// onboard flash holds its bindings, so the hook leaves its buttons as the
/// flash makes them and catches only the [`AGENT_BUTTONS`] the agent gave
/// its slots. Which ids those are is the device registry's decision.
#[must_use]
pub fn is_flash_bindings_input(vendor_id: u16, product_id: u16) -> bool {
    openlogi_device_registry::g700::is_g700_input(vendor_id, product_id)
}

/// A firmware function a flash binding can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlashFunction {
    /// Scroll left, as the wheel tilts.
    WheelLeft,
    /// Scroll right, as the wheel tilts.
    WheelRight,
    /// Show the battery level on the DPI LEDs.
    BatteryLevel,
    /// Next DPI mode, stopping at the last.
    ResolutionUp,
    /// Next DPI mode, wrapping around.
    ResolutionCycleUp,
    /// Previous DPI mode, stopping at the first.
    ResolutionDown,
    /// Previous DPI mode, wrapping around.
    ResolutionCycleDown,
    /// Next profile, stopping at the last.
    ProfileUp,
    /// Next profile, wrapping around.
    ProfileCycleUp,
    /// Previous profile, stopping at the first.
    ProfileDown,
    /// Previous profile, wrapping around.
    ProfileCycleDown,
    /// A function code nothing names, kept as read.
    Other(u16),
}

/// What one button of a flash profile does on its own, without OpenLogi.
///
/// Mirrors the device's binding codec closely enough to round-trip every
/// binding it reads: a binding this crate cannot name is [`Self::Raw`] and
/// goes back to the mouse as it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlashBinding {
    /// Press these mouse buttons: button `n` is bit `n - 1`.
    MouseButtons(u16),
    /// Press a key, a HID keyboard usage, with the HID modifier byte held.
    Key {
        /// The HID keyboard modifier byte.
        modifiers: u8,
        /// The HID keyboard usage (page `0x07`).
        usage: u8,
    },
    /// Press a HID consumer-control usage (page `0x0C`).
    Consumer(u16),
    /// Run a firmware function.
    Function(FlashFunction),
    /// Do nothing.
    Disabled,
    /// Run the macro stored at this address of onboard memory.
    Macro {
        /// The memory page.
        page: u8,
        /// The offset within the page, in 16-bit words.
        word_offset: u8,
    },
    /// Bytes nothing names, kept as read.
    Raw([u8; 3]),
}

/// The consumer usages of the media actions.
const MEDIA_USAGES: [(Action, u16); 6] = [
    (Action::PlayPause, 0x00cd),
    (Action::NextTrack, 0x00b5),
    (Action::PrevTrack, 0x00b6),
    (Action::VolumeUp, 0x00e9),
    (Action::VolumeDown, 0x00ea),
    (Action::MuteVolume, 0x00e2),
];

/// The firmware functions that are actions.
const FUNCTION_ACTIONS: [(Action, FlashFunction); 9] = [
    (Action::HorizontalScrollLeft, FlashFunction::WheelLeft),
    (Action::HorizontalScrollRight, FlashFunction::WheelRight),
    (Action::ShowBatteryLevel, FlashFunction::BatteryLevel),
    (Action::NextDpiPreset, FlashFunction::ResolutionUp),
    (Action::CycleDpiPresets, FlashFunction::ResolutionCycleUp),
    (Action::PreviousDpiPreset, FlashFunction::ResolutionDown),
    (Action::NextOnboardProfile, FlashFunction::ProfileUp),
    (Action::CycleOnboardProfiles, FlashFunction::ProfileCycleUp),
    (Action::PreviousOnboardProfile, FlashFunction::ProfileDown),
];

/// The actions of mouse buttons 1 to 5.
const BUTTON_ACTIONS: [Action; 5] = [
    Action::LeftClick,
    Action::RightClick,
    Action::MiddleClick,
    Action::MouseBack,
    Action::MouseForward,
];

impl FlashBinding {
    /// The binding that makes the mouse itself run `action`, written for
    /// `host`'s modifier keys; `None` when the mouse cannot run it and the
    /// agent must ([`SlotIntent::Agent`]).
    ///
    /// Of the keyboard actions only a recorded chord
    /// ([`Action::CustomShortcut`]) is stored: the named editing and browser
    /// shortcuts are a different chord per host, which the agent presses
    /// right where the mouse would press one chord everywhere.
    #[must_use]
    pub fn of_action(action: &Action, host: ModifierHost) -> Option<Self> {
        if let Some(index) = BUTTON_ACTIONS.iter().position(|a| a == action) {
            return Some(Self::MouseButtons(1 << index));
        }
        if let Some((_, usage)) = MEDIA_USAGES.iter().find(|(a, _)| a == action) {
            return Some(Self::Consumer(*usage));
        }
        if let Some((_, function)) = FUNCTION_ACTIONS.iter().find(|(a, _)| a == action) {
            return Some(Self::Function(*function));
        }
        match action {
            Action::MouseButton(number) => Some(Self::MouseButtons(number.mask())),
            Action::CustomShortcut(combo) => Some(Self::Key {
                modifiers: combo.hid_modifiers(host),
                usage: combo.key().code(),
            }),
            Action::None => Some(Self::Disabled),
            _ => None,
        }
    }

    /// The action this binding runs, as `host` reads its modifier keys;
    /// `None` for one no action stands for: several buttons at once, a key
    /// a shortcut cannot hold, a consumer usage or function without an
    /// action, a macro, or bytes nothing names. [`Self::of_action`] of the
    /// answer writes this binding back, up to the spelling of a chord's
    /// primary modifier (see [`KeyCombo::from_hid`]).
    #[must_use]
    pub fn action(&self, host: ModifierHost) -> Option<Action> {
        match *self {
            Self::MouseButtons(mask) => {
                let mut pressed = MouseButtonNumber::in_mask(mask);
                let (Some(button), None) = (pressed.next(), pressed.next()) else {
                    return None;
                };
                Some(
                    BUTTON_ACTIONS
                        .get(usize::from(button.get() - 1))
                        .cloned()
                        .unwrap_or(Action::MouseButton(button)),
                )
            }
            Self::Key { modifiers, usage } => KeyboardUsage::try_from(usage)
                .ok()
                .map(|key| Action::CustomShortcut(KeyCombo::from_hid(modifiers, key, host))),
            Self::Consumer(usage) => MEDIA_USAGES
                .iter()
                .find(|(_, u)| *u == usage)
                .map(|(action, _)| action.clone()),
            Self::Function(function) => FUNCTION_ACTIONS
                .iter()
                .find(|(_, f)| *f == function)
                .map(|(action, _)| action.clone()),
            Self::Disabled => Some(Action::None),
            Self::Macro { .. } | Self::Raw(_) => None,
        }
    }

    /// The single mouse button this binding presses, if it presses exactly
    /// one.
    #[must_use]
    pub fn single_button(&self) -> Option<MouseButtonNumber> {
        let Self::MouseButtons(mask) = *self else {
            return None;
        };
        let mut pressed = MouseButtonNumber::in_mask(mask);
        match (pressed.next(), pressed.next()) {
            (Some(button), None) => Some(button),
            _ => None,
        }
    }

    /// The mouse buttons this binding presses.
    pub fn buttons(&self) -> impl Iterator<Item = MouseButtonNumber> + use<> {
        let mask = match *self {
            Self::MouseButtons(mask) => mask,
            _ => 0,
        };
        MouseButtonNumber::in_mask(mask)
    }
}

/// The firmware functions only a mouse's flash runs. The host has nothing to
/// run for them, so only a flash-bindings mouse is offered them, and a slot
/// bound to one stays on the mouse even where an app profile overrides it.
pub const MOUSE_ONLY_FUNCTIONS: [Action; 4] = [
    Action::ShowBatteryLevel,
    Action::NextOnboardProfile,
    Action::PreviousOnboardProfile,
    Action::CycleOnboardProfiles,
];

/// Whether the host can run `action` at all: neither a
/// [`MOUSE_ONLY_FUNCTIONS`] function nor a press of a numbered mouse button,
/// which only the flash makes.
fn host_runs(action: &Action) -> bool {
    !(MOUSE_ONLY_FUNCTIONS.contains(action) || matches!(action, Action::MouseButton(_)))
}

/// Whether a host's OS input hook sees the [`AGENT_BUTTONS`], so the agent
/// can run what a slot routed to one asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentButtonHook {
    /// It sees them: evdev on Linux reports `BTN_` codes up to button 16,
    /// a macOS event tap numbers every other-mouse button.
    Sees,
    /// It does not: the Windows low-level mouse hook reports the two X
    /// buttons and nothing past them.
    Blind,
}

impl AgentButtonHook {
    /// The hook this build runs.
    pub const CURRENT: Self = if cfg!(target_os = "windows") {
        Self::Blind
    } else {
        Self::Sees
    };
}

/// What [`flash_intents`] needs to know of the host: how it reads modifier
/// keys, and whether its hook sees the agent buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlashHost {
    /// How chords are written for the host.
    pub modifiers: ModifierHost,
    /// Whether its hook sees the agent buttons.
    pub hook: AgentButtonHook,
}

impl FlashHost {
    /// The host this build runs on.
    pub const CURRENT: Self = Self {
        modifiers: ModifierHost::CURRENT,
        hook: AgentButtonHook::CURRENT,
    };
}

/// Why a slot whose action only the agent can run is not given to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusedSlot {
    /// The slot is a primary button. Handed to the agent it would press an
    /// agent button, and the mouse would have no left or right click while
    /// no agent runs, so it plays its layout default instead.
    PrimaryButton,
    /// This host's hook cannot see agent buttons ([`AgentButtonHook::Blind`]),
    /// so nothing would run the action; the slot keeps what the mouse holds.
    HookBlind,
}

/// What the configuration wants of one flash slot.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SlotIntent {
    /// Nothing: the slot keeps whatever the mouse holds, macro or not.
    Keep,
    /// The mouse runs the slot's action itself.
    Native(FlashBinding),
    /// The agent runs it: the slot presses one of the [`AGENT_BUTTONS`].
    Agent,
    /// The agent would run it but may not ([`RefusedSlot`]).
    Refused(RefusedSlot),
}

/// The slots the agent may never take: without it the mouse must still
/// click.
const PRIMARY_SLOTS: [ButtonId; 2] = [ButtonId::LeftClick, ButtonId::RightClick];

/// What `config` wants of each slot of `layout` on the device `key`, in slot
/// order, for `host`: chords written for its modifier keys, and no slot
/// handed to an agent its hook cannot see ([`RefusedSlot::HookBlind`]).
///
/// A slot with no stored binding is left as the mouse holds it. A stored
/// single action the mouse can run goes to the flash, unless an app profile
/// overrides the slot and the host can run the action too — the slot must
/// then press an agent button, so the agent can run either per app. Any
/// other binding (an action only the host runs, a long press, a gesture) is
/// the agent's. An app override of a slot with no stored binding, or of one
/// bound to a function only the mouse runs, has nothing to run in the
/// other apps and is not applied. A primary button is never handed to the
/// agent ([`RefusedSlot::PrimaryButton`]).
#[must_use]
pub fn flash_intents(
    config: &Config,
    key: &str,
    layout: &GamingLayout,
    flash_host: FlashHost,
) -> Vec<SlotIntent> {
    let host = flash_host.modifiers;
    let device = config.devices.get(key);
    let app_overridden = |button: ButtonId| {
        device.is_some_and(|device| {
            device
                .per_app_bindings
                .values()
                .any(|bindings| bindings.contains_key(&button))
        })
    };
    let agent_for = |button: ButtonId| {
        if PRIMARY_SLOTS.contains(&button) {
            SlotIntent::Refused(RefusedSlot::PrimaryButton)
        } else if flash_host.hook == AgentButtonHook::Blind {
            SlotIntent::Refused(RefusedSlot::HookBlind)
        } else {
            SlotIntent::Agent
        }
    };
    layout
        .slots
        .iter()
        .map(
            |&Slot { button, .. }| match device.and_then(|device| device.bindings.get(&button)) {
                None => SlotIntent::Keep,
                Some(Binding::Single(action)) => match FlashBinding::of_action(action, host) {
                    Some(native) if app_overridden(button) && host_runs(action) => {
                        // A primary button the agent may not take keeps its
                        // native action everywhere rather than losing it.
                        match agent_for(button) {
                            SlotIntent::Refused(RefusedSlot::PrimaryButton) => {
                                SlotIntent::Native(native)
                            }
                            intent => intent,
                        }
                    }
                    Some(native) => SlotIntent::Native(native),
                    None => agent_for(button),
                },
                Some(_) => agent_for(button),
            },
        )
        .collect()
}

/// `intents` with every slot that pressed an agent button on the last write
/// (`routed`) but now asks nothing ([`SlotIntent::Keep`]) sent back to
/// `layout`'s default: kept as it is, the slot would press an agent button
/// the hook no longer catches, and do nothing. Only the last write's routing
/// tells such a slot from one whose profile presses that button on purpose,
/// as a profile made by another tool can.
#[must_use]
pub fn release_cleared_slots(
    layout: &GamingLayout,
    intents: &[SlotIntent],
    routed: &BTreeMap<MouseButtonNumber, ButtonId>,
) -> Vec<SlotIntent> {
    layout
        .slots
        .iter()
        .zip(intents)
        .enumerate()
        .map(|(slot, (button, intent))| {
            let cleared =
                *intent == SlotIntent::Keep && routed.values().any(|b| *b == button.button);
            match layout.flash_defaults.get(slot) {
                Some(default) if cleared => SlotIntent::Native(*default),
                _ => intent.clone(),
            }
        })
        .collect()
}

/// Where a profile's bindings go: what [`plan_flash_bindings`] decided.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FlashPlan {
    /// Every slot's binding as the profile should hold it, in slot order.
    pub bindings: Vec<FlashBinding>,
    /// The agent button each agent-run slot presses, and the slot's button.
    pub routed: BTreeMap<MouseButtonNumber, ButtonId>,
    /// Agent-run slots no free agent button was left for. They keep what
    /// the mouse holds.
    pub unrouted: Vec<ButtonId>,
}

/// Lay out the bindings of one profile of a mouse with `layout`, which
/// holds `current` now, so that it does what `intents` ask.
///
/// A slot the agent runs presses an [`AGENT_BUTTONS`] button nothing else
/// presses: no other slot of the profile, and none of `reserved`, the
/// buttons the mouse's other profiles press, so the hook never takes a
/// button some profile means natively. A slot that already presses a free
/// agent button keeps it, so a write moves no assignment that need not
/// move; the rest take the highest free one. Slots past the shorter of
/// `current` and `intents` are left as they are.
#[must_use]
pub fn plan_flash_bindings(
    layout: &GamingLayout,
    current: &[FlashBinding],
    reserved: &BTreeSet<MouseButtonNumber>,
    intents: &[SlotIntent],
) -> FlashPlan {
    let mut bindings = current.to_vec();
    for (slot, (binding, intent)) in bindings.iter_mut().zip(intents).enumerate() {
        match intent {
            SlotIntent::Native(native) => *binding = *native,
            SlotIntent::Refused(RefusedSlot::PrimaryButton) => {
                if let Some(default) = layout.flash_defaults.get(slot) {
                    *binding = *default;
                }
            }
            SlotIntent::Keep | SlotIntent::Agent | SlotIntent::Refused(RefusedSlot::HookBlind) => {}
        }
    }
    let agent_slots: Vec<usize> = intents
        .iter()
        .take(current.len())
        .enumerate()
        .filter_map(|(slot, intent)| (*intent == SlotIntent::Agent).then_some(slot))
        .collect();
    let mut taken: BTreeSet<MouseButtonNumber> = reserved.clone();
    taken.extend(
        bindings
            .iter()
            .enumerate()
            .filter(|(slot, _)| !agent_slots.contains(slot))
            .flat_map(|(_, binding)| binding.buttons()),
    );
    let mut routed = BTreeMap::new();
    let mut waiting = Vec::new();
    for &slot in &agent_slots {
        match current[slot].single_button() {
            Some(button) if is_agent_button(button) && taken.insert(button) => {
                routed.insert(button, slot);
            }
            _ => waiting.push(slot),
        }
    }
    let mut unrouted = Vec::new();
    for slot in waiting {
        let free = AGENT_BUTTONS
            .rev()
            .filter_map(|n| MouseButtonNumber::try_new(n).ok())
            .find(|button| !taken.contains(button));
        match free {
            Some(button) => {
                taken.insert(button);
                bindings[slot] = FlashBinding::MouseButtons(button.mask());
                routed.insert(button, slot);
            }
            None => unrouted.extend(layout.slots.get(slot).map(|slot| slot.button)),
        }
    }
    FlashPlan {
        bindings,
        routed: routed
            .into_iter()
            .filter_map(|(button, slot)| Some((button, layout.slots.get(slot)?.button)))
            .collect(),
        unrouted,
    }
}

/// Where a write of a mouse's flash bindings stands.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum FlashWrite {
    /// Nothing is queued or running.
    #[default]
    Idle,
    /// A change waits for the write to start.
    Pending,
    /// The mouse is being written; a profile page rewrite takes seconds.
    Writing,
    /// The last write failed, and why.
    Failed(String),
}

/// The flash bindings of one mouse as the agent last read or wrote them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FlashBindingsState {
    /// The directory index of the profile the mouse plays, whose bindings
    /// these are; `None` before the first read, or while the mouse plays its
    /// factory profile, which has no page to hold bindings.
    pub profile: Option<u8>,
    /// The played profile's bindings, in slot order.
    pub slots: Vec<FlashBinding>,
    /// The agent button each agent-run slot presses, and the slot's button.
    pub routed: BTreeMap<MouseButtonNumber, ButtonId>,
    /// Agent-run slots no free agent button was left for.
    pub unrouted: Vec<ButtonId>,
    /// Where the write of a configuration change stands.
    pub write: FlashWrite,
}

impl FlashBindingsState {
    /// What slot `button` of `layout` holds, if it was read.
    #[must_use]
    pub fn slot(&self, layout: &GamingLayout, button: ButtonId) -> Option<&FlashBinding> {
        self.slots.get(usize::from(layout.slot_of(button)?))
    }

    /// Whether the agent runs `button`'s action: the slot presses an agent
    /// button the hook catches.
    #[must_use]
    pub fn routed_to_agent(&self, button: ButtonId) -> bool {
        self.routed.values().any(|&b| b == button)
    }
}

#[cfg(test)]
mod tests;
