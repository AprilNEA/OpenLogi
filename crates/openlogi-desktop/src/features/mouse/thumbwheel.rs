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
    HorizontalScroll,
    Zoom,
    ZoomReversed,
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
        Self::HorizontalScroll,
        Self::Zoom,
        Self::ZoomReversed,
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
            Self::HorizontalScroll => (Action::HorizontalScrollLeft, Action::HorizontalScrollRight),
            Self::Zoom => (Action::ZoomOut, Action::ZoomIn),
            Self::ZoomReversed => (Action::ZoomIn, Action::ZoomOut),
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

    /// Localized display label, rendered as "<backward> / <forward>" composed
    /// from each side's already-translated action name. The flat "X / Y"
    /// strings are keys in no locale file — every preset row used to fall back
    /// to English regardless of app language — while per-action names carry
    /// translations for all locales, so composing reuses that reviewed
    /// coverage instead of duplicating it. CycleDpi fires one action in both
    /// directions and renders as a single name without the separator.
    #[must_use]
    pub(crate) fn label(self) -> String {
        let pair = self.pair();
        if pair.backward == pair.forward {
            return rust_i18n::t!(pair.backward.label()).into_owned();
        }
        format!(
            "{} / {}",
            rust_i18n::t!(pair.backward.label()),
            rust_i18n::t!(pair.forward.label())
        )
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
            Self::VerticalScroll => "action-icons/chevrons-up.svg",
            Self::HorizontalScroll => "action-icons/chevrons-right.svg",
            // One glyph per feature — the row label carries the direction,
            // same convention as the two volume rows sharing volume-2.
            Self::Zoom | Self::ZoomReversed => "action-icons/zoom-in.svg",
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
            (Action::HorizontalScrollLeft, Action::HorizontalScrollRight),
            (Action::ZoomOut, Action::ZoomIn),
            (Action::ZoomIn, Action::ZoomOut),
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
