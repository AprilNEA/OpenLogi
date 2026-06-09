//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use openlogi_core::binding::{
    Action, ButtonId, GESTURE_HOLD_FOR_SWIPE, GestureDirection, detect_swipe,
};
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

/// Tracks an in-progress gesture-button hold and commits a swipe *mid-motion*,
/// the moment travel passes [`detect_swipe`] (like Logitech Options+), rather
/// than waiting for release — matching the HID++ thumb-pad path in
/// `openlogi-hid`. A press that never commits a direction is a plain click,
/// fired on release.
#[derive(Default)]
struct HoldState {
    button: Option<ButtonId>,
    dx: i32,
    dy: i32,
    held_since: Option<Instant>,
    /// Set once a swipe has committed this hold, so it fires exactly once and
    /// the release doesn't then also fire the click.
    fired: bool,
}

impl HoldState {
    /// Begin a hold for `button`, resetting the accumulator and commit state.
    fn begin(&mut self, button: ButtonId) {
        self.button = Some(button);
        self.dx = 0;
        self.dy = 0;
        self.held_since = Some(Instant::now());
        self.fired = false;
    }

    /// Feed a pointer-move delta. Once the button has been held past
    /// [`GESTURE_HOLD_FOR_SWIPE`] (so a quick click whose cursor drifted doesn't
    /// count) and the travel commits to a direction, returns
    /// `Some((button, direction))` exactly once per hold; the caller dispatches
    /// it. Returns `None` while still too short, already fired, or not holding.
    /// Saturating, so a very long hold can never overflow.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(ButtonId, GestureDirection)> {
        let button = self.button?;
        if self.fired {
            return None;
        }
        self.dx = self.dx.saturating_add(dx);
        self.dy = self.dy.saturating_add(dy);
        let held_long_enough = self
            .held_since
            .is_some_and(|t| t.elapsed() >= GESTURE_HOLD_FOR_SWIPE);
        if held_long_enough && let Some(dir) = detect_swipe(self.dx, self.dy) {
            self.fired = true;
            return Some((button, dir));
        }
        None
    }

    /// End the hold for `button`. Returns `Some(true)` when it ended a hold that
    /// never committed a swipe (the caller should fire the `Click` action),
    /// `Some(false)` when a swipe already fired, and `None` for a stray release
    /// of a button we weren't holding.
    fn end(&mut self, button: ButtonId) -> Option<bool> {
        if self.button == Some(button) {
            let was_click = !self.fired;
            self.button = None;
            Some(was_click)
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

            // Gesture button: suppress the native click and begin a hold. The
            // swipe commits mid-motion in the `Moved` arm; here, on release, we
            // only fire the plain `Click` when no swipe committed. The cursor is
            // free to drift via the pass-through `Moved` events during the hold.
            if pressed {
                let is_gesture = hook_gestures.read().is_ok_and(|g| g.contains_key(&id));
                if is_gesture {
                    lock_hold(&hold).begin(id);
                    return EventDisposition::Suppress;
                }
            } else if let Some(was_click) = lock_hold(&hold).end(id) {
                if was_click
                    && let Some(action) = hook_gestures.read().ok().and_then(|g| {
                        g.get(&id)
                            .and_then(|m| m.get(&GestureDirection::Click).cloned())
                    })
                {
                    info!(button = %id, action = %action.label(), "gesture click → executing bound action");
                    dispatch_action(&action, &dpi_cycle, &capture);
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
            // Feed an in-progress hold; a committed swipe fires here, mid-motion.
            // Always pass through so the cursor keeps moving — the swipe is read,
            // not consumed (the B2 cursor-drift tradeoff vs. a HID++ raw-XY divert
            // that would freeze the pointer).
            let commit = lock_hold(&hold).accumulate(delta_x, delta_y);
            if let Some((button, dir)) = commit
                && let Some(action) = hook_gestures
                    .read()
                    .ok()
                    .and_then(|g| g.get(&button).and_then(|m| m.get(&dir).cloned()))
            {
                info!(button = %button, ?dir, action = %action.label(), "gesture swipe → executing bound action");
                dispatch_action(&action, &dpi_cycle, &capture);
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
    use openlogi_core::binding::GESTURE_SWIPE_THRESHOLD;

    /// Backdate the hold so it is past the minimum-hold gate and a swipe can
    /// commit — without sleeping in the test.
    fn held_long_enough(hold: &mut HoldState) {
        hold.held_since = Instant::now().checked_sub(GESTURE_HOLD_FOR_SWIPE * 2);
    }

    #[test]
    fn swipe_commits_once_mid_motion_and_suppresses_the_click() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        held_long_enough(&mut hold);

        // A clear rightward swipe commits exactly once, mid-motion.
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Back, GestureDirection::Right))
        );
        // Further motion in the same hold must not re-fire.
        assert_eq!(hold.accumulate(50, 0), None);
        // A release after a committed swipe is NOT a click.
        assert_eq!(hold.end(ButtonId::Back), Some(false));
    }

    #[test]
    fn hold_without_a_swipe_is_a_click_on_release() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Forward);
        held_long_enough(&mut hold);
        // Tiny drift never commits a direction...
        assert_eq!(hold.accumulate(2, -1), None);
        // ...so the release fires the plain click.
        assert_eq!(hold.end(ButtonId::Forward), Some(true));
    }

    #[test]
    fn swipe_does_not_commit_before_the_minimum_hold() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::MiddleClick); // held_since = now
        // A big delta arriving immediately (a quick click whose cursor drifted)
        // must not commit — the hold is younger than GESTURE_HOLD_FOR_SWIPE.
        assert_eq!(hold.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0), None);
        // Once held long enough, the next delta commits.
        held_long_enough(&mut hold);
        assert!(hold.accumulate(GESTURE_SWIPE_THRESHOLD + 100, 0).is_some());
    }

    #[test]
    fn end_ignores_a_different_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        assert_eq!(hold.end(ButtonId::Forward), None);
        assert_eq!(hold.end(ButtonId::Back), Some(true));
    }
}
