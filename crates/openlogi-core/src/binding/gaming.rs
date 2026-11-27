//! Button layouts of G-series mice: the `0x8110` slots dumped from real
//! hardware, where slot `n` is spy bit `n` and asset slot `g{n + 1}`, and the
//! binding slots of a mouse that keeps its bindings in onboard flash.

use super::action::Action;
use super::button::ButtonId;
use super::flash::{FlashBinding, FlashFunction};
use crate::device::Capabilities;

/// One `0x8110` slot of a G-series mouse.
#[derive(Debug, PartialEq, Eq)]
pub struct Slot {
    /// The button on the slot.
    pub button: ButtonId,
    /// Whether the slot sends nothing in host mode unless captured.
    pub host_silent: bool,
    /// The button's stock function on this model, for a button whose stock
    /// function differs per model. `None` takes
    /// [`default_binding`](super::default_binding).
    pub stock: Option<Action>,
}

impl Slot {
    /// A slot that host mode still reports to the OS as a standard HID
    /// button.
    #[must_use]
    pub const fn native(button: ButtonId) -> Self {
        Self {
            button,
            host_silent: false,
            stock: None,
        }
    }

    /// A slot that host mode silences, with the same stock function on every
    /// model.
    #[must_use]
    pub const fn silent(button: ButtonId) -> Self {
        Self {
            button,
            host_silent: true,
            stock: None,
        }
    }

    /// A slot that host mode silences, with the stock function `stock` on
    /// this model. OpenLogi performs `stock` while it captures the button.
    #[must_use]
    pub const fn stock(button: ButtonId, stock: Action) -> Self {
        Self {
            button,
            host_silent: true,
            stock: Some(stock),
        }
    }
}

/// One model's button slots.
#[derive(Debug, PartialEq, Eq)]
pub struct GamingLayout {
    /// HID++ model keys sharing this layout.
    pub model_keys: &'static [&'static str],
    /// The slots, in `0x8110` order.
    pub slots: &'static [Slot],
    /// Stock DPI levels, used until the user sets presets.
    pub stock_dpi_presets: &'static [u16],
    /// What each slot does from the factory, for a mouse whose flash holds
    /// its bindings (see [`GamingButtons::Flash`]); empty for every other
    /// layout. Resetting a slot writes this back.
    pub flash_defaults: &'static [FlashBinding],
}

/// G502 LIGHTSPEED (wpid `0x407f`). The extended model id tells the colours
/// apart: `0` is the black unit dumped in #1729, `2` the purple one from
/// #2372, which reports the same wpid, model ids and 11 slots. `1` is the
/// pink one, sold with the purple one. No unit has been dumped. G HUB's
/// `g502_wireless` depot gives all three ids one default configuration of 11
/// inputs, and the same hotspot metadata to `1` and `2` (#2375).
pub const G502_LIGHTSPEED: GamingLayout = GamingLayout {
    model_keys: &["0407f", "1407f", "2407f"],
    slots: &[
        Slot::native(ButtonId::LeftClick),
        Slot::native(ButtonId::RightClick),
        Slot::native(ButtonId::MiddleClick),
        Slot::native(ButtonId::Back),
        Slot::native(ButtonId::Forward),
        Slot::stock(ButtonId::G6, Action::DpiShift),
        Slot::stock(ButtonId::G7, Action::PreviousDpiPreset),
        Slot::stock(ButtonId::G8, Action::NextDpiPreset),
        // G9 cycles the onboard profiles, which has no host equivalent.
        Slot::stock(ButtonId::G9, Action::None),
        Slot::silent(ButtonId::WheelTiltRight),
        Slot::silent(ButtonId::WheelTiltLeft),
    ],
    stock_dpi_presets: &[400, 800, 1600, 3200, 6400],
    flash_defaults: &[],
};

/// G903 LIGHTSPEED (wpid `0x4087`), dumped in #2178. Ambidextrous: the left
/// side pair is Back and Forward, the right side pair is G6 and G7, and the
/// DPI keys are G8 (down, left) and G9 (up, right). Slot 11 never reports.
pub const G903_LIGHTSPEED: GamingLayout = GamingLayout {
    model_keys: &["04087"],
    slots: &[
        Slot::native(ButtonId::LeftClick),
        Slot::native(ButtonId::RightClick),
        Slot::native(ButtonId::MiddleClick),
        Slot::native(ButtonId::Back),
        Slot::native(ButtonId::Forward),
        // The right side pair mirrors the left one.
        Slot::stock(ButtonId::G6, Action::MouseBack),
        Slot::stock(ButtonId::G7, Action::MouseForward),
        Slot::stock(ButtonId::G8, Action::PreviousDpiPreset),
        Slot::stock(ButtonId::G9, Action::NextDpiPreset),
        Slot::silent(ButtonId::WheelTiltLeft),
        Slot::silent(ButtonId::WheelTiltRight),
    ],
    stock_dpi_presets: &[400, 800, 1600, 3200, 6400],
    flash_defaults: &[],
};

/// Logitech G700 / G700s: the thirteen binding slots of each onboard
/// profile, slot `n` at index `n` of the profile's binding table (libratbag's
/// button order, Piper's `button{n}`).
///
/// The left, right, middle, back and forward slots and the wheel tilts are
/// libratbag's. Where slots 5 to 10 sit on the mouse has not been checked
/// against the hardware yet, so they carry neutral names, G6 to G11 by slot
/// number as the G502's do. This table and `flash_defaults` are the one
/// place a hardware pass corrects.
pub const G700: GamingLayout = GamingLayout {
    model_keys: &[],
    slots: &[
        Slot::native(ButtonId::LeftClick),
        Slot::native(ButtonId::RightClick),
        Slot::native(ButtonId::MiddleClick),
        Slot::native(ButtonId::Back),
        Slot::native(ButtonId::Forward),
        Slot::native(ButtonId::G6),
        Slot::native(ButtonId::G7),
        Slot::native(ButtonId::G8),
        Slot::native(ButtonId::G9),
        Slot::native(ButtonId::G10),
        Slot::native(ButtonId::G11),
        Slot::native(ButtonId::WheelTiltLeft),
        Slot::native(ButtonId::WheelTiltRight),
    ],
    stock_dpi_presets: &[],
    // libratbag's defaults and the user profiles dumped on 2026-10-31:
    // buttons 1 to 7, then the battery, DPI and profile functions.
    flash_defaults: &[
        FlashBinding::MouseButtons(1 << 0),
        FlashBinding::MouseButtons(1 << 1),
        FlashBinding::MouseButtons(1 << 2),
        FlashBinding::MouseButtons(1 << 3),
        FlashBinding::MouseButtons(1 << 4),
        FlashBinding::MouseButtons(1 << 5),
        FlashBinding::MouseButtons(1 << 6),
        FlashBinding::Function(FlashFunction::BatteryLevel),
        FlashBinding::Function(FlashFunction::ResolutionDown),
        FlashBinding::Function(FlashFunction::ResolutionUp),
        FlashBinding::Function(FlashFunction::ProfileCycleUp),
        FlashBinding::Function(FlashFunction::WheelLeft),
        FlashBinding::Function(FlashFunction::WheelRight),
    ],
};

/// A gaming mouse whose layout nobody has dumped. Its first five slots are
/// the standard HID buttons, which the OS input hook remaps without host
/// mode. G6 and up differ per model and are not guessed.
///
/// For drawing the Buttons panel only: [`GamingLayout::for_model_key`] never
/// returns it, so the agent captures nothing through it.
pub const STANDARD_BUTTONS: GamingLayout = GamingLayout {
    model_keys: &[],
    slots: &[
        Slot::native(ButtonId::LeftClick),
        Slot::native(ButtonId::RightClick),
        Slot::native(ButtonId::MiddleClick),
        Slot::native(ButtonId::Back),
        Slot::native(ButtonId::Forward),
    ],
    stock_dpi_presets: &[],
    flash_defaults: &[],
};

const LAYOUTS: &[&GamingLayout] = &[&G502_LIGHTSPEED, &G903_LIGHTSPEED];

/// Whether a device is a G-series mouse, and which buttons it offers. The
/// agent, the tabs and the Buttons panel all ask this one verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamingButtons {
    /// A model whose `0x8110` layout was dumped: host mode captures every
    /// slot.
    Dumped(&'static GamingLayout),
    /// A device with the gaming tables and no ReprogControls whose layout
    /// nobody has dumped: only [`STANDARD_BUTTONS`], which the OS hook
    /// remaps.
    Standard,
    /// A mouse that keeps its bindings in onboard flash
    /// ([`Capabilities::flash_bindings`]): the agent writes them there, and
    /// the OS hook catches only the buttons it gave actions the mouse cannot
    /// run (see [`crate::binding::flash`]).
    Flash(&'static GamingLayout),
}

impl GamingButtons {
    /// The verdict for `model_key` with `capabilities`, which is `None` for a
    /// device never probed; a dumped model answers without them. `None` for a
    /// device that diverts its controls through ReprogControls.
    #[must_use]
    pub fn for_device(model_key: &str, capabilities: Option<&Capabilities>) -> Option<Self> {
        if let Some(layout) = GamingLayout::for_model_key(model_key) {
            return Some(Self::Dumped(layout));
        }
        if let Some(layout) = Self::flash_layout_for(capabilities) {
            return Some(Self::Flash(layout));
        }
        capabilities
            .is_some_and(Capabilities::no_button_diversion)
            .then_some(Self::Standard)
    }

    /// The layout of a mouse whose flash holds its bindings, judged by its
    /// `capabilities` alone: the agent's flash writes need no model key,
    /// which a HID++ 1.0 mouse does not report.
    #[must_use]
    pub fn flash_layout_for(capabilities: Option<&Capabilities>) -> Option<&'static GamingLayout> {
        // Only the G700 sets the flag; a second such model would need its
        // own key to tell its layout apart.
        capabilities
            .is_some_and(|capabilities| capabilities.flash_bindings)
            .then_some(&G700)
    }

    /// The buttons the Buttons panel draws.
    #[must_use]
    pub const fn layout(self) -> &'static GamingLayout {
        match self {
            Self::Dumped(layout) | Self::Flash(layout) => layout,
            Self::Standard => &STANDARD_BUTTONS,
        }
    }

    /// The layout of a mouse whose flash holds its bindings, `None` for any
    /// other verdict.
    #[must_use]
    pub const fn flash_layout(self) -> Option<&'static GamingLayout> {
        match self {
            Self::Flash(layout) => Some(layout),
            Self::Dumped(_) | Self::Standard => None,
        }
    }
}

impl GamingLayout {
    /// The layout for a HID++ model key.
    #[must_use]
    pub fn for_model_key(model_key: &str) -> Option<&'static Self> {
        LAYOUTS
            .iter()
            .copied()
            .find(|layout| layout.model_keys.contains(&model_key))
    }

    /// The slot of `button`.
    #[must_use]
    pub fn slot_of(&self, button: ButtonId) -> Option<u8> {
        let slot = self.slots.iter().position(|slot| slot.button == button)?;
        u8::try_from(slot).ok()
    }

    /// The button printed `G{number}`.
    #[must_use]
    pub fn button_for_g_number(&self, number: u8) -> Option<ButtonId> {
        let slot = usize::from(number.checked_sub(1)?);
        self.slots.get(slot).map(|slot| slot.button)
    }

    /// The button on each slot, in slot order.
    pub fn buttons(&self) -> impl Iterator<Item = ButtonId> + '_ {
        self.slots.iter().map(|slot| slot.button)
    }

    /// Every slot but the primary clicks.
    pub fn remappable(&self) -> impl Iterator<Item = ButtonId> + '_ {
        self.buttons()
            .filter(|b| !matches!(b, ButtonId::LeftClick | ButtonId::RightClick))
    }

    /// Whether rebinding `button` needs host mode.
    #[must_use]
    pub fn needs_host_mode(&self, button: ButtonId) -> bool {
        self.slot(button).is_some_and(|slot| slot.host_silent)
    }

    /// The stock function of `button` on this model, if it differs per
    /// model. [`default_binding_on`](super::default_binding_on) falls back to
    /// [`default_binding`](super::default_binding) for every other button.
    #[must_use]
    pub fn stock_action(&self, button: ButtonId) -> Option<Action> {
        self.slot(button)?.stock.clone()
    }

    fn slot(&self, button: ButtonId) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.button == button)
    }

    /// What slot `button` does from the factory, on a flash layout.
    #[must_use]
    pub fn flash_default(&self, button: ButtonId) -> Option<FlashBinding> {
        self.flash_defaults
            .get(usize::from(self.slot_of(button)?))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaming_buttons_name_the_dumped_layout_or_the_standard_buttons() {
        let gaming = Capabilities::from_feature_ids(&[0x2201, 0x8100, 0x8110]);
        let mx = Capabilities::from_feature_ids(&[0x1b04, 0x2201]);
        assert_eq!(
            GamingButtons::for_device("0407f", None),
            Some(GamingButtons::Dumped(&G502_LIGHTSPEED)),
            "a dumped model answers before it is probed"
        );
        assert_eq!(
            GamingButtons::for_device("0409f", Some(&gaming)),
            Some(GamingButtons::Standard)
        );
        assert_eq!(GamingButtons::Standard.layout(), &STANDARD_BUTTONS);
        assert_eq!(GamingButtons::for_device("0409f", None), None);
        assert_eq!(GamingButtons::for_device("2b042", Some(&mx)), None);
    }

    #[test]
    fn g502_lightspeed_slots_match_the_hardware_dump() {
        let layout = GamingLayout::for_model_key("0407f").unwrap();
        assert_eq!(layout.slot_of(ButtonId::G9), Some(8));
        assert_eq!(layout.slot_of(ButtonId::WheelTiltLeft), Some(10));
        assert_eq!(layout.button_for_g_number(4), Some(ButtonId::Back));
        assert_eq!(
            layout.button_for_g_number(10),
            Some(ButtonId::WheelTiltRight)
        );
        assert_eq!(layout.button_for_g_number(0), None);
        assert_eq!(layout.button_for_g_number(12), None);
        assert!(!layout.remappable().any(|b| b == ButtonId::LeftClick));
        assert!(layout.needs_host_mode(ButtonId::G7));
        assert!(!layout.needs_host_mode(ButtonId::Back));
    }

    /// #2372: the purple G502 LIGHTSPEED reports extended model id 2, and
    /// its gaming tables made it a five-button `Standard` mouse. #2375: the
    /// pink one has id 1.
    #[test]
    fn every_g502_lightspeed_colour_takes_its_layout() {
        use crate::device::{DeviceModelInfo, DeviceTransports};
        let gaming = Capabilities::from_feature_ids(&[0x2201, 0x8060, 0x8100, 0x8110]);
        for extended_model_id in [0x00, 0x01, 0x02] {
            let model = DeviceModelInfo {
                entity_count: 3,
                serial_number: None,
                unit_id: [0; 4],
                transports: DeviceTransports {
                    usb: true,
                    equad: true,
                    btle: false,
                    bluetooth: false,
                },
                model_ids: [0x407f, 0xc08d, 0x0000],
                extended_model_id,
            };
            assert_eq!(
                GamingButtons::for_device(&model.model_key(), Some(&gaming)),
                Some(GamingButtons::Dumped(&G502_LIGHTSPEED)),
                "extended model id {extended_model_id}"
            );
        }
    }

    #[test]
    fn g903_lightspeed_slots_match_the_hardware_dump() {
        let layout = GamingLayout::for_model_key("04087").unwrap();
        assert_eq!(layout, &G903_LIGHTSPEED);
        assert_eq!(layout.button_for_g_number(4), Some(ButtonId::Back));
        assert_eq!(layout.button_for_g_number(5), Some(ButtonId::Forward));
        assert_eq!(layout.button_for_g_number(6), Some(ButtonId::G6));
        assert_eq!(layout.button_for_g_number(9), Some(ButtonId::G9));
        assert_eq!(
            layout.button_for_g_number(10),
            Some(ButtonId::WheelTiltLeft)
        );
        assert_eq!(
            layout.button_for_g_number(11),
            Some(ButtonId::WheelTiltRight)
        );
        assert_eq!(layout.button_for_g_number(12), None);
        assert!(layout.needs_host_mode(ButtonId::G8));
        assert!(!layout.needs_host_mode(ButtonId::Forward));
    }

    #[test]
    fn g903_lightspeed_keeps_its_stock_functions() {
        let layout = &G903_LIGHTSPEED;
        assert_eq!(layout.stock_action(ButtonId::G6), Some(Action::MouseBack));
        assert_eq!(
            layout.stock_action(ButtonId::G7),
            Some(Action::MouseForward)
        );
        assert_eq!(
            layout.stock_action(ButtonId::G8),
            Some(Action::PreviousDpiPreset)
        );
        assert_eq!(
            layout.stock_action(ButtonId::G9),
            Some(Action::NextDpiPreset)
        );
        assert_eq!(layout.stock_action(ButtonId::WheelTiltLeft), None);
    }

    #[test]
    fn standard_buttons_are_what_the_os_hook_remaps() {
        let layout = &STANDARD_BUTTONS;
        assert_eq!(layout.button_for_g_number(3), Some(ButtonId::MiddleClick));
        assert_eq!(layout.button_for_g_number(4), Some(ButtonId::Back));
        assert_eq!(layout.button_for_g_number(5), Some(ButtonId::Forward));
        assert_eq!(layout.button_for_g_number(6), None, "G6+ is model-specific");
        assert!(layout.remappable().all(ButtonId::is_os_hook_button));
        assert!(!layout.remappable().any(|b| layout.needs_host_mode(b)));
    }

    #[test]
    fn unknown_models_have_no_layout() {
        assert_eq!(GamingLayout::for_model_key("04099"), None);
        assert_eq!(GamingLayout::for_model_key("2b042"), None);
    }

    #[test]
    fn g502_lightspeed_keeps_its_stock_functions() {
        let layout = &G502_LIGHTSPEED;
        assert_eq!(layout.stock_action(ButtonId::G6), Some(Action::DpiShift));
        assert_eq!(
            layout.stock_action(ButtonId::G7),
            Some(Action::PreviousDpiPreset)
        );
        assert_eq!(
            layout.stock_action(ButtonId::G8),
            Some(Action::NextDpiPreset)
        );
        assert_eq!(layout.stock_action(ButtonId::G9), Some(Action::None));
        assert_eq!(layout.stock_action(ButtonId::WheelTiltLeft), None);
        assert_eq!(layout.stock_action(ButtonId::Back), None);
        assert!(layout.needs_host_mode(ButtonId::WheelTiltRight));
    }

    #[test]
    fn the_default_of_a_g_button_comes_from_its_layout() {
        use super::super::defaults::{default_binding, default_binding_on};
        let g502 = Some(&G502_LIGHTSPEED);
        assert_eq!(default_binding_on(g502, ButtonId::G6), Action::DpiShift);
        assert_eq!(default_binding_on(None, ButtonId::G6), Action::None);
        assert_eq!(default_binding(ButtonId::G8), Action::None);
        for button in [
            ButtonId::Back,
            ButtonId::WheelTiltLeft,
            ButtonId::MiddleClick,
        ] {
            assert_eq!(default_binding_on(g502, button), default_binding(button));
        }
    }

    /// Gesture mode and wheel hold resolve defaults without a layout, which
    /// is right only while no G-series button offers them.
    #[test]
    fn g_buttons_offer_neither_gesture_mode_nor_wheel_hold() {
        for button in ButtonId::G_BUTTONS {
            assert!(!button.supports_gesture_mode(), "{button:?}");
            assert!(!button.supports_wheel_hold(), "{button:?}");
        }
    }

    #[test]
    fn every_g_button_on_a_dumped_layout_has_a_stock_function() {
        for layout in LAYOUTS {
            for slot in layout.slots {
                if ButtonId::G_BUTTONS.contains(&slot.button) {
                    assert!(slot.stock.is_some(), "{:?}", slot.button);
                }
            }
        }
    }

    #[test]
    fn a_stock_function_belongs_to_a_host_silent_slot() {
        for layout in LAYOUTS {
            for slot in layout.slots {
                if slot.stock.is_some() {
                    assert!(slot.host_silent, "{:?}", slot.button);
                }
            }
        }
    }
}
