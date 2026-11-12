//! What the wheel does while the pointer rests on an Actions Ring slot.
//!
//! A slot keeps its click action and may add a [`RingScroll`] pair: each wheel
//! step over the slot runs the pair's action for that direction, and the ring
//! stays open, so one slot can sweep the volume or walk through tabs.

use serde::{Deserialize, Serialize};

use super::RingAction;
use crate::binding::Action;

/// Which way the wheel turned over a scrollable slot.
///
/// `Up` is the direction that scrolls a document towards its top, after the
/// host applied its natural-scrolling preference — the direction the user
/// already associates with "up" everywhere else.
///
/// Variants are append-only: this enum crosses the agent's IPC boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RingScrollDirection {
    /// Towards the top of a document.
    Up,
    /// Towards the bottom of a document.
    Down,
}

/// The two actions a slot runs as the wheel turns over it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingScroll {
    up: RingAction,
    down: RingAction,
}

impl RingScroll {
    /// Pair `up` with `down`. Both already satisfy the ring's own invariant.
    #[must_use]
    pub const fn new(up: RingAction, down: RingAction) -> Self {
        Self { up, down }
    }

    /// The action one wheel step in `direction` runs.
    #[must_use]
    pub fn action(&self, direction: RingScrollDirection) -> &Action {
        match direction {
            RingScrollDirection::Up => self.up.action(),
            RingScrollDirection::Down => self.down.action(),
        }
    }

    /// This pair with `direction` running `action` instead.
    #[must_use]
    pub fn with(self, direction: RingScrollDirection, action: RingAction) -> Self {
        match direction {
            RingScrollDirection::Up => Self { up: action, ..self },
            RingScrollDirection::Down => Self {
                down: action,
                ..self
            },
        }
    }

    /// The preset this pair is, if it is exactly one.
    #[must_use]
    pub fn preset(&self) -> Option<RingScrollPreset> {
        RingScrollPreset::ALL
            .into_iter()
            .find(|preset| preset.actions() == (self.up.action(), self.down.action()))
    }
}

/// Where a scrollable slot's wheel badge sits: a small disc across the slot's
/// lower-right rim. The overlay and the settings preview draw the same badge
/// at their own slot sizes, so both take its geometry from here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollBadge {
    /// The badge's diameter.
    pub diameter: f32,
    /// How far the badge reaches past the slot's right and bottom edges, as
    /// a negative inset from them.
    pub inset: f32,
    /// The size of the up-and-down chevron drawn in the badge.
    pub glyph: f32,
}

impl ScrollBadge {
    /// The badge's share of its slot's diameter: big enough to read as a
    /// mark, small enough to stay an accent on the slot's own glyph.
    const SHARE: f32 = 0.42;

    /// The badge on a slot `slot_size` across, in the caller's units, rounded
    /// to whole units so it stays crisp.
    #[must_use]
    pub fn for_slot(slot_size: f32) -> Self {
        let diameter = (slot_size * Self::SHARE).round();
        Self {
            diameter,
            inset: -(diameter / 6.0).round(),
            glyph: diameter * 0.7,
        }
    }
}

/// A ready-made [`RingScroll`] pair the settings offer by name.
///
/// The settings match a stored pair back to its preset through
/// [`RingScroll::preset`], so a hand-written pair that equals a preset shows
/// as that preset, and anything else shows as custom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RingScrollPreset {
    /// Raise and lower the system volume.
    Volume,
    /// Walk to the next and previous tab.
    Tabs,
    /// Skip to the next and previous track.
    Tracks,
}

impl RingScrollPreset {
    /// Every preset, in the order the settings list them.
    pub const ALL: [Self; 3] = [Self::Volume, Self::Tabs, Self::Tracks];

    /// The `(up, down)` actions of this preset.
    const fn actions(self) -> (&'static Action, &'static Action) {
        match self {
            Self::Volume => (&Action::VolumeUp, &Action::VolumeDown),
            // Up walks back towards the first tab, as the wheel does over a
            // browser's tab strip.
            Self::Tabs => (&Action::PrevTab, &Action::NextTab),
            Self::Tracks => (&Action::NextTrack, &Action::PrevTrack),
        }
    }

    /// The pair this preset stands for.
    ///
    /// # Panics
    ///
    /// Never: every preset action is an ordinary action, which a ring slot
    /// accepts. `every_preset_is_recognised_from_its_own_pair` builds each one.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "every preset action is statically known to satisfy RingAction's invariant"
    )]
    pub fn scroll(self) -> RingScroll {
        let (up, down) = self.actions();
        let ring = |action: &Action| {
            RingAction::new(action.clone()).expect("preset scroll actions must be valid")
        };
        RingScroll::new(ring(up), ring(down))
    }

    /// Localization key of this preset's name.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::Volume => "action_ring.scroll_volume",
            Self::Tabs => "action_ring.scroll_tabs",
            Self::Tracks => "action_ring.scroll_tracks",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_recognised_from_its_own_pair() {
        for preset in RingScrollPreset::ALL {
            assert_eq!(preset.scroll().preset(), Some(preset));
        }
    }

    #[test]
    fn a_pair_matching_no_preset_is_custom() {
        let ring = |action| RingAction::new(action).expect("valid ring action");
        let swapped = RingScroll::new(ring(Action::VolumeDown), ring(Action::VolumeUp));
        assert_eq!(swapped.preset(), None);
    }

    #[test]
    fn each_direction_runs_its_own_action() {
        let volume = RingScrollPreset::Volume.scroll();
        assert_eq!(volume.action(RingScrollDirection::Up), &Action::VolumeUp);
        assert_eq!(
            volume.action(RingScrollDirection::Down),
            &Action::VolumeDown
        );
    }

    #[test]
    fn replacing_one_direction_keeps_the_other() {
        let zoom_in = RingAction::new(Action::Copy).expect("valid ring action");
        let pair = RingScrollPreset::Volume
            .scroll()
            .with(RingScrollDirection::Up, zoom_in);
        assert_eq!(pair.action(RingScrollDirection::Up), &Action::Copy);
        assert_eq!(pair.action(RingScrollDirection::Down), &Action::VolumeDown);
        assert_eq!(pair.preset(), None);
    }

    #[test]
    fn scroll_actions_keep_the_ring_invariant() {
        let error = toml::from_str::<RingScroll>("up = \"ShowActionsRing\"\ndown = \"VolumeDown\"")
            .map(|scroll| panic!("a scroll pair must not open the ring, got {scroll:?}"))
            .unwrap_err();
        assert!(error.to_string().contains("Show Actions Ring"), "{error}");
    }

    #[test]
    fn a_pair_needs_both_directions() {
        toml::from_str::<RingScroll>("up = \"VolumeUp\"")
            .map(|scroll| panic!("a one-sided pair must not parse, got {scroll:?}"))
            .unwrap_err();
    }
}
