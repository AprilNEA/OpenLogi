//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

use openlogi_core::binding::{Action, ButtonId, GestureDirection, detect_swipe};
use openlogi_hid::CaptureChannel;
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};

use crate::DpiCycleState;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};

/// Shared binding map threaded between the config owner and the hook callback.
pub type BindingMap = Arc<RwLock<BTreeMap<ButtonId, Action>>>;

/// Shared per-direction maps for the OS-hook gesture buttons (Middle/Back/
/// Forward in gesture mode), threaded into the hook callback so a hold+swipe
/// resolves to a bound action. The dedicated HID++ gesture button (0x00c3) uses
/// the separate per-direction map on the gesture watcher instead — it never
/// reaches the OS hook.
pub type HookGestures = Arc<RwLock<BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>>>;

/// Pointer-movement accumulator for an in-progress gesture-button hold: which
/// button is held (between its down and up) and the summed movement, so the
/// release can resolve a swipe direction via [`detect_swipe`].
#[derive(Default)]
struct HoldState {
    button: Option<ButtonId>,
    dx: i32,
    dy: i32,
}

impl HoldState {
    /// Begin a hold for `button`, resetting the accumulated movement.
    fn begin(&mut self, button: ButtonId) {
        self.button = Some(button);
        self.dx = 0;
        self.dy = 0;
    }

    /// Add a pointer-move delta to the in-progress hold. Saturating, so a very
    /// long hold can never overflow (the direction is all that matters).
    fn accumulate(&mut self, dx: i32, dy: i32) {
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
    }

    /// Whether a gesture button is currently held.
    fn is_holding(&self) -> bool {
        self.button.is_some()
    }

    /// End the hold for `button`, returning the accumulated `(dx, dy)` — but only
    /// if `button` is the one being held, so a stray release of another button is
    /// ignored.
    fn end(&mut self, button: ButtonId) -> Option<(i32, i32)> {
        if self.button == Some(button) {
            self.button = None;
            Some((self.dx, self.dy))
        } else {
            None
        }
    }
}

/// Lock the hold accumulator, recovering the guard if a previous callback
/// panicked while holding it — a poisoned lock must never wedge the input hook.
fn lock_hold(hold: &Mutex<HoldState>) -> std::sync::MutexGuard<'_, HoldState> {
    hold.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    bindings: BindingMap,
    hook_gestures: HookGestures,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture: CaptureChannel,
) -> Option<Hook> {
    if !Hook::has_accessibility() {
        warn!(
            "Accessibility not granted — events will not be captured. \
             Open System Settings → Privacy & Security → Accessibility."
        );
        return None;
    }

    // Per-hold pointer accumulator. Touched only from the hook callback, which
    // runs serially on one thread, so the mutex is always uncontended (and the
    // callback must never block — see the freeze-hazard note in `macos.rs`).
    let hold = Mutex::new(HoldState::default());

    let result = Hook::start(move |event| match event {
        MouseEvent::Button { id, pressed } => {
            // The CGEventTap only sees standard buttons 0-4. We remap
            // Middle/Back/Forward; the primary L/R clicks always pass through
            // (suppressing them would brick the mouse), and the DPI / thumb /
            // dedicated gesture button aren't visible to the tap at all — the
            // dedicated gesture button is captured separately over HID++.
            if !matches!(
                id,
                ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
            ) {
                return EventDisposition::PassThrough;
            }

            // Gesture button: suppress the native click and, on release, resolve
            // the held movement to a swipe direction (or a plain Click when the
            // pointer barely moved). The button-down is recorded; the cursor is
            // free to drift via the pass-through `Moved` events between them.
            if pressed {
                let is_gesture = hook_gestures.read().is_ok_and(|g| g.contains_key(&id));
                if is_gesture {
                    lock_hold(&hold).begin(id);
                    return EventDisposition::Suppress;
                }
            } else if let Some((dx, dy)) = lock_hold(&hold).end(id) {
                if let Some(map) = hook_gestures.read().ok().and_then(|g| g.get(&id).cloned()) {
                    let action = match detect_swipe(dx, dy) {
                        Some(dir) => map.get(&dir).cloned(),
                        None => map.get(&GestureDirection::Click).cloned(),
                    };
                    if let Some(action) = action {
                        info!(button = %id, dx, dy, action = %action.label(), "gesture → executing bound action");
                        dispatch_action(&action, &dpi_cycle, &capture);
                    }
                }
                return EventDisposition::Suppress;
            }

            // Single-action button.
            let action = bindings.read().ok().and_then(|g| g.get(&id).cloned());
            let Some(action) = action else {
                // Unbound → leave the physical button to the OS.
                return EventDisposition::PassThrough;
            };

            // A button left on its own native click (e.g. Middle → MiddleClick)
            // should just do that click; suppressing and re-synthesising it
            // would be pointless churn.
            if is_native_click(id, &action) {
                return EventDisposition::PassThrough;
            }

            if pressed {
                info!(button = %id, action = %action.label(), "button → executing bound action");
                dispatch_action(&action, &dpi_cycle, &capture);
            }
            EventDisposition::Suppress
        }
        MouseEvent::Moved { delta_x, delta_y } => {
            // Feed an in-progress gesture hold; always pass through so the cursor
            // keeps moving. The swipe is read, not consumed — the B2 cursor-drift
            // tradeoff vs. a HID++ raw-XY divert that would freeze the pointer.
            let mut guard = lock_hold(&hold);
            if guard.is_holding() {
                guard.accumulate(delta_x, delta_y);
            }
            EventDisposition::PassThrough
        }
        MouseEvent::Scroll { .. } => EventDisposition::PassThrough,
    });

    match result {
        Ok(hook) => {
            info!("OS mouse hook installed");
            Some(hook)
        }
        Err(e) => {
            warn!(error = %e, "could not install OS mouse hook — events will not be captured");
            None
        }
    }
}

/// Whether `action` is just `id`'s own native click — i.e. the button is mapped
/// to the very click it already produces. In that case the hook should pass the
/// event through to the OS rather than suppress and re-synthesise it.
fn is_native_click(id: ButtonId, action: &Action) -> bool {
    matches!(
        (id, action),
        (ButtonId::LeftClick, Action::LeftClick)
            | (ButtonId::RightClick, Action::RightClick)
            | (ButtonId::MiddleClick, Action::MiddleClick)
    )
}

/// Route a bound action either to OS-level event synthesis
/// ([`Action::execute`]) or to one of OpenLogi's hardware-side handlers.
///
/// `dpi_cycle` is held across a write lock long enough to advance the index
/// and snapshot the new DPI + target; the actual HID write spawns its own
/// thread via [`write_dpi_in_background`] to keep event callbacks non-blocking.
/// `capture` lets those writes reuse the capture session's open channel.
pub fn dispatch_action(
    action: &Action,
    dpi_cycle: &Arc<RwLock<DpiCycleState>>,
    capture: &CaptureChannel,
) {
    let next = match action {
        Action::CycleDpiPresets => match dpi_cycle.write() {
            Ok(mut guard) => guard.cycle(),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                None
            }
        },
        Action::SetDpiPreset(i) => match dpi_cycle.write() {
            Ok(mut guard) => guard.set(usize::from(*i)),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                None
            }
        },
        Action::ToggleSmartShift => {
            let target = dpi_cycle.read().ok().and_then(|g| g.target.clone());
            info!("SmartShift toggle → flipping wheel mode");
            toggle_smartshift_in_background(Some(capture), target);
            return;
        }
        other => {
            other.execute();
            None
        }
    };
    if let Some((dpi, target)) = next {
        info!(dpi, "DPI action → writing to device");
        write_dpi_in_background(Some(capture), target, dpi);
    } else if matches!(action, Action::CycleDpiPresets | Action::SetDpiPreset(_)) {
        info!(
            action = %action.label(),
            "no DPI presets configured for active device — press ignored"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_accumulates_movement_between_begin_and_end() {
        let mut hold = HoldState::default();
        assert!(!hold.is_holding());

        hold.begin(ButtonId::Back);
        assert!(hold.is_holding());
        hold.accumulate(3, -1);
        hold.accumulate(2, -4);

        // `end` of the held button returns the summed movement and clears the hold.
        assert_eq!(hold.end(ButtonId::Back), Some((5, -5)));
        assert!(!hold.is_holding());
    }

    #[test]
    fn hold_begin_resets_prior_accumulation() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        hold.accumulate(10, 10);
        // A fresh press starts a clean accumulator.
        hold.begin(ButtonId::Forward);
        hold.accumulate(1, 2);
        assert_eq!(hold.end(ButtonId::Forward), Some((1, 2)));
    }

    #[test]
    fn hold_end_ignores_a_different_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        hold.accumulate(4, 4);
        // Releasing a button we aren't holding must not end the hold.
        assert_eq!(hold.end(ButtonId::Forward), None);
        assert!(hold.is_holding());
        assert_eq!(hold.end(ButtonId::Back), Some((4, 4)));
    }

    #[test]
    fn hold_accumulation_saturates_instead_of_overflowing() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::MiddleClick);
        hold.accumulate(i32::MAX, i32::MIN);
        hold.accumulate(i32::MAX, i32::MIN);
        assert_eq!(hold.end(ButtonId::MiddleClick), Some((i32::MAX, i32::MIN)));
    }
}
