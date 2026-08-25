//! Single-action vs per-direction gesture bindings.

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

/// What a single rebindable [`ButtonId`](crate::binding::ButtonId) does: one
/// immediate [`Action`], an independent short/long action pair, or — for a
/// raw-XY-capable button placed in gesture mode — a per-[`GestureDirection`]
/// map (hold + swipe up/down/left/right, or a plain click).
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
/// `Click`), and [`LongPress`](Binding::LongPress) as the structurally distinct
/// `{ short = ..., long = ... }` table.
///
/// The arms are disambiguated structurally: action variant names and gesture
/// direction names have zero overlap, while a long press requires both
/// lowercase `short` and `long` fields and rejects unknown fields. The
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
}

impl Binding {
    /// The plain-click action for this binding: the [`Single`](Binding::Single)
    /// action, the [`Gesture`](Binding::Gesture) map's
    /// [`Click`](GestureDirection::Click) entry, or a
    /// [`LongPress`](Binding::LongPress) binding's short action. Falls back to
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
        }
    }

    /// The action bound to `direction`, if this is a gesture binding.
    /// [`Single`](Binding::Single) has no directions and returns `None`.
    #[must_use]
    pub fn direction_action(&self, direction: GestureDirection) -> Option<&Action> {
        match self {
            Binding::Single(_) | Binding::LongPress(_) => None,
            Binding::Gesture(map) => map.get(&direction),
        }
    }

    /// Whether this binding drives raw-XY swipe capture (the
    /// [`Gesture`](Binding::Gesture) arm).
    #[must_use]
    pub fn is_gesture(&self) -> bool {
        matches!(self, Binding::Gesture(_))
    }

    /// Promote a [`Single`](Binding::Single) binding in place to a
    /// [`Gesture`](Binding::Gesture), keeping its action as the
    /// [`GestureDirection::Click`] entry and leaving the swipe arms unbound.
    /// A long-press binding keeps its short action as `Click`; its long action
    /// is discarded because gesture and threshold modes are mutually exclusive.
    /// A no-op when this is already a [`Gesture`](Binding::Gesture).
    pub fn upgrade_to_gesture(&mut self) {
        let click = match self {
            Binding::Single(action) => action.clone(),
            Binding::LongPress(binding) => binding.short().clone(),
            Binding::Gesture(_) => return,
        };
        *self = Binding::Gesture(BTreeMap::from([(GestureDirection::Click, click)]));
    }

    /// Demote a [`Gesture`](Binding::Gesture) binding in place to a
    /// [`Single`](Binding::Single) of its [`Click`](GestureDirection::Click)
    /// entry, falling back to `fallback` when the map has no explicit `Click` —
    /// the inverse of [`Self::upgrade_to_gesture`]. A no-op on a
    /// [`Single`](Binding::Single) or [`LongPress`](Binding::LongPress).
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
