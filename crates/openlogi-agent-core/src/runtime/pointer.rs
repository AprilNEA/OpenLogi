//! Admission of pointer-selected actions, on the action worker, never the tap.

use openlogi_core::binding::{Action, Effect, NativeAction};
use openlogi_hook::PointerTarget;

use super::ActionDispatchTarget;

impl ActionDispatchTarget {
    pub(super) fn resolve(self, action: &Action) -> Option<Self> {
        let Self::Pointer(target) = self else {
            return Some(self);
        };
        let current = openlogi_hook::pointer_context();
        if !pointer_action_allowed(action, target, current.target, || {
            openlogi_hook::pointer_target_is_focused(target)
        }) {
            return None;
        }
        // Safari's existing AX implementation uses AXFocusedWindow. It is
        // admitted only after the exact hovered window passed the focus check.
        Some(match target {
            PointerTarget::Window { process_id, .. }
                if openlogi_hook::frontmost_safari_pid() == Some(process_id) =>
            {
                Self::SafariProcess(process_id)
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
    if captured != current
        || matches!(
            captured,
            PointerTarget::Unavailable | PointerTarget::Unsupported
        )
    {
        return false;
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
    fn stale_or_unknown_context_never_becomes_a_desktop_action() {
        for (captured, current) in [
            (BROWSER, PointerTarget::Desktop),
            (PointerTarget::Desktop, BROWSER),
            (PointerTarget::Unavailable, PointerTarget::Unavailable),
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
}
