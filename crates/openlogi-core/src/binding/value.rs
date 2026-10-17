//! Single-action, long-press, wheel-hold and per-direction gesture bindings.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::action::Action;
use super::defaults::default_gesture_binding;
use super::gesture::GestureDirection;

/// How long a physical button must remain down before its independent long
/// action fires.
pub const LONG_PRESS_THRESHOLD: Duration = Duration::from_millis(500);

/// The mutually exclusive actions of a threshold-based button binding.
///
/// `short` fires only on an ordinary release before the threshold. `long`
/// fires once when the threshold elapses and suppresses `short` for that press.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongPressBinding {
    short: Action,
    long: Action,
}

impl LongPressBinding {
    /// Pair the release-before-threshold action with the threshold action.
    #[must_use]
    pub const fn new(short: Action, long: Action) -> Self {
        Self { short, long }
    }

    /// Action fired by a normal release before the threshold.
    #[must_use]
    pub const fn short(&self) -> &Action {
        &self.short
    }

    /// Action fired once when the threshold is reached.
    #[must_use]
    pub const fn long(&self) -> &Action {
        &self.long
    }
}

/// What the main scroll wheel does while a [`WheelHoldBinding`] button is
/// held.
///
/// Variant identifiers are TOML-stable: renames are migration events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HeldWheel {
    /// Scroll horizontally, as a thumb wheel does by default: turning the
    /// wheel away from the user scrolls left, toward the user scrolls right.
    HorizontalScroll,
}

/// A button that turns the main scroll wheel into a virtual thumb wheel while
/// it is held, and runs its own action when clicked without the wheel moving.
/// Logi Options+ calls this the virtual thumbwheel. It needs the button to be
/// diverted with HID++ `0x1b04` raw-wheel reporting, which the firmware
/// offers only on some controls.
///
/// `click` fires on release, and only when the wheel did not move while the
/// button was down: a press that scrolled fires nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WheelHoldBinding {
    click: Action,
    wheel: HeldWheel,
}

impl WheelHoldBinding {
    /// Scroll horizontally while held, and run `click` on a plain click.
    #[must_use]
    pub const fn horizontal_scroll(click: Action) -> Self {
        Self {
            click,
            wheel: HeldWheel::HorizontalScroll,
        }
    }

    /// Action fired by a release when the wheel did not move during the press.
    #[must_use]
    pub const fn click(&self) -> &Action {
        &self.click
    }

    /// What the wheel does while the button is held.
    #[must_use]
    pub const fn wheel(&self) -> HeldWheel {
        self.wheel
    }
}

/// What a single rebindable [`ButtonId`](crate::binding::ButtonId) does: one
/// immediate [`Action`], an independent short/long action pair, a wheel-hold
/// click action, or — for a raw-XY-capable button placed in gesture mode — a
/// per-[`GestureDirection`] map (hold + swipe up/down/left/right, or a plain
/// click).
///
/// There has only ever been one binding map per device; a gesture binding is
/// just a binding whose payload is a direction map instead of a single action.
///
/// # Serialization
///
/// `#[serde(untagged)]`: [`Single`](Binding::Single) serializes exactly as the
/// bare [`Action`] did before (a string `"BrowserBack"`, or a single-key table
/// for the payload variants), [`Gesture`](Binding::Gesture) serializes as a
/// table keyed by [`GestureDirection`] names (`Up`/`Down`/`Left`/`Right`/
/// `Click`), [`LongPress`](Binding::LongPress) as the structurally distinct
/// `{ short = ..., long = ... }` table, and [`WheelHold`](Binding::WheelHold)
/// as `{ click = ..., wheel = "HorizontalScroll" }`.
///
/// The arms are disambiguated structurally: action variant names and gesture
/// direction names have zero overlap, while a long press requires both
/// lowercase `short` and `long` fields, a wheel hold both lowercase `click`
/// and `wheel` fields, and each rejects unknown fields. The
/// `binding_untagged_*` tests guard these routing invariants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    /// One action, fired on press. The shape every non-gesture button uses.
    Single(Action),
    /// Per-direction sub-bindings for a button in gesture mode. Keyed by the
    /// committed swipe direction, with [`GestureDirection::Click`] holding the
    /// plain-click (no-swipe) action.
    Gesture(BTreeMap<GestureDirection, Action>),
    /// Independent release-before-threshold and threshold actions.
    LongPress(LongPressBinding),
    /// The main wheel scrolls horizontally while the button is held; a click
    /// without wheel movement runs the click action.
    WheelHold(WheelHoldBinding),
}

impl Binding {
    /// The plain-click action for this binding: the [`Single`](Binding::Single)
    /// action, the [`Gesture`](Binding::Gesture) map's
    /// [`Click`](GestureDirection::Click) entry, a
    /// [`LongPress`](Binding::LongPress) binding's short action, or a
    /// [`WheelHold`](Binding::WheelHold) binding's click action. Falls back to
    /// [`Action::None`] when a gesture binding has no explicit `Click`.
    ///
    /// Lets the click-dispatch path stay binding-shape-agnostic.
    #[must_use]
    pub fn click_action(&self) -> Action {
        match self {
            Binding::Single(action) => action.clone(),
            Binding::Gesture(map) => map
                .get(&GestureDirection::Click)
                .cloned()
                .unwrap_or(Action::None),
            Binding::LongPress(binding) => binding.short().clone(),
            Binding::WheelHold(binding) => binding.click().clone(),
        }
    }

    /// The action bound to `direction`, if this is a gesture binding.
    /// The other shapes have no directions and return `None`.
    #[must_use]
    pub fn direction_action(&self, direction: GestureDirection) -> Option<&Action> {
        match self {
            Binding::Single(_) | Binding::LongPress(_) | Binding::WheelHold(_) => None,
            Binding::Gesture(map) => map.get(&direction),
        }
    }

    /// Whether this binding drives raw-XY swipe capture (the
    /// [`Gesture`](Binding::Gesture) arm).
    #[must_use]
    pub fn is_gesture(&self) -> bool {
        matches!(self, Binding::Gesture(_))
    }

    /// Whether this binding needs the button diverted with raw-wheel
    /// reporting (the [`WheelHold`](Binding::WheelHold) arm).
    #[must_use]
    pub fn is_wheel_hold(&self) -> bool {
        matches!(self, Binding::WheelHold(_))
    }

    /// Promote a [`Single`](Binding::Single) binding in place to a
    /// [`Gesture`](Binding::Gesture), keeping its action as the
    /// [`GestureDirection::Click`] entry and leaving the swipe arms unbound.
    /// A long-press binding keeps its short action as `Click`; its long action
    /// is discarded because gesture and threshold modes are mutually exclusive.
    /// A wheel-hold binding keeps its click action the same way.
    /// A no-op when this is already a [`Gesture`](Binding::Gesture).
    pub fn upgrade_to_gesture(&mut self) {
        let click = match self {
            Binding::Single(action) => action.clone(),
            Binding::LongPress(binding) => binding.short().clone(),
            Binding::WheelHold(binding) => binding.click().clone(),
            Binding::Gesture(_) => return,
        };
        *self = Binding::Gesture(BTreeMap::from([(GestureDirection::Click, click)]));
    }

    /// Demote a [`Gesture`](Binding::Gesture) binding in place to a
    /// [`Single`](Binding::Single) of its [`Click`](GestureDirection::Click)
    /// entry, falling back to `fallback` when the map has no explicit `Click` —
    /// the inverse of [`Self::upgrade_to_gesture`]. A no-op on the other
    /// shapes.
    pub fn demote_to_single(&mut self, fallback: Action) {
        if let Binding::Gesture(map) = self {
            let click = map
                .get(&GestureDirection::Click)
                .cloned()
                .unwrap_or(fallback);
            *self = Binding::Single(click);
        }
    }

    /// Fill any unbound directions of a [`Gesture`](Binding::Gesture) binding
    /// with their canonical [`default_gesture_binding`], so a button promoted to
    /// the gesture role always exposes the full five-direction set — rather than
    /// leaving swipe arms the GUI renders as defaults but the runtime never
    /// dispatches. A no-op on [`Single`](Binding::Single) and on directions
    /// already bound (existing user choices are preserved).
    pub fn fill_gesture_defaults(&mut self) {
        if let Binding::Gesture(map) = self {
            for dir in GestureDirection::ALL {
                map.entry(dir)
                    .or_insert_with(|| default_gesture_binding(dir));
            }
        }
    }
}

impl From<Action> for Binding {
    fn from(action: Action) -> Self {
        Binding::Single(action)
    }
}
