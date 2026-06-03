//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The GPUI thread owns `AppState`, while the CGEventTap hook and HID++
//! gesture watcher run outside it. This module contains the shared runtime
//! surface between them: the binding map mirrored from `AppState`, lazy hook
//! installation, and action dispatch for both hook and gesture events.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use openlogi_core::binding::{Action, ButtonId};
use openlogi_hid::CaptureChannel;
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};

use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::state::DpiCycleState;

/// Shared binding map threaded between `AppState` and the hook callback.
pub type BindingMap = Arc<RwLock<BTreeMap<ButtonId, Action>>>;

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    bindings: BindingMap,
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

    let result = Hook::start(move |event| match event {
        MouseEvent::Button { id, pressed } => {
            // The CGEventTap only sees standard buttons 0-4. We remap
            // Middle/Back/Forward; the primary L/R clicks always pass through
            // (suppressing them would brick the mouse), and the DPI / thumb /
            // gesture buttons aren't visible to the tap at all — the gesture
            // button is captured separately over HID++.
            if !matches!(
                id,
                ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
            ) {
                return EventDisposition::PassThrough;
            }

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

/// Whether `action` is just `id`'s own native event — i.e. the button is mapped
/// to the very click (or extra-button press) it already produces. In that case
/// the hook should pass the event through to the OS rather than suppress and
/// re-synthesise it. For Back/Forward this also keeps the genuine hardware
/// button4/5 intact instead of round-tripping it through synthesis.
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
