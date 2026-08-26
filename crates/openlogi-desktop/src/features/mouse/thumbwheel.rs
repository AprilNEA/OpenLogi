//! GUI projection of the two persisted thumb-wheel direction bindings.
//!
//! Configuration remains backward-compatible: the picker writes the existing
//! `ThumbwheelScrollDown` and `ThumbwheelScrollUp` entries. This module only
//! groups exact pairs into the presets displayed by the mouse diagram.

use openlogi_core::binding::Action;

/// Actions fired when the thumb wheel moves backward/down and forward/up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThumbwheelPair {
    pub backward: Action,
    pub forward: Action,
}

/// Paired actions exposed by the thumb-wheel picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThumbwheelPreset {
    BackForward,
    UndoRedo,
    BrowserHistory,
    Tabs,
    Desktops,
    Tracks,
    Volume,
    VolumeReversed,
    CycleDpi,
    VerticalScroll,
    VerticalScrollReversed,
    HorizontalScroll,
    HorizontalScrollReversed,
}

impl ThumbwheelPreset {
    pub(crate) const ALL: [Self; 13] = [
        Self::BackForward,
        Self::UndoRedo,
        Self::BrowserHistory,
        Self::Tabs,
        Self::Desktops,
        Self::Tracks,
        Self::Volume,
        Self::VolumeReversed,
        Self::CycleDpi,
        Self::VerticalScroll,
        Self::VerticalScrollReversed,
        Self::HorizontalScroll,
        Self::HorizontalScrollReversed,
    ];

    #[must_use]
    pub(crate) fn pair(self) -> ThumbwheelPair {
        let (backward, forward) = match self {
            Self::BackForward => (Action::MouseBack, Action::MouseForward),
            Self::UndoRedo => (Action::Undo, Action::Redo),
            Self::BrowserHistory => (Action::BrowserBack, Action::BrowserForward),
            Self::Tabs => (Action::PrevTab, Action::NextTab),
            Self::Desktops => (Action::PreviousDesktop, Action::NextDesktop),
            Self::Tracks => (Action::PrevTrack, Action::NextTrack),
            Self::Volume => (Action::VolumeDown, Action::VolumeUp),
            Self::VolumeReversed => (Action::VolumeUp, Action::VolumeDown),
            Self::CycleDpi => (Action::CycleDpiPresets, Action::CycleDpiPresets),
            Self::VerticalScroll => (Action::ScrollDown, Action::ScrollUp),
            Self::VerticalScrollReversed => (Action::ScrollUp, Action::ScrollDown),
            // The plain pair must equal the native-direction defaults so
            // picking it never diverts the wheel; the reversed pair is the
            // swap, which diverts and injects the opposite direction.
            Self::HorizontalScroll => (Action::HorizontalScrollRight, Action::HorizontalScrollLeft),
            Self::HorizontalScrollReversed => {
                (Action::HorizontalScrollLeft, Action::HorizontalScrollRight)
            }
        };
        ThumbwheelPair { backward, forward }
    }

    /// Recognize only an exact approved pair. Mixed or reversed bindings stay
    /// `Custom` in the UI until the user selects a preset.
    #[must_use]
    pub(crate) fn recognize(backward: &Action, forward: &Action) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| {
            let pair = preset.pair();
            pair.backward.eq(backward) && pair.forward.eq(forward)
        })
    }

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::BackForward => "Back / Forward",
            Self::UndoRedo => "Undo / Redo",
            Self::BrowserHistory => "Browser Back / Forward",
            Self::Tabs => "Previous / Next Tab",
            Self::Desktops => "Previous / Next Desktop",
            Self::Tracks => "Previous / Next Track",
            Self::Volume => "Volume Down / Up",
            Self::VolumeReversed => "Volume Up / Down",
            Self::CycleDpi => "Cycle DPI Presets",
            Self::VerticalScroll => "Vertical Scroll",
            Self::VerticalScrollReversed => "Vertical Scroll (Reversed)",
            Self::HorizontalScroll => "Horizontal Scroll",
            Self::HorizontalScrollReversed => "Horizontal Scroll (Reversed)",
        }
    }

    #[must_use]
    pub(crate) const fn icon(self) -> &'static str {
        match self {
            Self::BackForward => "action-icons/circle-arrow-right.svg",
            Self::UndoRedo => "action-icons/redo-2.svg",
            Self::BrowserHistory => "action-icons/arrow-right.svg",
            Self::Tabs => "action-icons/chevron-right.svg",
            Self::Desktops => "action-icons/square-arrow-right.svg",
            Self::Tracks => "action-icons/skip-forward.svg",
            Self::Volume | Self::VolumeReversed => "action-icons/volume-2.svg",
            Self::CycleDpi => "action-icons/gauge.svg",
            Self::VerticalScroll | Self::VerticalScrollReversed => "action-icons/chevrons-up.svg",
            Self::HorizontalScroll | Self::HorizontalScrollReversed => {
                "action-icons/chevrons-right.svg"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_map_backward_and_forward_in_physical_direction() {
        let expected = [
            (Action::MouseBack, Action::MouseForward),
            (Action::Undo, Action::Redo),
            (Action::BrowserBack, Action::BrowserForward),
            (Action::PrevTab, Action::NextTab),
            (Action::PreviousDesktop, Action::NextDesktop),
            (Action::PrevTrack, Action::NextTrack),
            (Action::VolumeDown, Action::VolumeUp),
            (Action::VolumeUp, Action::VolumeDown),
            (Action::CycleDpiPresets, Action::CycleDpiPresets),
            (Action::ScrollDown, Action::ScrollUp),
            (Action::ScrollUp, Action::ScrollDown),
            (Action::HorizontalScrollRight, Action::HorizontalScrollLeft),
            (Action::HorizontalScrollLeft, Action::HorizontalScrollRight),
        ];

        for (preset, (backward, forward)) in ThumbwheelPreset::ALL.into_iter().zip(expected) {
            assert_eq!(preset.pair(), ThumbwheelPair { backward, forward });
        }
    }

    #[test]
    fn recognition_requires_an_exact_approved_pair() {
        for preset in ThumbwheelPreset::ALL {
            let pair = preset.pair();
            assert_eq!(
                ThumbwheelPreset::recognize(&pair.backward, &pair.forward),
                Some(preset)
            );
        }

        assert_eq!(
            ThumbwheelPreset::recognize(&Action::NextTab, &Action::PrevTab),
            None,
            "reversed directions are custom"
        );
        assert_eq!(
            ThumbwheelPreset::recognize(&Action::VolumeDown, &Action::NextTrack),
            None,
            "mixed actions are custom"
        );
    }

    #[test]
    fn cycle_dpi_uses_the_same_action_in_both_directions() {
        assert_eq!(
            ThumbwheelPreset::CycleDpi.pair(),
            ThumbwheelPair {
                backward: Action::CycleDpiPresets,
                forward: Action::CycleDpiPresets,
            }
        );
    }
}
