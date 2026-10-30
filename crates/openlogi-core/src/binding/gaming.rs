//! `0x8110` button layouts of G-series mice, dumped from real hardware.
//! Slot `n` is spy bit `n` and asset slot `g{n + 1}`.

use super::button::ButtonId;
use crate::device::Capabilities;

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

/// A gaming mouse whose layout nobody has dumped. Its first five slots are
/// the standard HID buttons, which the OS input hook remaps without host
/// mode. G6 and up differ per model and are not guessed.
///
/// For drawing the Buttons panel only: [`GamingLayout::for_model_key`] never
/// returns it, so the agent captures nothing through it.
pub const STANDARD_BUTTONS: GamingLayout = GamingLayout {
    model_keys: &[],
    slots: &[
        ButtonId::LeftClick,
        ButtonId::RightClick,
        ButtonId::MiddleClick,
        ButtonId::Back,
        ButtonId::Forward,
    ],
    host_silent: &[],
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
    fn host_silent_buttons_are_slots() {
        for layout in LAYOUTS {
            for button in layout.host_silent {
                assert!(layout.slot_of(*button).is_some(), "{button:?}");
            }
        }
    }
}
