//! Default bindings for a fresh device / gesture map.

use super::action::Action;
use super::button::ButtonId;
use super::gesture::GestureDirection;
use super::value::Binding;

/// Sensible defaults for a fresh device so the panel isn't empty on first run.
///
/// Thumbwheel / GestureButton defaults match what Logi Options+ ships for
/// MX-line devices: thumb wheel click → App Exposé, gesture button →
/// Mission Control. The thumb wheel isn't captured yet; the dedicated gesture button is
/// (per-direction, see [`default_gesture_binding`]). The bindings persist
/// regardless so the user only configures once.
///
/// `GestureButton`'s entry here is vestigial: in the merged [`Binding`] model
/// the gesture button defaults to [`Binding::Gesture`] (see
/// [`default_binding_for`]), so this single-action value is never the source of
/// truth for it. It is retained only so the per-button-`Action` callers (the
/// hook map, scroll defaults, labels) stay total.
#[must_use]
pub fn default_binding(button: ButtonId) -> Action {
    match button {
        ButtonId::LeftClick => Action::LeftClick,
        ButtonId::RightClick => Action::RightClick,
        ButtonId::MiddleClick => Action::MiddleClick,
        ButtonId::Back => Action::BrowserBack,
        ButtonId::Forward => Action::BrowserForward,
        ButtonId::DpiToggle => Action::CycleDpiPresets,
        ButtonId::Thumbwheel => Action::AppExpose,
        // The thumb wheel scrolls horizontally by default: rotating it produces
        // continuous horizontal scroll, with "up" → right and "down" → left.
        // The wheel watcher renders these two actions as smooth, sensitivity-
        // scaled scrolling rather than the discrete per-press burst a button
        // would get (see `watchers::gesture`).
        ButtonId::ThumbwheelScrollUp => Action::HorizontalScrollRight,
        ButtonId::ThumbwheelScrollDown => Action::HorizontalScrollLeft,
        ButtonId::GestureButton => Action::MissionControl,
        ButtonId::HapticPanel => Action::ShowActionsRing,
        // Keyboard keys stay on their native firmware function until the user
        // explicitly binds them; an unbound key is never diverted, so a
        // `None` default keeps the projection total without capturing anything.
        ButtonId::KeySearch
        | ButtonId::KeyDictation
        | ButtonId::KeyEmoji
        | ButtonId::KeyScreenCapture
        | ButtonId::KeyMicMute
        | ButtonId::KeyPlayPause
        | ButtonId::KeyMute
        | ButtonId::KeyVolumeDown
        | ButtonId::KeyVolumeUp
        | ButtonId::KeyCircle
        | ButtonId::KeyTriangle
        | ButtonId::KeyDiamond => Action::None,
    }
}

/// Per-direction defaults for the gesture button. These are captured live over
/// HID++ `0x1b04` (raw-XY diversion) and dispatched like any other binding; the
/// defaults give the picker something sensible to show on first run.
#[must_use]
pub fn default_gesture_binding(direction: GestureDirection) -> Action {
    match direction {
        GestureDirection::Up => Action::MissionControl,
        GestureDirection::Down => Action::ShowDesktop,
        GestureDirection::Left => Action::PrevTab,
        GestureDirection::Right => Action::NextTab,
        GestureDirection::Click => Action::AppExpose,
    }
}

/// The canonical default [`Binding`] for a fresh button in the merged model.
///
/// [`ButtonId::GestureButton`] defaults to [`Binding::Gesture`] populated from
/// [`default_gesture_binding`] — preserving the existing per-direction swipe
/// behavior — so the GUI mode toggle and the runtime agree it starts in gesture
/// mode. Every other button defaults to [`Binding::Single`] of its
/// [`default_binding`].
///
/// This is the seed when a button is first promoted to a gesture binding (see
/// [`Config::set_gesture_direction`](crate::config::Config::set_gesture_direction)),
/// so a freshly-customized gesture button always carries a full default
/// direction map — including a [`GestureDirection::Click`] — rather than a sparse
/// map whose click would project to a no-op [`Action::None`].
#[must_use]
pub fn default_binding_for(button: ButtonId) -> Binding {
    match button {
        ButtonId::GestureButton => Binding::Gesture(
            GestureDirection::ALL
                .into_iter()
                .map(|d| (d, default_gesture_binding(d)))
                .collect(),
        ),
        other => Binding::Single(default_binding(other)),
    }
}
