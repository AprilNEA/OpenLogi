//! The events [`AppState`] emits, the one place that emits them, and the one
//! filter device panels listen through.
//!
//! A mutator decides which [`StateEvent`] its change causes and returns it as
//! [`StateEvents`]; [`AppState::apply`] emits what the mutation reported. Views
//! therefore never pick an event, and mutators stay free of a GPUI context so
//! plain `#[test]`s can drive them. On the receiving side a panel names the
//! events it renders and [`AppState::repaint_on`] decides whether one of them
//! is about the device on screen.

use gpui::{App, Context, EventEmitter, Subscription};

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

impl StateEvent {
    /// The device this event is about, or `None` for an app-wide one.
    pub(crate) fn device(&self) -> Option<&DeviceKey> {
        match self {
            Self::DeviceSelected(key)
            | Self::BindingsChanged(key)
            | Self::DpiChanged(key)
            | Self::SmartShiftChanged(key)
            | Self::LightingChanged(key)
            | Self::DeviceConfigChanged(key) => Some(key),
            Self::AgentChanged
            | Self::ForegroundChanged
            | Self::DiagnosticsChanged
            | Self::InventoryChanged
            | Self::CameraChanged
            | Self::CameraPermissionChanged
            | Self::SettingsChanged
            | Self::LanguageChanged => None,
        }
    }
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

    /// Repaint `cx`'s view — a panel of the active device — whenever that
    /// device or what is known about it changes, and on every event `topic`
    /// picks that is app-wide or about that device. An event about another
    /// device never repaints the panel, whatever `topic` says.
    pub(crate) fn repaint_on<V: 'static>(
        cx: &mut Context<V>,
        topic: impl Fn(&StateEvent) -> bool + 'static,
    ) -> Subscription {
        cx.subscribe(&Self::global(cx), move |_, state, event, cx| {
            if state.read(cx).concerns_device_panel(event, &topic) {
                cx.notify();
            }
        })
    }

    /// Whether `key` is the device on screen.
    pub(crate) fn is_current_device(&self, key: &DeviceKey) -> bool {
        self.current_record()
            .is_some_and(|record| record.device_key() == *key)
    }

    /// The rule behind [`Self::repaint_on`]. Exhaustive on purpose: a new
    /// event has to say here whether every device panel repaints on it, or
    /// only the panels that asked.
    fn concerns_device_panel(
        &self,
        event: &StateEvent,
        topic: impl Fn(&StateEvent) -> bool,
    ) -> bool {
        match event {
            StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
            StateEvent::AgentChanged
            | StateEvent::ForegroundChanged
            | StateEvent::DiagnosticsChanged
            | StateEvent::BindingsChanged(_)
            | StateEvent::DpiChanged(_)
            | StateEvent::SmartShiftChanged(_)
            | StateEvent::LightingChanged(_)
            | StateEvent::CameraChanged
            | StateEvent::CameraPermissionChanged
            | StateEvent::DeviceConfigChanged(_)
            | StateEvent::SettingsChanged
            | StateEvent::LanguageChanged => {
                topic(event) && event.device().is_none_or(|key| self.is_current_device(key))
            }
        }
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

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Action, ButtonId};
    use openlogi_core::config::Config;

    use super::{StateEvent, StateEvents};
    use crate::services::assets::AssetResolver;
    use crate::state::tests::{KNOWN_MOUSE_KEY, state_with_a_known_mouse};
    use crate::state::{AppState, ConfigPersistence, DeviceKey};

    fn state_without_devices() -> AppState {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        AppState::with_runtime(
            Config::ephemeral(),
            &[],
            &[],
            &AssetResolver::new(),
            &[],
            ConfigPersistence::MemoryOnly,
            commands,
        )
    }

    #[test]
    fn a_device_edit_is_announced_for_the_device_on_screen() {
        let mut state = state_with_a_known_mouse();

        assert_eq!(
            state.commit_binding(ButtonId::Back, Action::Undo),
            [StateEvent::BindingsChanged(DeviceKey::from(
                KNOWN_MOUSE_KEY
            ))]
        );
    }

    #[test]
    fn a_device_edit_with_no_device_selected_announces_nothing() {
        let mut state = state_without_devices();

        assert!(
            state
                .commit_binding(ButtonId::Back, Action::Undo)
                .is_empty()
        );
    }

    #[test]
    fn merged_changes_announce_each_event_once_in_first_seen_order() {
        let merged = StateEvents::from(StateEvent::LanguageChanged)
            .and(StateEvent::SettingsChanged)
            .and(StateEvents::none())
            .and(StateEvent::LanguageChanged);

        assert_eq!(
            merged,
            [StateEvent::LanguageChanged, StateEvent::SettingsChanged]
        );
    }

    #[test]
    fn a_device_panel_repaints_for_its_topic_only_when_it_concerns_the_device_on_screen() {
        let state = state_with_a_known_mouse();
        let on_screen = || DeviceKey::from(KNOWN_MOUSE_KEY);
        let elsewhere = || DeviceKey::from("unit:00000001");
        let topic = |event: &StateEvent| {
            matches!(
                event,
                StateEvent::BindingsChanged(_) | StateEvent::CameraChanged
            )
        };

        for (event, repaints) in [
            // Whatever the topic, a change of the device on screen repaints.
            (StateEvent::InventoryChanged, true),
            (StateEvent::DeviceSelected(elsewhere()), true),
            (StateEvent::BindingsChanged(on_screen()), true),
            (StateEvent::BindingsChanged(elsewhere()), false),
            (StateEvent::CameraChanged, true),
            // Off topic, even about the device on screen or app-wide.
            (StateEvent::DpiChanged(on_screen()), false),
            (StateEvent::SettingsChanged, false),
        ] {
            assert_eq!(
                state.concerns_device_panel(&event, topic),
                repaints,
                "{event:?}"
            );
        }
    }

    #[test]
    fn no_device_on_screen_means_no_device_event_concerns_a_panel() {
        let state = state_without_devices();

        assert!(!state.concerns_device_panel(
            &StateEvent::BindingsChanged(DeviceKey::from(KNOWN_MOUSE_KEY)),
            |_| true
        ));
    }
}
