//! `0x8110` button layouts of G-series mice, dumped from real hardware.
//! Slot `n` is spy bit `n` and asset slot `g{n + 1}`.

use super::button::ButtonId;

/// One model's button slots.
#[derive(Debug, PartialEq, Eq)]
pub struct GamingLayout {
    /// HID++ model keys sharing this layout.
    pub model_keys: &'static [&'static str],
    /// The button on each slot.
    pub slots: &'static [ButtonId],
    /// Slots that send nothing in host mode unless captured.
    pub host_silent: &'static [ButtonId],
    /// Stock DPI levels, used until the user sets presets.
    pub stock_dpi_presets: &'static [u16],
}

/// G502 LIGHTSPEED (wpid `0x407f`).
pub const G502_LIGHTSPEED: GamingLayout = GamingLayout {
    model_keys: &["0407f"],
    slots: &[
        ButtonId::LeftClick,
        ButtonId::RightClick,
        ButtonId::MiddleClick,
        ButtonId::Back,
        ButtonId::Forward,
        ButtonId::G6,
        ButtonId::G7,
        ButtonId::G8,
        ButtonId::G9,
        ButtonId::WheelTiltRight,
        ButtonId::WheelTiltLeft,
    ],
    host_silent: &[
        ButtonId::G6,
        ButtonId::G7,
        ButtonId::G8,
        ButtonId::G9,
        ButtonId::WheelTiltRight,
        ButtonId::WheelTiltLeft,
    ],
    stock_dpi_presets: &[400, 800, 1600, 3200, 6400],
};

const LAYOUTS: &[&GamingLayout] = &[&G502_LIGHTSPEED];

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
        let slot = self.slots.iter().position(|&b| b == button)?;
        u8::try_from(slot).ok()
    }

    /// The button printed `G{number}`.
    #[must_use]
    pub fn button_for_g_number(&self, number: u8) -> Option<ButtonId> {
        let slot = usize::from(number.checked_sub(1)?);
        self.slots.get(slot).copied()
    }

    /// Every slot but the primary clicks.
    pub fn remappable(&self) -> impl Iterator<Item = ButtonId> + '_ {
        self.slots
            .iter()
            .copied()
            .filter(|b| !matches!(b, ButtonId::LeftClick | ButtonId::RightClick))
    }

    /// Whether rebinding `button` needs host mode.
    #[must_use]
    pub fn needs_host_mode(&self, button: ButtonId) -> bool {
        self.host_silent.contains(&button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn unknown_models_have_no_layout() {
        assert_eq!(GamingLayout::for_model_key("04099"), None);
        assert_eq!(GamingLayout::for_model_key("2b042"), None);
    }

    #[test]
    fn host_silent_buttons_are_slots() {
        for layout in LAYOUTS {
            for button in layout.host_silent {
                assert!(layout.slot_of(*button).is_some(), "{button:?}");
            }
        }
    }
}
