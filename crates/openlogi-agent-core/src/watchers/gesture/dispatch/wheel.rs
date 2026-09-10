//! Thumb-wheel binding state for one captured session's input dispatcher.
//!
//! Each physical rotation direction owns an independent state machine. Within
//! one direction, continuous scrolling and discrete actions are mutually
//! exclusive states; suppressed input clears either. Changing the effective
//! binding or sensitivity discards progress and cooldown from the previous
//! configuration.

use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::thumbwheel::WheelResolution;

/// Idle gap after which a partly accumulated discrete action is forgotten, so
/// slow intermittent nudges do not eventually cross the threshold.
const ACTION_DECAY: Duration = Duration::from_millis(300);

/// Per-direction wheel state. Reversing the physical wheel must not cancel
/// progress already earned in the other direction.
#[derive(Default)]
pub(super) struct WheelAccumulators {
    up: WheelDirection,
    down: WheelDirection,
}

impl WheelAccumulators {
    /// Advance the state belonging to `rotation`'s physical direction.
    pub(super) fn advance(
        &mut self,
        rotation: WheelRotation,
        action: &Action,
        scale: ScrollScale,
        now: Instant,
    ) -> WheelOutput {
        let direction = match rotation.direction {
            PhysicalDirection::Up => &mut self.up,
            PhysicalDirection::Down => &mut self.down,
        };
        direction.advance(action, rotation.magnitude, scale, now)
    }
}

/// One non-zero captured wheel rotation, split into physical direction and
/// positive magnitude once at the input boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WheelRotation {
    direction: PhysicalDirection,
    magnitude: i32,
}

impl WheelRotation {
    /// Decode the signed HID++ increment count. Zero carries no rotation.
    pub(super) fn from_increments(increments: i16) -> Option<Self> {
        let direction = match increments.cmp(&0) {
            std::cmp::Ordering::Greater => PhysicalDirection::Up,
            std::cmp::Ordering::Less => PhysicalDirection::Down,
            std::cmp::Ordering::Equal => return None,
        };
        Some(Self {
            direction,
            // Convert before `abs` so i16::MIN remains representable.
            magnitude: i32::from(increments).abs(),
        })
    }

    /// Binding key for this physical direction.
    pub(super) const fn button(self) -> ButtonId {
        match self.direction {
            PhysicalDirection::Up => ButtonId::ThumbwheelScrollUp,
            PhysicalDirection::Down => ButtonId::ThumbwheelScrollDown,
        }
    }
}

/// Physical sign of one captured rotation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhysicalDirection {
    Up,
    Down,
}

/// Running state for one physical rotation direction.
#[derive(Default)]
struct WheelDirection {
    state: WheelState,
}

impl WheelDirection {
    /// Advance this direction for its current effective binding.
    fn advance(
        &mut self,
        action: &Action,
        magnitude: i32,
        scale: ScrollScale,
        now: Instant,
    ) -> WheelOutput {
        if matches!(action, Action::None) {
            self.state = WheelState::Idle;
            return WheelOutput::Idle;
        }
        if let Some(binding) = ScrollBinding::from_action(action) {
            return self.advance_scroll(binding, magnitude, scale);
        }
        self.advance_action(action, magnitude, scale.sensitivity, now)
    }

    /// Emit continuous scroll immediately as a typed fractional delta. The
    /// smooth-scroll runtime owns interpolation; this state only records the
    /// mutually exclusive mode so returning to a discrete action starts fresh.
    fn advance_scroll(
        &mut self,
        binding: ScrollBinding,
        magnitude: i32,
        scale: ScrollScale,
    ) -> WheelOutput {
        let context = ScrollContext { binding, scale };
        if !matches!(&self.state, WheelState::Scroll(previous) if *previous == context) {
            self.state = WheelState::Scroll(context);
        }
        let distance = f64::from(magnitude) * scale.per_increment();
        if distance == 0.0 {
            WheelOutput::Idle
        } else {
            binding.output(distance)
        }
    }

    /// Accumulate one discrete action. Progress, decay, and cooldown all belong
    /// to the exact action and sensitivity that earned them.
    fn advance_action(
        &mut self,
        action: &Action,
        magnitude: i32,
        sensitivity: ThumbwheelSensitivity,
        now: Instant,
    ) -> WheelOutput {
        let next_binding = DiscreteBinding {
            action: action.clone(),
            sensitivity,
        };
        let (mut increments, last_event, last_fired) = match std::mem::take(&mut self.state) {
            WheelState::Action {
                binding,
                increments,
                last_event,
                last_fired,
            } if binding == next_binding => (increments, Some(last_event), last_fired),
            _ => (0, None, None),
        };

        if last_event.is_some_and(|time| now.saturating_duration_since(time) > ACTION_DECAY) {
            increments = 0;
        }

        // The cooldown comes from the sensitivity slider like the threshold
        // does. The threshold bottoms out at one increment halfway up the
        // range, so past that point the cooldown is the only thing the slider
        // can still move.
        let cooldown = next_binding.sensitivity.action_cooldown();
        let cooling_down =
            last_fired.is_some_and(|time| now.saturating_duration_since(time) < cooldown);
        let (output, last_fired) = if cooling_down {
            (WheelOutput::Idle, last_fired)
        } else {
            increments += magnitude;
            if increments >= next_binding.sensitivity.action_threshold() {
                increments = 0;
                (WheelOutput::FireAction, Some(now))
            } else {
                (WheelOutput::Idle, last_fired)
            }
        };
        self.state = WheelState::Action {
            binding: next_binding,
            increments,
            last_event: now,
            last_fired,
        };
        output
    }
}

/// Mutually exclusive state for one physical wheel direction.
#[derive(Default)]
enum WheelState {
    /// No binding has retained state.
    #[default]
    Idle,
    /// Continuous scrolling under one exact axis, resolution, and sensitivity.
    Scroll(ScrollContext),
    /// Increment progress and timing retained for one exact discrete binding.
    Action {
        binding: DiscreteBinding,
        increments: i32,
        last_event: Instant,
        last_fired: Option<Instant>,
    },
}

/// Identity of one continuous-scroll mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScrollContext {
    binding: ScrollBinding,
    scale: ScrollScale,
}

/// Identity of one discrete binding, including the threshold it uses.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscreteBinding {
    action: Action,
    sensitivity: ThumbwheelSensitivity,
}

/// Axis and sign encoded by a continuous-scroll action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScrollBinding {
    Up,
    Down,
    Right,
    Left,
}

impl ScrollBinding {
    fn from_action(action: &Action) -> Option<Self> {
        match action {
            Action::ScrollUp => Some(Self::Up),
            Action::ScrollDown => Some(Self::Down),
            Action::HorizontalScrollRight => Some(Self::Right),
            Action::HorizontalScrollLeft => Some(Self::Left),
            _ => None,
        }
    }

    /// Convert positive distance into the configured axis and sign.
    fn output(self, distance: f64) -> WheelOutput {
        let delta = match self {
            Self::Up => ScrollDelta::wheel_ticks(0.0, distance),
            Self::Down => ScrollDelta::wheel_ticks(0.0, -distance),
            Self::Right => ScrollDelta::wheel_ticks(distance, 0.0),
            Self::Left => ScrollDelta::wheel_ticks(-distance, 0.0),
        };
        WheelOutput::Scroll(delta)
    }
}

/// What advancing one direction should produce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum WheelOutput {
    /// Below threshold or suppressed.
    Idle,
    /// Typed fractional distance for the smooth-scroll runtime or injector.
    Scroll(ScrollDelta),
    /// Fire the direction's bound discrete action.
    FireAction,
}

/// Device-native scroll scale combined with the user's sensitivity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScrollScale {
    /// Native and diverted increments per revolution reported by the device.
    resolution: WheelResolution,
    /// User multiplier relative to the device's native amount.
    sensitivity: ThumbwheelSensitivity,
}

impl ScrollScale {
    /// Pair one captured event's device resolution with the active setting.
    pub(super) const fn new(
        resolution: WheelResolution,
        sensitivity: ThumbwheelSensitivity,
    ) -> Self {
        Self {
            resolution,
            sensitivity,
        }
    }

    /// Native scroll ticks one diverted increment contributes.
    fn per_increment(self) -> f64 {
        self.resolution.native_per_increment() * self.sensitivity.scroll_multiplier()
    }
}

#[cfg(test)]
mod tests;
