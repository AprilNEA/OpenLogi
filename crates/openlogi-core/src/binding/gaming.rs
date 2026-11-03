//! `0x8110` button layouts of G-series mice, dumped from real hardware.
//! Slot `n` is spy bit `n` and asset slot `g{n + 1}`.

use super::action::Action;
use super::button::ButtonId;
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
}

/// G502 LIGHTSPEED (wpid `0x407f`).
pub const G502_LIGHTSPEED: GamingLayout = GamingLayout {
    model_keys: &["0407f"],
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
};

const LAYOUTS: &[&GamingLayout] = &[&G502_LIGHTSPEED];

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
        capabilities
            .is_some_and(Capabilities::no_button_diversion)
            .then_some(Self::Standard)
    }

    /// The buttons the Buttons panel draws.
    #[must_use]
    pub const fn layout(self) -> &'static GamingLayout {
        match self {
            Self::Dumped(layout) => layout,
            Self::Standard => &STANDARD_BUTTONS,
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
