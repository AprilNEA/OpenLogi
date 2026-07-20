//! Single-action vs per-direction gesture bindings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::action::Action;
use super::defaults::default_gesture_binding;
use super::gesture::GestureDirection;

/// What a single rebindable [`ButtonId`] does: either one [`Action`], or — for a
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
/// for the payload variants), and [`Gesture`](Binding::Gesture) serializes as a
/// table keyed by [`GestureDirection`] names (`Up`/`Down`/`Left`/`Right`/
/// `Click`).
///
/// The two arms are disambiguated by the **zero overlap** between [`Action`]
/// variant names and [`GestureDirection`] variant names — untagged tries
/// `Single(Action)` first, and a table keyed by `Up` etc. cannot parse as an
/// externally-tagged `Action`, so it falls through to `Gesture`. A payload
/// action like `{ SetDpiPreset = 2 }` is a valid externally-tagged `Action`, so
/// it stays `Single` and never reaches the `Gesture` arm. This invariant is the
/// entire safety basis for untagged routing; the `binding_untagged_*` tests
/// guard it (a future `Action` named `Up`/`Down`/`Left`/`Right`/`Click` would
/// silently mis-route, and those tests would fail).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    /// One action, fired on press. The shape every non-gesture button uses.
    Single(Action),
    /// Per-direction sub-bindings for a button in gesture mode. Keyed by the
    /// committed swipe direction, with [`GestureDirection::Click`] holding the
    /// plain-click (no-swipe) action.
    Gesture(BTreeMap<GestureDirection, Action>),
}

impl Binding {
    /// The plain-click action for this binding: the [`Single`](Binding::Single)
    /// action, or the [`Gesture`](Binding::Gesture) map's
    /// [`Click`](GestureDirection::Click) entry. Falls back to [`Action::None`]
    /// when a gesture binding has no explicit `Click`.
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
        }
    }

    /// The action bound to `direction`, if this is a gesture binding.
    /// [`Single`](Binding::Single) has no directions and returns `None`.
    #[must_use]
    pub fn direction_action(&self, direction: GestureDirection) -> Option<&Action> {
        match self {
            Binding::Single(_) => None,
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
    /// A no-op when this is already a [`Gesture`](Binding::Gesture).
    pub fn upgrade_to_gesture(&mut self) {
        if let Binding::Single(action) = self {
            let mut map = BTreeMap::new();
            map.insert(GestureDirection::Click, action.clone());
            *self = Binding::Gesture(map);
        }
    }

    /// Demote a [`Gesture`](Binding::Gesture) binding in place to a
    /// [`Single`](Binding::Single) of its [`Click`](GestureDirection::Click)
    /// entry, falling back to `fallback` when the map has no explicit `Click` —
    /// the inverse of [`Self::upgrade_to_gesture`]. A no-op on a
    /// [`Single`](Binding::Single).
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
