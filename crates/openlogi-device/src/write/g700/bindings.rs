//! The G700's button bindings as [`FlashBinding`]: the conversion between a
//! profile's binding codec and the vocabulary
//! `openlogi_core::binding::flash` decides in. The sync that reads and
//! writes them is `super::profiles`'.

use hidpp::register::onboard_memory::g700::{
    BUTTON_COUNT, Binding, ButtonMask, KeyModifiers, MacroPointer, Special,
};
use openlogi_core::binding::{FlashBinding, FlashFunction};

const _: () = assert!(BUTTON_COUNT == 13, "the G700 layout has thirteen slots");

/// A profile binding as [`FlashBinding`]. Lossless: [`wire_binding`] turns
/// it back into the binding it came from.
pub(super) fn flash_binding(binding: Binding) -> FlashBinding {
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
pub(super) fn wire_binding(binding: FlashBinding) -> Binding {
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
