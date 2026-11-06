//! A mouse click repeated in a row with modifiers held around it: the
//! "Advanced click" Logi Options+ offers for a button.

use serde::{Deserialize, Serialize};

use super::action::Action;
use super::effect::MouseButton;
use super::key_combo::ChordModifiers;

/// The mouse button an [`AdvancedClick`] presses.
///
/// Append-only: the serde variant index is the IPC wire format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClickButton {
    /// The primary button.
    #[default]
    Left,
    /// The secondary button.
    Right,
    /// The middle button (wheel click).
    Middle,
}

impl From<ClickButton> for MouseButton {
    fn from(button: ClickButton) -> Self {
        match button {
            ClickButton::Left => Self::Left,
            ClickButton::Right => Self::Right,
            ClickButton::Middle => Self::Middle,
        }
    }
}

/// How many clicks an [`AdvancedClick`] posts in a row.
///
/// Append-only: the serde variant index is the IPC wire format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClickCount {
    /// One click.
    #[default]
    Single,
    /// Two clicks the OS counts as a double click.
    Double,
    /// Three clicks the OS counts as a triple click.
    Triple,
}

impl ClickCount {
    /// The number of clicks.
    #[must_use]
    pub const fn get(self) -> u8 {
        match self {
            Self::Single => 1,
            Self::Double => 2,
            Self::Triple => 3,
        }
    }
}

/// `count` clicks of `button` in a row, with `modifiers` held around them.
///
/// The clicks reach the OS as one double or triple click, not as separate
/// single clicks. That is the difference from firing [`Action::LeftClick`]
/// twice: macOS counts clicks by the click state each event carries, and the
/// other platforms by the time between them.
///
/// In `config.toml`, `modifiers` takes [`KeyCombo`](super::KeyCombo)'s
/// modifier names and may be left out when it is empty:
///
/// ```toml
/// Back = { AdvancedClick = { button = "Left", count = "Double", modifiers = "Cmd+Shift" } }
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdvancedClick {
    /// The button clicked.
    pub button: ClickButton,
    /// How many times it is clicked.
    pub count: ClickCount,
    /// The modifiers held from before the first click until after the last.
    #[serde(default)]
    pub modifiers: ChordModifiers,
}

impl AdvancedClick {
    /// Canonical English label, such as `Shift+Double Click`.
    #[must_use]
    pub fn rendered_label(self) -> String {
        let click = self.click_label();
        match self.modifiers.rendered() {
            Some(modifiers) => format!("{modifiers}+{click}"),
            None => click.to_owned(),
        }
    }

    /// English name of the click without its modifiers, such as
    /// `Double Click`.
    #[must_use]
    pub const fn click_label(self) -> &'static str {
        self.click_names().0
    }

    /// Catalog key whose English text is [`Self::click_label`].
    #[must_use]
    pub const fn click_translation_key(self) -> &'static str {
        self.click_names().1
    }

    /// The English name and catalog key of each button and count, in one
    /// table so the two cannot disagree. A single click shares the plain
    /// click actions' names.
    const fn click_names(self) -> (&'static str, &'static str) {
        match (self.button, self.count) {
            (ClickButton::Left, ClickCount::Single) => ("Left Click", "actions.left_click"),
            (ClickButton::Left, ClickCount::Double) => ("Double Click", "actions.double_click"),
            (ClickButton::Left, ClickCount::Triple) => ("Triple Click", "actions.triple_click"),
            (ClickButton::Right, ClickCount::Single) => ("Right Click", "actions.right_click"),
            (ClickButton::Right, ClickCount::Double) => {
                ("Right Double Click", "actions.right_double_click")
            }
            (ClickButton::Right, ClickCount::Triple) => {
                ("Right Triple Click", "actions.right_triple_click")
            }
            (ClickButton::Middle, ClickCount::Single) => ("Middle Click", "actions.middle_click"),
            (ClickButton::Middle, ClickCount::Double) => {
                ("Middle Double Click", "actions.middle_double_click")
            }
            (ClickButton::Middle, ClickCount::Triple) => {
                ("Middle Triple Click", "actions.middle_triple_click")
            }
        }
    }
}

/// A single click without modifiers becomes the matching plain click
/// action, so one behavior has one spelling in `config.toml`.
impl From<AdvancedClick> for Action {
    fn from(click: AdvancedClick) -> Self {
        if !click.modifiers.is_empty() || click.count != ClickCount::Single {
            return Self::AdvancedClick(click);
        }
        match click.button {
            ClickButton::Left => Self::LeftClick,
            ClickButton::Right => Self::RightClick,
            ClickButton::Middle => Self::MiddleClick,
        }
    }
}
