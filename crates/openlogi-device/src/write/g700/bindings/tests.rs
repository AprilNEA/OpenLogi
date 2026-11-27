use hidpp::register::onboard_memory::g700::{
    Binding, ButtonMask, KeyModifiers, MacroPointer, Special,
};
use openlogi_core::binding::{FlashBinding, FlashFunction};

use super::super::sim::button;
use super::{flash_binding, wire_binding};

#[test]
fn every_profile_binding_converts_losslessly() {
    let bindings = [
        button(1),
        Binding::MouseButtons(ButtonMask(0x8003)),
        Binding::Key {
            modifiers: KeyModifiers::RIGHT_GUI | KeyModifiers::LEFT_SHIFT,
            usage: 0x2b,
        },
        Binding::Consumer(0x0192),
        Binding::Disabled,
        Binding::Macro(MacroPointer {
            page: 7,
            word_offset: 0x20,
        }),
        Binding::Unknown([0x85, 0x01, 0x02]),
        Binding::Unknown([0x8f, 0x01, 0x00]),
        Binding::Special(Special::Unknown(0x40)),
        Binding::Special(Special::ProfileCycleDown),
        Binding::Special(Special::ResolutionCycleDown),
        Binding::Special(Special::BatteryLevel),
    ];
    for binding in bindings {
        assert_eq!(
            wire_binding(flash_binding(binding)).to_bytes(),
            binding.to_bytes(),
            "{binding:?}"
        );
    }
    assert_eq!(
        flash_binding(Binding::Special(Special::Unknown(0x40))),
        FlashBinding::Function(FlashFunction::Other(0x40))
    );
}
