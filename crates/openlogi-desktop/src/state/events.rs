//! The events [`AppState`] emits, and the one place that emits them.
//!
//! A mutator decides which [`StateEvent`] its change causes and returns it as
//! [`StateEvents`]; [`AppState::apply`] emits what the mutation reported. Views
//! therefore never pick an event, and mutators stay free of a GPUI context so
//! plain `#[test]`s can drive them.

use gpui::{App, Context, EventEmitter};

use super::AppState;
use super::device_key::DeviceKey;
use super::devices::DeviceRecord;

/// Semantic changes emitted by the shared application-state entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateEvent {
    /// Agent connection or permission state changed.
    AgentChanged,
    /// The foreground application or recent-application list changed.
    ForegroundChanged,
    /// Cached diagnostics/event-monitor data changed.
    #[cfg_attr(
        not(all(target_os = "macos", debug_assertions)),
        expect(dead_code, reason = "the live event monitor is macOS debug-only")
    )]
    DiagnosticsChanged,
    /// The merged device inventory changed.
    InventoryChanged,
    /// The active device changed.
    DeviceSelected(DeviceKey),
    /// Mouse, keyboard, gesture, or Actions Ring bindings changed.
    BindingsChanged(DeviceKey),
    /// DPI data or the active DPI value changed.
    DpiChanged(DeviceKey),
    /// SmartShift data or write status changed.
    SmartShiftChanged(DeviceKey),
    /// Device or standalone-light settings changed.
    LightingChanged(DeviceKey),
    /// Camera settings or activity changed.
    CameraChanged,
    /// Host camera-permission status may have changed.
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(
            dead_code,
            reason = "camera consent polling is macOS-only outside tests"
        )
    )]
    CameraPermissionChanged,
    /// Per-device preferences outside the feature-specific events changed.
    DeviceConfigChanged(DeviceKey),
    /// Application-wide preferences changed.
    SettingsChanged,
    /// The interface language switched live. Views re-render localized strings
    /// on the accompanying refresh; this event is for localized text *cached
    /// in state*, which must be recomputed in the new locale.
    LanguageChanged,
}

impl EventEmitter<StateEvent> for AppState {}

/// The [`StateEvent`]s one change to [`AppState`] causes, in the order they
/// happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use = "a change nobody announces leaves subscribed views stale; return it from `AppState::apply`"]
pub(crate) struct StateEvents(Vec<StateEvent>);

impl StateEvents {
    /// A change no view needs to hear about.
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// Whether the change needs no announcement.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `event` is one of them.
    pub(crate) fn contains(&self, event: &StateEvent) -> bool {
        self.0.contains(event)
    }

    /// These events followed by `next`'s, each distinct event once — so a
    /// change built from several mutators announces itself the way a single
    /// one would.
    pub(crate) fn and(mut self, next: impl Into<Self>) -> Self {
        for event in next.into().0 {
            if !self.0.contains(&event) {
                self.0.push(event);
            }
        }
        self
    }

    /// Emit every event from the state entity's own context.
    pub(crate) fn emit(self, cx: &mut Context<AppState>) {
        for event in self.0 {
            let language_changed = event == StateEvent::LanguageChanged;
            cx.emit(event);
            if language_changed {
                relocalize_windows(cx);
            }
        }
    }
}

/// Repaint what a live language switch reaches beyond the subscribed views.
fn relocalize_windows(cx: &mut Context<AppState>) {
    // Locale lookup is process-global, so every open window must repaint;
    // localized text cached in view state re-derives on the event itself.
    cx.refresh_windows();
    // Deferred: the Device menu reads the state entity, whose lease is still
    // held here (events are emitted inside `AppState::update`), and a
    // re-entrant read panics. The native window titles ride along — they are
    // stamped at open and don't re-render with the refresh.
    cx.defer(|cx| {
        crate::app::menu::rebuild(cx);
        crate::windows::retitle_open(cx);
    });
}

/// Lets a test state the exact events a mutation must report.
impl<const N: usize> PartialEq<[StateEvent; N]> for StateEvents {
    fn eq(&self, other: &[StateEvent; N]) -> bool {
        self.0 == *other
    }
}

impl From<StateEvent> for StateEvents {
    fn from(event: StateEvent) -> Self {
        Self(vec![event])
    }
}

impl From<Option<StateEvent>> for StateEvents {
    fn from(event: Option<StateEvent>) -> Self {
        Self(event.into_iter().collect())
    }
}

impl AppState {
    /// Run one mutation against the shared state and emit the events it
    /// reports.
    pub(crate) fn apply(cx: &mut App, mutate: impl FnOnce(&mut Self) -> StateEvents) {
        Self::update(cx, |state, cx| mutate(state).emit(cx));
    }

    /// `event` about the active device, or nothing when no device is selected:
    /// what every editor of the active device announces its change as.
    pub(super) fn for_current_device(&self, event: fn(DeviceKey) -> StateEvent) -> StateEvents {
        self.current_record()
            .map(DeviceRecord::device_key)
            .map(event)
            .into()
    }
}
