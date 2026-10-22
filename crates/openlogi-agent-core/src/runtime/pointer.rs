//! Admission of pointer-selected actions, on the action worker, never the tap.

use openlogi_core::binding::{Action, Effect, NativeAction};
use openlogi_hook::PointerTarget;

use super::ActionDispatchTarget;

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
            fallback_safari_pid,
        } = self
        else {
            return Some(self);
        };
        if !pointer_action_allowed(action, target, current(), || is_focused(target)) {
            return None;
        }
        // Safari's existing AX implementation uses AXFocusedWindow. It is
        // admitted only after the exact hovered window passed the focus check.
        Some(match target {
            PointerTarget::Window { process_id, .. } => {
                if frontmost_safari_pid() == Some(process_id) {
                    Self::SafariProcess(process_id)
                } else {
                    Self::Keyboard
                }
            }
            PointerTarget::Unavailable => {
                fallback_safari_pid.map_or(Self::Keyboard, Self::SafariProcess)
            }
            _ => Self::Keyboard,
        })
    }
}

fn pointer_action_allowed(
    action: &Action,
    captured: PointerTarget,
    current: PointerTarget,
    is_focused: impl FnOnce() -> bool,
) -> bool {
    if captured != current || captured == PointerTarget::Unsupported {
        return false;
    }
    // Still unidentified: the binding came from the focused profile, so it
    // runs as focus would run it, keyboard effects included.
    if captured == PointerTarget::Unavailable {
        return true;
    }
    let needs_focus = match action.effect() {
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
    };
    !needs_focus || (matches!(captured, PointerTarget::Window { .. }) && is_focused())
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

    #[test]
    fn volume_batches_read_the_live_pointer_once_and_reject_all_steps_together() {
        for action in [Action::VolumeUp, Action::VolumeDown] {
            for steps in [0, 1, 5, 20] {
                for (captured, current, fallback, admitted) in [
                    (BROWSER, BROWSER, None, true),
                    (BROWSER, OTHER_WINDOW, None, false),
                    (
                        PointerTarget::Unsupported,
                        PointerTarget::Unsupported,
                        None,
                        false,
                    ),
                    (
                        PointerTarget::Unavailable,
                        PointerTarget::Unavailable,
                        None,
                        true,
                    ),
                    (
                        PointerTarget::Unavailable,
                        PointerTarget::Unavailable,
                        Some(417),
                        true,
                    ),
                    (BROWSER, PointerTarget::Unavailable, None, false),
                ] {
                    let reads = std::cell::Cell::new(0);
                    let captures = std::cell::Cell::new(0);
                    let mut dispatched = Vec::new();
                    super::super::dispatch_resolved_batch(
                        steps,
                        || {
                            ActionDispatchTarget::for_pointer(Some(captured), || {
                                captures.set(captures.get() + 1);
                                fallback
                            })
                            .resolve_with(
                                &action,
                                || {
                                    reads.set(reads.get() + 1);
                                    current
                                },
                                |_| panic!("volume must not require keyboard focus"),
                                || None,
                            )
                        },
                        |target| dispatched.push(target),
                    );
                    assert_eq!(reads.get(), u32::from(steps != 0));
                    assert_eq!(
                        captures.get(),
                        u32::from(steps != 0 && captured == PointerTarget::Unavailable)
                    );
                    assert_eq!(dispatched.len(), if admitted { steps as usize } else { 0 });
                    let expected = fallback.map_or(
                        ActionDispatchTarget::Keyboard,
                        ActionDispatchTarget::SafariProcess,
                    );
                    assert!(dispatched.iter().all(|target| *target == expected));
                }
            }
        }
    }

    #[test]
    fn desktop_switch_does_not_require_browser_focus_or_send_browser_navigation() {
        assert!(pointer_action_allowed(
            &Action::NextDesktop,
            PointerTarget::Desktop,
            PointerTarget::Desktop,
            || false
        ));
        assert!(!pointer_action_allowed(
            &Action::BrowserBack,
            PointerTarget::Desktop,
            PointerTarget::Desktop,
            || true
        ));
    }

    #[test]
    fn keyboard_effects_require_the_exact_hovered_window_to_have_focus() {
        let shortcut = "Ctrl+Tab".parse().expect("valid shortcut");
        for action in [
            Action::BrowserForward,
            Action::CustomShortcut(shortcut),
            Action::TypeText("hello".into()),
            Action::AppExpose,
        ] {
            assert!(!pointer_action_allowed(&action, BROWSER, BROWSER, || false));
            assert!(pointer_action_allowed(&action, BROWSER, BROWSER, || true));
            assert!(!pointer_action_allowed(
                &action,
                BROWSER,
                OTHER_WINDOW,
                || true
            ));
        }
    }

    #[test]
    fn stale_context_drops_even_target_independent_actions() {
        for (captured, current) in [
            (BROWSER, PointerTarget::Desktop),
            (PointerTarget::Desktop, BROWSER),
            // The pointer crossed between an overlay and an identified
            // target after the binding was chosen for the other one.
            (PointerTarget::Desktop, PointerTarget::Unavailable),
            (BROWSER, PointerTarget::Unavailable),
            (PointerTarget::Unavailable, PointerTarget::Desktop),
            (PointerTarget::Unavailable, BROWSER),
            // Never captured: unsupported sessions dispatch through focus.
            (PointerTarget::Unsupported, PointerTarget::Unsupported),
        ] {
            assert!(!pointer_action_allowed(
                &Action::NextDesktop,
                captured,
                current,
                || true
            ));
        }
        assert!(pointer_action_allowed(
            &Action::VolumeUp,
            BROWSER,
            BROWSER,
            || false
        ));
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
            assert!(
                pointer_action_allowed(
                    &action,
                    PointerTarget::Unavailable,
                    PointerTarget::Unavailable,
                    || panic!("an unidentified target has no window to check")
                ),
                "{action:?}"
            );
        }
    }
}
