//! Opt-in auxiliary virtual-gamepad mapping for pointing devices.
//!
//! The persisted config is intentionally tiny (`enabled` / `rumble`); the
//! default control → standard-layout map lives here so later override TOML can
//! land without another schema fight.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ButtonId, GestureDirection};

/// Per-device opt-in for exposing remapped extras as a virtual gamepad.
///
/// Disabled by default and omitted from `config.toml` when unset. Left/right
/// click and pointer motion are never claimed by the default map — the mouse
/// stays a mouse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GamepadConfig {
    /// Publish an OS-visible virtual gamepad for this device.
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    /// Forward host dual-rumble to device haptics (`0x19b0`) when capable.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub rumble: bool,
}

impl Default for GamepadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rumble: true,
        }
    }
}

impl GamepadConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// A button on the W3C / Xbox "standard" gamepad layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GamepadFaceButton {
    /// `buttons[0]` — bottom (A / South).
    A,
    /// `buttons[1]` — right (B / East).
    B,
    /// `buttons[2]` — left (X / West).
    X,
    /// `buttons[3]` — top (Y / North).
    Y,
    /// `buttons[4]` — left bumper.
    LeftShoulder,
    /// `buttons[5]` — right bumper.
    RightShoulder,
    /// `buttons[8]` — Select / Back.
    Select,
    /// `buttons[9]` — Start.
    Start,
}

impl GamepadFaceButton {
    /// Index in the Gamepad API `buttons` array for the standard mapping.
    #[must_use]
    pub const fn standard_index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::X => 2,
            Self::Y => 3,
            Self::LeftShoulder => 4,
            Self::RightShoulder => 5,
            Self::Select => 8,
            Self::Start => 9,
        }
    }
}

/// Continuous axes on the standard gamepad layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GamepadAxis {
    /// `axes[0]` — left stick X.
    LeftStickX,
    /// `axes[1]` — left stick Y.
    LeftStickY,
    /// `axes[2]` — right stick X.
    RightStickX,
    /// `axes[3]` — right stick Y.
    RightStickY,
    /// Left trigger (`buttons[6]` analog).
    LeftTrigger,
    /// Right trigger (`buttons[7]` analog).
    RightTrigger,
}

/// How a physical mouse control feeds the virtual pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GamepadBinding {
    /// Discrete face / shoulder / menu button.
    Button(GamepadFaceButton),
    /// Continuous axis; for thumb-wheel scroll the sign follows rotation.
    Axis(GamepadAxis),
    /// Four swipe directions become the D-pad hat; click is separate.
    Dpad,
}

/// Resolved control → pad map used while gamepad mode is enabled.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GamepadMap {
    /// Plain button and thumb-wheel slots.
    buttons: BTreeMap<ButtonId, GamepadBinding>,
    /// Gesture sources whose swipe map owns the D-pad (and optional click).
    gestures: BTreeMap<ButtonId, GamepadGestureMap>,
}

/// Per-gesture-source pad bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamepadGestureMap {
    /// Swipe directions → D-pad.
    pub dpad: bool,
    /// Plain click without a swipe.
    pub click: Option<GamepadFaceButton>,
}

impl GamepadMap {
    /// Default auxiliary map for MX Master–class mice.
    ///
    /// Left/right/middle and the main wheel stay native so the pointer keeps
    /// working while extras feed the pad.
    #[must_use]
    pub fn default_for_mouse() -> Self {
        let mut buttons = BTreeMap::new();
        buttons.insert(ButtonId::Back, GamepadBinding::Button(GamepadFaceButton::B));
        buttons.insert(
            ButtonId::Forward,
            GamepadBinding::Button(GamepadFaceButton::A),
        );
        buttons.insert(
            ButtonId::DpiToggle,
            GamepadBinding::Button(GamepadFaceButton::Select),
        );
        buttons.insert(
            ButtonId::Thumbwheel,
            GamepadBinding::Button(GamepadFaceButton::Y),
        );
        buttons.insert(
            ButtonId::ThumbwheelScrollUp,
            GamepadBinding::Axis(GamepadAxis::RightStickX),
        );
        buttons.insert(
            ButtonId::ThumbwheelScrollDown,
            GamepadBinding::Axis(GamepadAxis::RightStickX),
        );

        let mut gestures = BTreeMap::new();
        gestures.insert(
            ButtonId::GestureButton,
            GamepadGestureMap {
                dpad: true,
                click: Some(GamepadFaceButton::Start),
            },
        );
        gestures.insert(
            ButtonId::HapticPanel,
            GamepadGestureMap {
                dpad: true,
                click: Some(GamepadFaceButton::X),
            },
        );

        Self { buttons, gestures }
    }

    /// Whether this physical control is owned by the pad (actions must not run).
    #[must_use]
    pub fn owns_button(&self, button: ButtonId) -> bool {
        self.buttons.contains_key(&button) || self.gestures.contains_key(&button)
    }

    /// Plain (non-gesture) binding for `button`, if any.
    #[must_use]
    pub fn button_binding(&self, button: ButtonId) -> Option<GamepadBinding> {
        self.buttons.get(&button).copied()
    }

    /// Gesture-source map for `button`, if any.
    #[must_use]
    pub fn gesture_map(&self, button: ButtonId) -> Option<&GamepadGestureMap> {
        self.gestures.get(&button)
    }

    /// Every button id the capture path must divert while the pad is live.
    pub fn divert_buttons(&self) -> impl Iterator<Item = ButtonId> + '_ {
        self.buttons
            .keys()
            .copied()
            .chain(self.gestures.keys().copied())
    }

    /// Face button for a gesture click, when the source maps one.
    #[must_use]
    pub fn gesture_click(&self, button: ButtonId) -> Option<GamepadFaceButton> {
        self.gestures.get(&button).and_then(|map| map.click)
    }

    /// Whether swipes on `button` drive the D-pad.
    #[must_use]
    pub fn gesture_owns_dpad(&self, button: ButtonId) -> bool {
        self.gestures.get(&button).is_some_and(|map| map.dpad)
    }

    /// D-pad direction for a swipe, when this source owns the hat.
    #[must_use]
    pub fn dpad_direction(
        &self,
        button: ButtonId,
        swipe: GestureDirection,
    ) -> Option<DpadDirection> {
        if !self.gesture_owns_dpad(button) {
            return None;
        }
        match swipe {
            GestureDirection::Up => Some(DpadDirection::Up),
            GestureDirection::Down => Some(DpadDirection::Down),
            GestureDirection::Left => Some(DpadDirection::Left),
            GestureDirection::Right => Some(DpadDirection::Right),
            GestureDirection::Click => None,
        }
    }
}

/// One of the four D-pad arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DpadDirection {
    /// Hat north.
    Up,
    /// Hat south.
    Down,
    /// Hat west.
    Left,
    /// Hat east.
    Right,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a fn(&T) -> bool signature"
)]
fn is_true(b: &bool) -> bool {
    *b
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_omitted_from_toml() {
        let cfg = GamepadConfig::default();
        assert!(cfg.is_default());
        assert!(!cfg.enabled);
        assert!(cfg.rumble);
    }

    #[test]
    fn default_map_leaves_primary_mouse_native() {
        let map = GamepadMap::default_for_mouse();
        assert!(!map.owns_button(ButtonId::LeftClick));
        assert!(!map.owns_button(ButtonId::RightClick));
        assert!(!map.owns_button(ButtonId::MiddleClick));
        assert!(map.owns_button(ButtonId::Back));
        assert!(map.owns_button(ButtonId::GestureButton));
        assert!(map.owns_button(ButtonId::HapticPanel));
        assert!(map.owns_button(ButtonId::ThumbwheelScrollUp));
    }

    #[test]
    fn gamepad_config_round_trips() {
        let raw = "enabled = true\nrumble = false\n";
        let cfg: GamepadConfig = toml::from_str(raw).expect("parse");
        assert!(cfg.enabled);
        assert!(!cfg.rumble);
        let out = toml::to_string(&cfg).expect("serialize");
        assert!(out.contains("enabled = true"));
        assert!(out.contains("rumble = false"));
    }
}
