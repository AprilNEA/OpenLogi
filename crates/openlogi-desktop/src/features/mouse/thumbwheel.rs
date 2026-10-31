//! GUI projection of the two persisted thumb-wheel direction bindings.
//!
//! Configuration remains backward-compatible: the picker writes the existing
//! `ThumbwheelScrollDown` and `ThumbwheelScrollUp` entries. This module only
//! groups exact pairs into the presets displayed by the mouse diagram.
//!
//! It also presents the per-application horizontal scroll methods of the
//! `[thumbwheel_apps]` table, with the built-in suggestions offered for them.

use openlogi_core::app::{executable_selector, profile_applies_to};
use openlogi_core::binding::Action;
use openlogi_core::config::Config;
use openlogi_core::scroll::ScrollDelivery;

/// Whether the agent delivers horizontal scrolling by method on this platform.
/// The other backends ignore `[thumbwheel_apps]` and keep the native wheel, so
/// no picker is offered there.
pub(crate) const SCROLL_METHODS_SUPPORTED: bool = cfg!(target_os = "windows");

/// A horizontal scroll method the inspector offers for one application. Only
/// the user's click applies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollMethodSuggestion {
    pub delivery: ScrollDelivery,
    /// Reports disagree on whether the method works in every version, so the
    /// inspector asks the user to check it.
    pub needs_verification: bool,
}

/// Applications known to ignore the native horizontal wheel, by the selector
/// [`scroll_method_selector`] derives.
const SUGGESTED_SCROLL_METHODS: &[(&str, ScrollMethodSuggestion)] = &[
    (
        "exe:excel.exe",
        ScrollMethodSuggestion {
            delivery: ScrollDelivery::CtrlShiftWheel,
            needs_verification: true,
        },
    ),
    (
        "exe:adobe premiere pro.exe",
        ScrollMethodSuggestion {
            delivery: ScrollDelivery::VerticalWheel,
            needs_verification: false,
        },
    ),
    (
        "exe:javaw.exe",
        ScrollMethodSuggestion {
            delivery: ScrollDelivery::ShiftWheel,
            needs_verification: false,
        },
    ),
    (
        "exe:java.exe",
        ScrollMethodSuggestion {
            delivery: ScrollDelivery::ShiftWheel,
            needs_verification: false,
        },
    ),
];

/// The open per-app profile's horizontal scroll method, as the inspector
/// presents it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollMethodChoice {
    pub current: ScrollDelivery,
    pub suggestion: Option<ScrollMethodSuggestion>,
}

/// The `[thumbwheel_apps]` key the GUI writes for `app`: its `exe:` selector,
/// which survives versioned install directories. An identifier that names no
/// executable is already a selector and keys its own rule.
pub(crate) fn scroll_method_selector(app: &str) -> String {
    executable_selector(app).unwrap_or_else(|| app.to_string())
}

/// Runtime identifiers represented by an editor key. An `exe:` key also
/// covers known install paths, whose exact rules or aliases can outrank it.
/// Portable aliases expand only to this platform's identifiers.
pub(crate) fn scroll_method_apps<'a>(config: &'a Config, app: &'a str) -> Vec<&'a str> {
    let identifiers = config.apps.for_profile_key(app).map_or_else(
        || vec![app],
        |(_, alias)| {
            alias
                .identifiers_here()
                .iter()
                .map(String::as_str)
                .collect()
        },
    );
    identifiers
        .into_iter()
        .flat_map(|identifier| {
            config
                .thumbwheel_apps
                .keys()
                .map(String::as_str)
                .chain(
                    config
                        .apps
                        .iter()
                        .flat_map(|(_, alias)| alias.identifiers_here().iter().map(String::as_str)),
                )
                .filter(move |candidate| {
                    *candidate != identifier
                        && executable_selector(candidate).is_some()
                        && profile_applies_to(identifier, &config.apps, candidate)
                })
                .chain(std::iter::once(identifier))
        })
        .collect()
}

/// Effective non-native delivery for an editor key, including aliases and
/// known install paths. Concrete identifiers use the agent's matcher directly.
pub(crate) fn scroll_method_delivery(config: &Config, app: &str) -> ScrollDelivery {
    scroll_method_apps(config, app)
        .into_iter()
        .map(|app| config.scroll_delivery(Some(app)))
        .find(|delivery| !delivery.is_native())
        .unwrap_or_default()
}

/// Whether an editor key represents an application the rule can apply to.
/// Effective delivery still comes from the agent's precedence matcher.
pub(crate) fn scroll_rule_applies(app: &str, rule: &str, config: &Config) -> bool {
    scroll_method_apps(config, app)
        .into_iter()
        .any(|app| profile_applies_to(rule, &config.apps, app))
}

/// The built-in method suggested for `app`, if it is a known application.
pub(crate) fn suggested_scroll_method(app: &str) -> Option<ScrollMethodSuggestion> {
    let selector = scroll_method_selector(app);
    SUGGESTED_SCROLL_METHODS
        .iter()
        .find(|(known, _)| *known == selector)
        .map(|(_, suggestion)| *suggestion)
}

#[must_use]
pub(crate) const fn scroll_method_translation_key(delivery: ScrollDelivery) -> &'static str {
    match delivery {
        ScrollDelivery::Native => "pointer.scroll_method_native",
        ScrollDelivery::ShiftWheel => "pointer.scroll_method_shift_wheel",
        ScrollDelivery::CtrlShiftWheel => "pointer.scroll_method_ctrl_shift_wheel",
        ScrollDelivery::AltWheel => "pointer.scroll_method_alt_wheel",
        ScrollDelivery::VerticalWheel => "pointer.scroll_method_vertical_wheel",
        ScrollDelivery::CtrlWheel => "pointer.scroll_method_ctrl_wheel",
    }
}

#[must_use]
pub(crate) const fn scroll_method_description_key(delivery: ScrollDelivery) -> &'static str {
    match delivery {
        ScrollDelivery::Native => "pointer.scroll_method_native_description",
        ScrollDelivery::ShiftWheel => "pointer.scroll_method_shift_wheel_description",
        ScrollDelivery::CtrlShiftWheel => "pointer.scroll_method_ctrl_shift_wheel_description",
        ScrollDelivery::AltWheel => "pointer.scroll_method_alt_wheel_description",
        ScrollDelivery::VerticalWheel => "pointer.scroll_method_vertical_wheel_description",
        ScrollDelivery::CtrlWheel => "pointer.scroll_method_ctrl_wheel_description",
    }
}

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
    DesktopsReversed,
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
    pub(crate) const ALL: [Self; 14] = [
        Self::BackForward,
        Self::UndoRedo,
        Self::BrowserHistory,
        Self::Tabs,
        Self::Desktops,
        Self::DesktopsReversed,
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
            Self::DesktopsReversed => (Action::NextDesktop, Action::PreviousDesktop),
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
    pub(crate) const fn translation_key(self) -> &'static str {
        match self {
            Self::BackForward => "pointer.back_forward",
            Self::UndoRedo => "pointer.undo_redo",
            Self::BrowserHistory => "pointer.browser_back_forward",
            Self::Tabs => "pointer.previous_next_tab",
            Self::Desktops => "pointer.previous_next_desktop",
            Self::DesktopsReversed => "pointer.next_previous_desktop",
            Self::Tracks => "pointer.previous_next_track",
            // `Self::Volume` is the intuitive pairing (rolling the wheel up
            // raises the volume — see `pair()` above), so it takes the label a
            // user reads that way, "Volume Up / Down"; `VolumeReversed` gets
            // the other wording, matching the actual inverted direction it
            // configures.
            Self::Volume => "pointer.volume_up_down",
            Self::VolumeReversed => "pointer.volume_down_up",
            Self::CycleDpi => "pointer.cycle_dpi_presets",
            Self::VerticalScroll => "pointer.vertical_scroll",
            Self::VerticalScrollReversed => "pointer.vertical_scroll_reversed",
            Self::HorizontalScroll => "pointer.horizontal_scroll",
            Self::HorizontalScrollReversed => "pointer.horizontal_scroll_reversed",
        }
    }

    #[must_use]
    pub(crate) const fn icon(self) -> &'static str {
        match self {
            Self::BackForward => "action-icons/circle-arrow-right.svg",
            Self::UndoRedo => "action-icons/redo-2.svg",
            Self::BrowserHistory => "action-icons/arrow-right.svg",
            Self::Tabs => "action-icons/chevron-right.svg",
            Self::Desktops | Self::DesktopsReversed => "action-icons/square-arrow-right.svg",
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
    use openlogi_core::binding::{ButtonId, default_binding};

    use super::*;

    #[test]
    fn presets_map_backward_and_forward_in_physical_direction() {
        let expected = [
            (Action::MouseBack, Action::MouseForward),
            (Action::Undo, Action::Redo),
            (Action::BrowserBack, Action::BrowserForward),
            (Action::PrevTab, Action::NextTab),
            (Action::PreviousDesktop, Action::NextDesktop),
            (Action::NextDesktop, Action::PreviousDesktop),
            (Action::PrevTrack, Action::NextTrack),
            (Action::VolumeDown, Action::VolumeUp),
            (Action::VolumeUp, Action::VolumeDown),
            (Action::CycleDpiPresets, Action::CycleDpiPresets),
            (Action::ScrollDown, Action::ScrollUp),
            (Action::ScrollUp, Action::ScrollDown),
            (Action::HorizontalScrollRight, Action::HorizontalScrollLeft),
            (Action::HorizontalScrollLeft, Action::HorizontalScrollRight),
        ];

        assert_eq!(expected.len(), ThumbwheelPreset::ALL.len());
        for (preset, (backward, forward)) in ThumbwheelPreset::ALL.into_iter().zip(expected) {
            assert_eq!(preset.pair(), ThumbwheelPair { backward, forward });
        }
    }

    #[test]
    fn horizontal_scroll_presets_follow_native_defaults() {
        let native = ThumbwheelPreset::HorizontalScroll.pair();
        assert_eq!(
            native.backward,
            default_binding(ButtonId::ThumbwheelScrollDown),
            "backward must stay the native down default so the plain preset does not divert"
        );
        assert_eq!(
            native.forward,
            default_binding(ButtonId::ThumbwheelScrollUp),
            "forward must stay the native up default so the plain preset does not divert"
        );

        let reversed = ThumbwheelPreset::HorizontalScrollReversed.pair();
        assert_eq!(reversed.backward, native.forward);
        assert_eq!(reversed.forward, native.backward);
        assert_eq!(
            ThumbwheelPreset::recognize(&reversed.backward, &reversed.forward),
            Some(ThumbwheelPreset::HorizontalScrollReversed)
        );
    }

    #[test]
    fn desktop_preset_labels_match_the_direction_they_actually_configure() {
        // Rolling the wheel down fires `backward` and up fires `forward`; the
        // label names the down action first, as the plain preset does.
        for (preset, key) in [
            (ThumbwheelPreset::Desktops, "pointer.previous_next_desktop"),
            (
                ThumbwheelPreset::DesktopsReversed,
                "pointer.next_previous_desktop",
            ),
        ] {
            let pair = preset.pair();
            assert_eq!(preset.translation_key(), key);
            assert_eq!(
                pair.backward == Action::PreviousDesktop,
                key == "pointer.previous_next_desktop",
                "{preset:?}: label must name the down action first"
            );
        }
        let plain = ThumbwheelPreset::Desktops.pair();
        let reversed = ThumbwheelPreset::DesktopsReversed.pair();
        assert_eq!(reversed.backward, plain.forward);
        assert_eq!(reversed.forward, plain.backward);
    }

    #[test]
    fn volume_preset_labels_match_the_direction_they_actually_configure() {
        // The label must read the way the wheel actually behaves — whichever
        // preset's `forward` (rolling the wheel up) raises the volume is the
        // one a user expects "Up" to come first for.
        for preset in [ThumbwheelPreset::Volume, ThumbwheelPreset::VolumeReversed] {
            let pair = preset.pair();
            let reads_up_first = preset.translation_key() == "pointer.volume_up_down";
            assert_eq!(
                pair.forward == Action::VolumeUp,
                reads_up_first,
                "{preset:?}: label must say \"Up\" first exactly when rolling up raises the volume"
            );
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
    fn scroll_method_rules_are_keyed_by_executable_name() {
        assert_eq!(
            scroll_method_selector(r"c:\program files\microsoft office\root\office16\excel.exe"),
            "exe:excel.exe"
        );
        // A hand-authored profile key is already a selector.
        assert_eq!(scroll_method_selector("exe:excel.exe"), "exe:excel.exe");
    }

    #[test]
    fn a_rule_applies_under_its_exact_key_or_the_executable_selector() {
        let config = Config::ephemeral();
        let excel = r"c:\program files\microsoft office\root\office16\excel.exe";
        assert!(scroll_rule_applies(excel, "exe:excel.exe", &config));
        assert!(scroll_rule_applies(excel, excel, &config));
        assert!(scroll_rule_applies(
            "exe:excel.exe",
            "exe:excel.exe",
            &config
        ));
        assert!(
            !scroll_rule_applies("exe:excel.exe", excel, &config),
            "a selector names no install path"
        );
        assert!(!scroll_rule_applies(excel, "exe:winword.exe", &config));
    }

    #[test]
    fn suggestions_follow_the_executable_wherever_it_is_installed() {
        assert_eq!(
            suggested_scroll_method(r"C:\Program Files\Microsoft Office\root\Office16\EXCEL.EXE"),
            Some(ScrollMethodSuggestion {
                delivery: ScrollDelivery::CtrlShiftWheel,
                needs_verification: true,
            }),
            "Excel's method is offered, flagged for the user to verify"
        );
        for (app, delivery) in [
            (
                r"c:\program files\adobe\adobe premiere pro 2025\adobe premiere pro.exe",
                ScrollDelivery::VerticalWheel,
            ),
            (
                r"c:\program files\java\jdk-21\bin\javaw.exe",
                ScrollDelivery::ShiftWheel,
            ),
            (
                r"c:\program files\java\jdk-21\bin\java.exe",
                ScrollDelivery::ShiftWheel,
            ),
            ("exe:javaw.exe", ScrollDelivery::ShiftWheel),
        ] {
            assert_eq!(
                suggested_scroll_method(app),
                Some(ScrollMethodSuggestion {
                    delivery,
                    needs_verification: false,
                }),
                "{app}"
            );
        }
    }

    #[test]
    fn unknown_applications_and_lookalikes_get_no_suggestion() {
        for app in [
            r"c:\program files\google\chrome\application\chrome.exe",
            r"c:\tools\excel.exe.bak",
            "excel.exe",
            "com.microsoft.Excel",
            "",
        ] {
            assert_eq!(suggested_scroll_method(app), None, "{app}");
        }
    }

    #[test]
    fn every_suggestion_is_reachable_and_changes_something() {
        for (selector, suggestion) in SUGGESTED_SCROLL_METHODS {
            // The derived selector is lower-cased, so any other key never matches.
            assert_eq!(*selector, selector.to_ascii_lowercase());
            assert!(selector.starts_with("exe:"), "{selector}");
            assert!(
                !suggestion.delivery.is_native(),
                "{selector}: suggesting the default offers nothing"
            );
        }
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
