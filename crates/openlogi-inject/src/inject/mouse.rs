//! Reference-counted mouse output for physical press lifecycles.

use super::KeyPhase;
use openlogi_core::binding::MouseButton;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

#[derive(Default)]
struct MouseOwners(HashMap<MouseButton, usize>);

impl MouseOwners {
    fn transition(&mut self, button: MouseButton, phase: KeyPhase) -> bool {
        match phase {
            KeyPhase::Down => {
                let count = self.0.entry(button).or_default();
                *count += 1;
                *count == 1
            }
            KeyPhase::Up => {
                let Some(count) = self.0.get_mut(&button) else {
                    return false;
                };
                *count -= 1;
                if *count == 0 {
                    self.0.remove(&button);
                    true
                } else {
                    false
                }
            }
        }
    }
}

static OWNERS: LazyLock<Mutex<MouseOwners>> = LazyLock::new(|| Mutex::new(MouseOwners::default()));

/// One synthetic mouse press; dropping it releases its share of the output.
#[must_use = "keep the mouse hold until physical release or cancellation"]
pub struct HeldMouseButton(MouseButton);

/// Hold a mouse button until the returned owner is dropped. Overlapping
/// physical presses share the output and only the final release posts up.
pub fn press_mouse(button: MouseButton) -> HeldMouseButton {
    transition(button, KeyPhase::Down);
    HeldMouseButton(button)
}

impl Drop for HeldMouseButton {
    fn drop(&mut self) {
        transition(self.0, KeyPhase::Up);
    }
}

fn transition(button: MouseButton, phase: KeyPhase) {
    let mut owners = OWNERS.lock().unwrap_or_else(PoisonError::into_inner);
    if owners.transition(button, phase) {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        super::platform::post_mouse_edge(button, phase);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn middle_hold_posts_no_up_until_the_last_owner_releases() {
        let mut owners = MouseOwners::default();
        assert!(owners.transition(MouseButton::Middle, KeyPhase::Down));
        assert!(!owners.transition(MouseButton::Middle, KeyPhase::Down));
        assert!(!owners.transition(MouseButton::Middle, KeyPhase::Up));
        assert!(owners.transition(MouseButton::Middle, KeyPhase::Up));
        assert!(!owners.transition(MouseButton::Middle, KeyPhase::Up));
        assert!(owners.transition(MouseButton::Middle, KeyPhase::Down));
        assert!(owners.transition(MouseButton::Middle, KeyPhase::Up));
    }
}
