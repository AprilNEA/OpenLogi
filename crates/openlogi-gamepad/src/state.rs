//! Snapshot of a standard-layout gamepad and host rumble.

use openlogi_core::binding::{DpadDirection, GamepadAxis, GamepadFaceButton};

/// Dual-rumble magnitudes from the host (0.0–1.0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rumble {
    /// Strong / low-frequency motor.
    pub strong: f32,
    /// Weak / high-frequency motor.
    pub weak: f32,
}

impl Rumble {
    /// Whether either motor is above a quiet threshold.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.strong > 0.01 || self.weak > 0.01
    }

    /// Peak magnitude as a 0..=100 haptic intensity percentage.
    #[must_use]
    pub fn intensity_percent(self) -> u8 {
        let peak = self.strong.max(self.weak).clamp(0.0, 1.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "peak is clamped to 0..=1 before scaling to u8"
        )]
        {
            (peak * 100.0).round() as u8
        }
    }
}

/// Full standard-layout pad state submitted as one report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamepadState {
    /// Face / shoulder / menu buttons by standard index bit.
    buttons: u16,
    /// D-pad: -1/0/1 on X and Y.
    pub dpad_x: i8,
    /// D-pad Y.
    pub dpad_y: i8,
    /// Left stick X in `-1.0..=1.0`.
    pub left_x: f32,
    /// Left stick Y in `-1.0..=1.0` (negative is up).
    pub left_y: f32,
    /// Right stick X.
    pub right_x: f32,
    /// Right stick Y.
    pub right_y: f32,
    /// Left trigger `0.0..=1.0`.
    pub left_trigger: f32,
    /// Right trigger `0.0..=1.0`.
    pub right_trigger: f32,
}

impl GamepadState {
    /// Set a face/shoulder/menu button.
    pub fn set_button(&mut self, button: GamepadFaceButton, pressed: bool) {
        let bit = 1u16 << button.standard_index();
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
    }

    /// Whether `button` is pressed.
    #[must_use]
    pub fn button_pressed(&self, button: GamepadFaceButton) -> bool {
        self.buttons & (1u16 << button.standard_index()) != 0
    }

    /// Raw button bitmask (bit N = standard index N).
    #[must_use]
    pub const fn buttons_mask(&self) -> u16 {
        self.buttons
    }

    /// Apply a D-pad arm (clears other arms when `pressed`).
    pub fn set_dpad(&mut self, direction: DpadDirection, pressed: bool) {
        if !pressed {
            match direction {
                DpadDirection::Up if self.dpad_y < 0 => self.dpad_y = 0,
                DpadDirection::Down if self.dpad_y > 0 => self.dpad_y = 0,
                DpadDirection::Left if self.dpad_x < 0 => self.dpad_x = 0,
                DpadDirection::Right if self.dpad_x > 0 => self.dpad_x = 0,
                _ => {}
            }
            return;
        }
        match direction {
            DpadDirection::Up => self.dpad_y = -1,
            DpadDirection::Down => self.dpad_y = 1,
            DpadDirection::Left => self.dpad_x = -1,
            DpadDirection::Right => self.dpad_x = 1,
        }
    }

    /// Write a continuous axis value in `-1.0..=1.0` (triggers clamp to `0..=1`).
    pub fn set_axis(&mut self, axis: GamepadAxis, value: f32) {
        let clamped = value.clamp(-1.0, 1.0);
        match axis {
            GamepadAxis::LeftStickX => self.left_x = clamped,
            GamepadAxis::LeftStickY => self.left_y = clamped,
            GamepadAxis::RightStickX => self.right_x = clamped,
            GamepadAxis::RightStickY => self.right_y = clamped,
            GamepadAxis::LeftTrigger => self.left_trigger = clamped.clamp(0.0, 1.0),
            GamepadAxis::RightTrigger => self.right_trigger = clamped.clamp(0.0, 1.0),
        }
    }

    /// Pack into the 8-byte input report matching [`super::descriptor`].
    #[must_use]
    pub fn to_input_report(&self) -> [u8; 8] {
        let lx = axis_to_u8(self.left_x);
        let ly = axis_to_u8(self.left_y);
        let rx = axis_to_u8(self.right_x);
        let ry = axis_to_u8(self.right_y);
        let lt = trigger_to_u8(self.left_trigger);
        let rt = trigger_to_u8(self.right_trigger);
        let hat = hat_nibble(self.dpad_x, self.dpad_y);
        let btn = self.buttons;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "report packs the low 12 button bits into two bytes by design"
        )]
        {
            [
                lx,
                ly,
                rx,
                ry,
                lt,
                rt,
                hat | (((btn & 0x0F) as u8) << 4),
                (btn >> 4) as u8,
            ]
        }
    }
}

fn axis_to_u8(value: f32) -> u8 {
    let scaled = ((value + 1.0) * 127.5).round().clamp(0.0, 255.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scaled is clamped to 0..=255"
    )]
    {
        scaled as u8
    }
}

fn trigger_to_u8(value: f32) -> u8 {
    let scaled = (value.clamp(0.0, 1.0) * 255.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scaled is clamped to 0..=255"
    )]
    {
        scaled as u8
    }
}

/// HID hat switch nibble: 0–7 directions, 8 = neutral.
fn hat_nibble(x: i8, y: i8) -> u8 {
    match (x, y) {
        (0, -1) => 0,
        (1, -1) => 1,
        (1, 0) => 2,
        (1, 1) => 3,
        (0, 1) => 4,
        (-1, 1) => 5,
        (-1, 0) => 6,
        (-1, -1) => 7,
        _ => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_bits_follow_standard_indices() {
        let mut state = GamepadState::default();
        state.set_button(GamepadFaceButton::A, true);
        state.set_button(GamepadFaceButton::Start, true);
        assert_eq!(state.buttons_mask() & 1, 1);
        assert_eq!(state.buttons_mask() & (1 << 9), 1 << 9);
    }

    #[test]
    fn rumble_intensity_scales() {
        assert_eq!(
            Rumble {
                strong: 0.5,
                weak: 0.25
            }
            .intensity_percent(),
            50
        );
        assert!(!Rumble::default().is_active());
    }
}
