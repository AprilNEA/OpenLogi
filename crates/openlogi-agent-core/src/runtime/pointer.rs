//! Admission of pointer-selected actions, on the action worker, never the tap.

use std::collections::BTreeSet;

use openlogi_core::binding::{Action, ButtonId, Effect, NativeAction};
use openlogi_hook::PointerTarget;
use tracing::debug;

use super::ActionDispatchTarget;

/// The pointer context a snapshot selected its mouse bindings for.
///
/// It also names the buttons that the hovered context binds unlike the
/// focused application. A per-app override replaces a whole button, so every
/// other button resolves to the binding that focus resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PointerSelection {
    target: PointerTarget,
    unlike_focus: BTreeSet<ButtonId>,
}

impl PointerSelection {
    /// `target`'s selection. `unlike_focus` holds the buttons bound there
    /// unlike the focused application.
    #[must_use]
    pub(crate) fn new(target: PointerTarget, unlike_focus: BTreeSet<ButtonId>) -> Self {
        Self {
            target,
            unlike_focus,
        }
    }

    /// The pointer identity that one press of `button` carries to the worker.
    pub(crate) fn press(&self, button: ButtonId) -> PointerPress {
        let focus = if self.unlike_focus.contains(&button) {
            FocusBinding::Distinct
        } else {
            FocusBinding::Shared
        };
        PointerPress {
            target: self.target,
            focus,
        }
    }

    /// The pointer identity that a press from a layer no app profile changes
    /// carries, such as a held G-Shift layer. Focus shares such a binding.
    pub(crate) fn unscoped_press(&self) -> PointerPress {
        PointerPress {
            target: self.target,
            focus: FocusBinding::Shared,
        }
    }
}

/// Whether the focused application binds a pointer-selected button the same
/// way as the hovered context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusBinding {
    /// Focus resolves the same binding. The keystrokes of the press are the
    /// ones that focused mode sends.
    Shared,
    /// The hovered context binds the button its own way. The keystrokes of
    /// the press run only while the hovered window has focus.
    Distinct,
}

/// The pointer identity captured with one press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PointerPress {
    pub(crate) target: PointerTarget,
    pub(crate) focus: FocusBinding,
}

#[cfg(test)]
impl PointerSelection {
    /// The surface under the pointer that selected the bindings.
    pub(crate) fn target(&self) -> PointerTarget {
        self.target
    }
}

#[cfg(test)]
impl PointerPress {
    /// A press of a button that the hovered context binds its own way, the
    /// strictest admission.
    pub(crate) fn distinct(target: PointerTarget) -> Self {
        Self {
            target,
            focus: FocusBinding::Distinct,
        }
    }
}

/// How an admitted pointer-selected press runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerRoute {
    /// Against the surface that selected the press. Focus stands in only for
    /// an unidentified surface.
    Selected,
    /// As focused mode runs it, because focus binds the button the same way.
    Focus,
}

impl ActionDispatchTarget {
    pub(super) fn resolve(self, action: &Action) -> Option<Self> {
        self.resolve_with(
            action,
            || openlogi_hook::pointer_context().target,
            openlogi_hook::pointer_target_is_focused,
            openlogi_hook::frontmost_safari_pid,
        )
    }

    pub(super) fn resolve_with(
        self,
        action: &Action,
        current: impl FnOnce() -> PointerTarget,
        is_focused: impl FnOnce(PointerTarget) -> bool,
        frontmost_safari_pid: impl FnOnce() -> Option<i32>,
    ) -> Option<Self> {
        let Self::Pointer {
            target,
            focus,
            fallback_safari_pid,
        } = self
        else {
            return Some(self);
        };
        let current = current();
        let press = PointerPress { target, focus };
        let Some(route) = pointer_route(action, press, current, || is_focused(target)) else {
            debug!(action = %action.label(), captured = ?target, ?current, ?focus, "mouse action target unavailable, moved, or not focused — skipped");
            return None;
        };
        Some(match (route, target) {
            // The Safari that was frontmost at the press, as focused mode
            // captures it.
            (PointerRoute::Focus, _) | (PointerRoute::Selected, PointerTarget::Unavailable) => {
                fallback_safari_pid.map_or(Self::Keyboard, Self::SafariProcess)
            }
            // Safari's existing AX implementation uses AXFocusedWindow. It is
            // admitted only after the exact hovered window passed the focus
            // check.
            (PointerRoute::Selected, PointerTarget::Window { process_id, .. }) => {
                if frontmost_safari_pid() == Some(process_id) {
                    Self::SafariProcess(process_id)
                } else {
                    Self::Keyboard
                }
            }
            (PointerRoute::Selected, _) => Self::Keyboard,
        })
    }
}

/// Whether `press` may run `action` with the pointer now over `current`, and
/// how. `is_focused` is consulted only when nothing else decides, because its
/// Accessibility lookup can block.
fn pointer_route(
    action: &Action,
    press: PointerPress,
    current: PointerTarget,
    is_focused: impl FnOnce() -> bool,
) -> Option<PointerRoute> {
    let captured = press.target;
    if captured != current || captured == PointerTarget::Unsupported {
        return None;
    }
    // Still unidentified: the binding came from the focused profile, so it
    // runs as focus would run it, keyboard effects included.
    if captured == PointerTarget::Unavailable || !needs_focus(action) {
        return Some(PointerRoute::Selected);
    }
    match press.focus {
        // Focused mode sends the same keystrokes for this button (#2214).
        FocusBinding::Shared => Some(PointerRoute::Focus),
        FocusBinding::Distinct => (matches!(captured, PointerTarget::Window { .. })
            && is_focused())
        .then_some(PointerRoute::Selected),
    }
}

/// Whether `action` acts on the focused window: keystrokes, workflows, and
/// App Exposé.
fn needs_focus(action: &Action) -> bool {
    match action.effect() {
        Effect::Shortcut(_)
        | Effect::Key(_)
        | Effect::HeldKey(_)
        | Effect::Text(_)
        | Effect::Script(_)
        | Effect::Native(NativeAction::AppExpose) => true,
        Effect::None
        | Effect::Click(_)
        | Effect::Scroll { .. }
        | Effect::Media(_)
        | Effect::Native(_)
        | Effect::AgentSide => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROWSER: PointerTarget = PointerTarget::Window {
        process_id: 41,
        window_id: 7,
    };
    const OTHER_WINDOW: PointerTarget = PointerTarget::Window {
        process_id: 41,
        window_id: 9,
    };
    const BOTH: [FocusBinding; 2] = [FocusBinding::Shared, FocusBinding::Distinct];

    fn press(target: PointerTarget, focus: FocusBinding) -> PointerPress {
        PointerPress { target, focus }
    }

    fn keyboard_actions() -> [Action; 5] {
        let shortcut = "Ctrl+Tab".parse().expect("valid shortcut");
        [
            Action::BrowserForward,
            Action::CustomShortcut(shortcut),
            Action::TypeText("hello".into()),
            Action::AppExpose,
            Action::Copy,
        ]
    }

    #[test]
    fn selection_marks_only_buttons_bound_unlike_focus_as_distinct() {
        let selection =
            PointerSelection::new(PointerTarget::Desktop, BTreeSet::from([ButtonId::Back]));
        assert_eq!(selection.target(), PointerTarget::Desktop);
        assert_eq!(
            selection.press(ButtonId::Back),
            press(PointerTarget::Desktop, FocusBinding::Distinct)
        );
        assert_eq!(
            selection.press(ButtonId::Forward),
            press(PointerTarget::Desktop, FocusBinding::Shared)
        );
    }

    #[test]
    fn desktop_switch_does_not_require_browser_focus_or_send_browser_navigation() {
        for focus in BOTH {
            assert_eq!(
                pointer_route(
                    &Action::NextDesktop,
                    press(PointerTarget::Desktop, focus),
                    PointerTarget::Desktop,
                    || panic!("a target-independent action needs no focus check")
                ),
                Some(PointerRoute::Selected)
            );
        }
        assert_eq!(
            pointer_route(
                &Action::BrowserBack,
                press(PointerTarget::Desktop, FocusBinding::Distinct),
                PointerTarget::Desktop,
                || true
            ),
            None
        );
    }

    #[test]
    fn distinct_keyboard_effects_require_the_exact_hovered_window_to_have_focus() {
        for action in keyboard_actions() {
            let distinct = press(BROWSER, FocusBinding::Distinct);
            assert_eq!(pointer_route(&action, distinct, BROWSER, || false), None);
            assert_eq!(
                pointer_route(&action, distinct, BROWSER, || true),
                Some(PointerRoute::Selected)
            );
            assert_eq!(
                pointer_route(&action, distinct, OTHER_WINDOW, || true),
                None
            );
        }
    }

    #[test]
    fn shared_keyboard_effects_run_as_focus_would_without_a_focus_check() {
        // #2214: a global hotkey over the desktop, over a background window,
        // or over a panel whose app is not frontmost. Focused mode sends the
        // same keystrokes, so skipping them only made the button look flaky.
        for action in keyboard_actions() {
            for target in [PointerTarget::Desktop, BROWSER] {
                assert_eq!(
                    pointer_route(
                        &action,
                        press(target, FocusBinding::Shared),
                        target,
                        || panic!("a shared binding needs no focus check")
                    ),
                    Some(PointerRoute::Focus),
                    "{action:?} over {target:?}"
                );
            }
        }
    }

    #[test]
    fn stale_context_drops_even_target_independent_actions() {
        for focus in BOTH {
            for (captured, current) in [
                (BROWSER, PointerTarget::Desktop),
                (PointerTarget::Desktop, BROWSER),
                (BROWSER, OTHER_WINDOW),
                // The pointer crossed between an overlay and an identified
                // target after the binding was chosen for the other one.
                (PointerTarget::Desktop, PointerTarget::Unavailable),
                (BROWSER, PointerTarget::Unavailable),
                (PointerTarget::Unavailable, PointerTarget::Desktop),
                (PointerTarget::Unavailable, BROWSER),
                // Never captured: unsupported sessions dispatch through focus.
                (PointerTarget::Unsupported, PointerTarget::Unsupported),
            ] {
                for action in [Action::NextDesktop, Action::Copy] {
                    assert_eq!(
                        pointer_route(&action, press(captured, focus), current, || true),
                        None,
                        "{action:?} captured {captured:?}, now {current:?}, {focus:?}"
                    );
                }
            }
        }
        assert_eq!(
            pointer_route(
                &Action::VolumeUp,
                press(BROWSER, FocusBinding::Distinct),
                BROWSER,
                || false
            ),
            Some(PointerRoute::Selected)
        );
    }

    #[test]
    fn unidentified_context_runs_every_effect_as_focus_would() {
        let shortcut = "Ctrl+Tab".parse().expect("valid shortcut");
        for action in [
            Action::MissionControl,
            Action::CaptureRegion,
            Action::NextDesktop,
            Action::VolumeUp,
            Action::AppExpose,
            Action::BrowserBack,
            Action::Copy,
            Action::CustomShortcut(shortcut),
            Action::TypeText("hello".into()),
        ] {
            for focus in BOTH {
                assert_eq!(
                    pointer_route(
                        &action,
                        press(PointerTarget::Unavailable, focus),
                        PointerTarget::Unavailable,
                        || panic!("an unidentified target has no window to check")
                    ),
                    Some(PointerRoute::Selected),
                    "{action:?}"
                );
            }
        }
    }

    #[test]
    fn shared_browser_navigation_goes_to_the_safari_frontmost_at_the_press() {
        // Focus can change before a long press fires. Focused mode keeps the
        // Safari captured at the press, so a shared binding does too.
        let resolve = |target, at_press| {
            ActionDispatchTarget::for_pointer(Some(press(target, FocusBinding::Shared)), || {
                at_press
            })
            .resolve_with(
                &Action::BrowserBack,
                || target,
                |_| panic!("a shared binding needs no focus check"),
                || Some(999),
            )
        };
        for target in [PointerTarget::Desktop, BROWSER] {
            assert_eq!(
                resolve(target, Some(417)),
                Some(ActionDispatchTarget::SafariProcess(417))
            );
            assert_eq!(resolve(target, None), Some(ActionDispatchTarget::Keyboard));
        }
    }

    #[test]
    fn distinct_binding_over_the_desktop_is_skipped() {
        let resolved = ActionDispatchTarget::for_pointer(
            Some(PointerPress::distinct(PointerTarget::Desktop)),
            || panic!("an identified target needs no focused fallback"),
        )
        .resolve_with(
            &Action::BrowserBack,
            || PointerTarget::Desktop,
            |_| true,
            || Some(417),
        );
        assert_eq!(resolved, None);
    }
}
