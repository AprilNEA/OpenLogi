//! Thumb-wheel binding state for one captured session's input dispatcher.
//!
//! Ordinary bindings own independent state per physical direction. Non-Mac Zoom
//! shares signed progress across both directions. Continuous scrolling and
//! discrete actions are mutually exclusive; suppressed input clears either. Changing the effective
//! binding or sensitivity discards progress and cooldown from the previous
//! configuration.

mod zoom;

#[cfg(any(target_os = "macos", test))]
pub(super) use zoom::speed as zoom_speed;

use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::thumbwheel::WheelResolution;

/// Idle gap after which a partly accumulated discrete action is forgotten, so
/// slow intermittent nudges do not eventually cross the threshold.
const ACTION_DECAY: Duration = Duration::from_millis(300);

/// Minimum gap between two fires of the same discrete action, so one deliberate
/// flick triggers once instead of repeating across a fast spin.
const ACTION_COOLDOWN: Duration = Duration::from_millis(200);

/// Bound synchronous action dispatch for one captured event.
const MAX_REPEATS_PER_EVENT: i32 = 20;

/// Ordinary binding progress is per direction; non-Mac Zoom progress is signed
/// and shared so physical reversals cancel outstanding Zoom displacement.
#[derive(Default)]
pub(super) struct WheelAccumulators {
    up: WheelDirection,
    down: WheelDirection,
    #[cfg(any(not(target_os = "macos"), test))]
    zoom: zoom::Response,
}

impl WheelAccumulators {
    /// Admit only progress after raw phase repair and deferred-end handling.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(super) fn prepare_zoom_report(
        &mut self,
        status: openlogi_hid::thumbwheel::RotationStatus,
        increments: i16,
        now: Instant,
    ) -> bool {
        self.zoom.prepare(status, increments, now)
    }

    /// Advance the state belonging to `rotation`'s physical direction.
    pub(super) fn advance(
        &mut self,
        rotation: WheelRotation,
        action: &Action,
        scale: ScrollScale,
        now: Instant,
    ) -> WheelOutput {
        #[cfg(not(target_os = "macos"))]
        if matches!(action, Action::ZoomIn | Action::ZoomOut) {
            return self.advance_zoom(rotation, scale.sensitivity, now);
        }
        #[cfg(any(not(target_os = "macos"), test))]
        self.clear_zoom();
        let direction = match rotation.direction {
            PhysicalDirection::Up => &mut self.up,
            PhysicalDirection::Down => &mut self.down,
        };
        direction.advance(action, rotation.magnitude, scale, now)
    }

    /// Share physical signed displacement without deriving output from its sign.
    /// The caller fires the action configured for the report's direction.
    #[cfg(any(not(target_os = "macos"), test))]
    fn advance_zoom(
        &mut self,
        rotation: WheelRotation,
        sensitivity: ThumbwheelSensitivity,
        now: Instant,
    ) -> WheelOutput {
        // Crossing to Zoom also ends ordinary binding progress for this direction.
        match rotation.direction {
            PhysicalDirection::Up => self.up.state = WheelState::Idle,
            PhysicalDirection::Down => self.down.state = WheelState::Idle,
        }
        let increments = match rotation.direction {
            PhysicalDirection::Up => rotation.magnitude,
            PhysicalDirection::Down => -rotation.magnitude,
        };
        if self.zoom.advance(increments, sensitivity, now) {
            WheelOutput::FireAction(1)
        } else {
            WheelOutput::Idle
        }
    }

    /// Discard recognition and displacement at a lifecycle boundary.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(super) fn clear_zoom(&mut self) {
        self.zoom = zoom::Response::default();
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

        let threshold = next_binding.sensitivity.action_threshold();
        let (output, last_fired) = if is_repeatable(action) {
            // Volume actions track accumulated distance without the discrete-action cooldown.
            increments += magnitude;
            // Bound synchronous work even when one report spans many thresholds.
            let repeats = (increments / threshold).min(MAX_REPEATS_PER_EVENT);
            // Discard whole thresholds above the cap; retain sub-threshold progress.
            increments %= threshold;
            if repeats > 0 {
                (WheelOutput::FireAction(repeats.cast_unsigned()), Some(now))
            } else {
                (WheelOutput::Idle, last_fired)
            }
        } else {
            let cooling_down = last_fired
                .is_some_and(|time| now.saturating_duration_since(time) < ACTION_COOLDOWN);
            if cooling_down {
                (WheelOutput::Idle, last_fired)
            } else {
                increments += magnitude;
                if increments >= threshold {
                    increments = 0;
                    (WheelOutput::FireAction(1), Some(now))
                } else {
                    (WheelOutput::Idle, last_fired)
                }
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

/// Only volume actions repeat with thumb-wheel distance.
fn is_repeatable(action: &Action) -> bool {
    matches!(action, Action::VolumeUp | Action::VolumeDown)
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
    /// Fire the bound action this many times; only volume actions may repeat.
    FireAction(u32),
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
