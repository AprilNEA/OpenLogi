//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};
use openlogi_hid::CaptureChannel;
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};

use crate::DpiCycleState;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};

/// The two button maps the OS-hook callback reads, kept behind ONE lock so a
/// config rebuild publishes both atomically — a press during an owner switch can
/// never see the new single-action bindings against the old gesture map (or vice
/// versa), and the common case reads one lock instead of two.
#[derive(Default)]
pub struct HookMaps {
    /// Per-button single action — the single-action dispatch path.
    pub bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
    /// gesture mode), so a hold+swipe resolves to a bound action. The dedicated
    /// HID++ gesture button (0x00c3) uses the gesture watcher's separate map
    /// instead — it never reaches the OS hook.
    pub gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
}

/// Shared, atomically-published [`HookMaps`], threaded between the config owner
/// (orchestrator), the OS-hook callback, and the gesture watcher.
pub type SharedHookMaps = Arc<RwLock<HookMaps>>;

/// Tracks which OS-hook button (Middle/Back/Forward) is mid-hold and defers the
/// swipe detection itself to a shared [`SwipeAccumulator`], which commits a swipe
/// *mid-motion* like the HID++ thumb-pad path in `openlogi-hid`. This wrapper
/// adds only the button identity the accumulator doesn't track; a press that
/// never commits a direction is a plain click, fired on release.
#[derive(Default)]
struct HoldState {
    button: Option<ButtonId>,
    swipe: SwipeAccumulator,
}

impl HoldState {
    /// Begin a hold for `button`.
    fn begin(&mut self, button: ButtonId) {
        self.button = Some(button);
        self.swipe.begin();
    }

    /// Feed a pointer-move delta into the active hold, tagging a committed swipe
    /// with the held button. Returns `Some((button, direction))` exactly once per
    /// hold, or `None` while still too short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(ButtonId, GestureDirection)> {
        let button = self.button?;
        self.swipe.accumulate(dx, dy).map(|dir| (button, dir))
    }

    /// End the hold for `button`. Returns `Some(true)` when it ended a hold that
    /// never committed a swipe (the caller should fire the `Click` action),
    /// `Some(false)` when a swipe already fired, and `None` for a stray release
    /// of a button we weren't holding.
    fn end(&mut self, button: ButtonId) -> Option<bool> {
        if self.button == Some(button) {
            self.button = None;
            Some(self.swipe.end())
        } else {
            None
        }
    }

    /// Cancel any in-progress hold without firing anything — used when the OS
    /// interrupts capture. A dropped button-up would otherwise leave a stale hold
    /// that the next stray pointer move turns into a phantom swipe.
    fn cancel(&mut self) {
        self.button = None;
        self.swipe.end();
    }
}

thread_local! {
    /// In-progress gesture hold, one instance per hook-callback thread: the
    /// single macOS tap thread, or — on Linux — one thread per device, so two
    /// mice never share a hold (a press on one can't hijack the other's swipe).
    /// Thread-local rather than a shared `Mutex` keeps the hot path lock-free and
    /// free of cross-thread contention on the freeze-sensitive callback.
    static HOLD: RefCell<HoldState> = RefCell::new(HoldState::default());
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
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

    // The per-hold pointer accumulator lives in the thread-local `HOLD`; the
    // callback must never block — see the freeze-hazard note in `macos.rs`.
    let result = Hook::start(move |event| match event {
        MouseEvent::Button { id, pressed } => {
            // The CGEventTap only sees standard buttons 0-4. We remap
            // Middle/Back/Forward; the primary L/R clicks always pass through
            // (suppressing them would brick the mouse), and the DPI / thumb /
            // dedicated gesture button aren't visible to the tap at all — the
            // dedicated gesture button is captured separately over HID++.
            if !id.is_os_hook_button() {
                return EventDisposition::PassThrough;
            }

            // Gesture button: suppress the native click and begin a hold. The
            // swipe commits mid-motion in the `Moved` arm; here, on release, we
            // only fire the plain `Click` when no swipe committed. The cursor is
            // free to drift via the pass-through `Moved` events during the hold.
            if pressed {
                let is_gesture = hooks.read().is_ok_and(|m| m.gestures.contains_key(&id));
                if is_gesture {
                    HOLD.with_borrow_mut(|h| h.begin(id));
                    return EventDisposition::Suppress;
                }
            } else {
                // Release: end the hold and release the `HOLD` borrow *before* any
                // dispatch — the callback must stay lock-light, since a
                // synthesized event could otherwise re-enter the tap and re-borrow
                // `HOLD` (a RefCell double-borrow panic, freeze hazard).
                let ended = HOLD.with_borrow_mut(|h| h.end(id));
                if let Some(was_click) = ended {
                    if was_click {
                        // No swipe committed → fire the plain click. Resolve to an
                        // owned action (so no lock is held across dispatch), then
                        // dispatch with the guard already dropped.
                        let action = hooks
                            .read()
                            .ok()
                            .map(|m| resolve_gesture_click(&m.gestures, id));
                        if let Some(action) = action {
                            info!(button = %id, action = %action.label(), "gesture click → executing bound action");
                            dispatch_action(&action, &dpi_cycle, &capture);
                        }
                    }
                    return EventDisposition::Suppress;
                }
            }

            // Single-action button.
            let action = hooks.read().ok().and_then(|m| m.bindings.get(&id).cloned());
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
            let commit = HOLD.with_borrow_mut(|h| h.accumulate(delta_x, delta_y));
            if let Some((button, dir)) = commit {
                // Resolve to an owned action and drop the read guard before
                // dispatch (same lock-light rule as the release arm). The button
                // can leave the gesture set mid-hold (a per-app rebuild); the
                // commit has already armed `fired`, so the release won't fire a
                // click. Fall back to the same click action the release path uses
                // so the suppressed press is never swallowed into nothing —
                // symmetric with `resolve_gesture_click`.
                let action = hooks.read().ok().map(|m| {
                    m.gestures
                        .get(&button)
                        .and_then(|dirs| dirs.get(&dir).cloned())
                        .unwrap_or_else(|| resolve_gesture_click(&m.gestures, button))
                });
                if let Some(action) = action {
                    info!(button = %button, ?dir, action = %action.label(), "gesture swipe → executing bound action");
                    dispatch_action(&action, &dpi_cycle, &capture);
                }
            }
            EventDisposition::PassThrough
        }
        MouseEvent::CaptureInterrupted => {
            // The OS dropped events (tap disabled); cancel any hold so a lost
            // button-up can't later commit a phantom swipe off ordinary motion.
            HOLD.with_borrow_mut(HoldState::cancel);
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

/// The action a gesture button's plain (no-swipe) click should fire: its
/// explicit [`GestureDirection::Click`] entry — honoring an explicit
/// [`Action::None`] ("Do Nothing") — or the button's [`default_binding`] when
/// the gesture map has no `Click` key (a sparse / hand-edited map, or the button
/// left the gesture set mid-hold). The fallback guarantees a gesture button's
/// suppressed press is never swallowed into nothing.
fn resolve_gesture_click(
    gestures: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    id: ButtonId,
) -> Action {
    gestures
        .get(&id)
        .and_then(|m| m.get(&GestureDirection::Click).cloned())
        .unwrap_or_else(|| default_binding(id))
}

/// Whether `action` is just `id`'s own native event — i.e. the button is mapped
/// to the very click (or extra-button press) it already produces. In that case
/// the hook should pass the event through to the OS rather than suppress and
/// re-synthesise it. For Back/Forward this keeps the genuine hardware button
/// 4/5 intact instead of round-tripping it through synthesis.
fn is_native_click(id: ButtonId, action: &Action) -> bool {
    matches!(
        (id, action),
        (ButtonId::LeftClick, Action::LeftClick)
            | (ButtonId::RightClick, Action::RightClick)
            | (ButtonId::MiddleClick, Action::MiddleClick)
            | (ButtonId::Back, Action::MouseBack)
            | (ButtonId::Forward, Action::MouseForward)
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

    // The mid-swipe gate itself is unit-tested on `SwipeAccumulator` in
    // `openlogi-core`; these cover only what `HoldState` adds on top — tagging a
    // commit with the held button, and matching the button on release.

    #[test]
    fn accumulate_tags_a_committed_swipe_with_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        hold.swipe.backdate_hold_for_test();

        // A clear rightward swipe commits, tagged with the held button.
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Back, GestureDirection::Right))
        );
        assert_eq!(
            hold.accumulate(50, 0),
            None,
            "commits at most once per hold"
        );
        // A release after a committed swipe is NOT a click.
        assert_eq!(hold.end(ButtonId::Back), Some(false));
    }

    #[test]
    fn end_matches_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        // A stray release of a button we weren't holding is ignored...
        assert_eq!(hold.end(ButtonId::Forward), None);
        // ...and ending the held button with no swipe is a plain click.
        assert_eq!(hold.end(ButtonId::Back), Some(true));
    }

    #[test]
    fn resolve_gesture_click_prefers_explicit_then_falls_back_to_default() {
        // Explicit Click action wins.
        let gestures = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Click, Action::Copy)]),
        )]);
        assert_eq!(
            resolve_gesture_click(&gestures, ButtonId::Back),
            Action::Copy
        );

        // Explicit `Click = None` ("Do Nothing") is respected, NOT overridden by
        // the default — the button intentionally does nothing on a plain click.
        let off = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Click, Action::None)]),
        )]);
        assert_eq!(resolve_gesture_click(&off, ButtonId::Back), Action::None);
    }

    #[test]
    fn resolve_gesture_click_falls_back_when_click_is_absent() {
        // A gesture map with no Click key (sparse/hand-edited) falls back to the
        // button's default, so the suppressed press is never swallowed.
        let no_click = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Up, Action::Copy)]),
        )]);
        assert_eq!(
            resolve_gesture_click(&no_click, ButtonId::Back),
            default_binding(ButtonId::Back)
        );

        // The button missing from the map entirely (e.g. removed by a config
        // reload mid-hold) also falls back to its default rather than nothing.
        let empty = BTreeMap::new();
        assert_eq!(
            resolve_gesture_click(&empty, ButtonId::Forward),
            default_binding(ButtonId::Forward)
        );
    }
}
