//! What the Buttons panel shows of a mouse whose flash holds its bindings
//! (the G700): what each slot does on the mouse, who runs a configured
//! action — the mouse or OpenLogi — and where a flash write stands.
//!
//! Which action goes where is `openlogi_core::binding::flash`'s decision;
//! this view model only reads it back for display.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use openlogi_core::binding::flash::{MOUSE_ONLY_FUNCTIONS, flash_intents};
use openlogi_core::binding::{
    Action, ButtonId, FlashBinding, FlashBindingsState, FlashHost, FlashWrite, GamingLayout,
    MouseButtonNumber, RefusedSlot, Slot, SlotIntent,
};
use openlogi_core::config::Config;

use super::AppState;
use crate::state::DeviceRecord;

/// What the panel says about one slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SlotNote {
    /// The mouse runs the slot's action itself, with or without OpenLogi.
    OnMouse,
    /// OpenLogi runs the configured action when the mouse presses agent
    /// button `number`.
    ByOpenLogi(Option<MouseButtonNumber>),
    /// OpenLogi would run it, but no agent button was left free.
    NoFreeButton,
    /// The mouse holds a macro here, which picking an action replaces.
    Macro,
    /// The mouse holds a binding OpenLogi cannot name.
    Unknown,
    /// The configured action is one only OpenLogi runs, and this is a
    /// primary button, which keeps its click on the mouse instead.
    PrimaryKept,
    /// The configured action is one only OpenLogi runs, and this platform's
    /// input hook cannot see the button the mouse would press for it, so
    /// the slot keeps what the mouse holds.
    Unavailable,
}

/// Where the mouse's flash stands, for the panel's status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlashStatus {
    Writing,
    Failed(String),
    /// The mouse plays its factory profile, which holds no bindings.
    FactoryProfile,
}

/// The panel's reading of one flash-bindings mouse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FlashView {
    /// What the mouse does for each slot with no configured binding, when
    /// an action names it.
    pub shown: BTreeMap<ButtonId, Action>,
    pub notes: BTreeMap<ButtonId, SlotNote>,
    pub status: Option<FlashStatus>,
    /// What each slot does as the layout ships it, where an action names it:
    /// what "reset to default" binds.
    pub defaults: BTreeMap<ButtonId, Action>,
}

impl AppState {
    /// The Buttons panel's reading of the selected mouse, if its flash holds
    /// its bindings.
    #[must_use]
    pub(crate) fn current_flash_view(&self) -> Option<FlashView> {
        let layout = self.current_flash_layout()?;
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key);
        let state = self.current_flash_bindings();
        // The played profile's own configuration, whichever the entry holds.
        let profile = key
            .zip(state.as_ref().and_then(|state| state.page))
            .map(|(key, page)| self.config.flash_profile_config(key, page));
        Some(flash_view(
            profile.as_ref().unwrap_or(&self.config),
            key,
            layout,
            state.as_deref(),
            FlashHost::CURRENT,
        ))
    }
}

/// Read `state`, the flash of the mouse `key` with `layout`, against what
/// `config` asks of it. `state` is `None` until the agent has answered; a
/// mouse with no configuration key has nothing configured.
fn flash_view(
    config: &Config,
    key: Option<&str>,
    layout: &GamingLayout,
    state: Option<&FlashBindingsState>,
    host: FlashHost,
) -> FlashView {
    let mut view = FlashView {
        defaults: layout
            .slots
            .iter()
            .zip(layout.flash_defaults)
            .filter_map(|(slot, binding)| Some((slot.button, binding.action(host.modifiers)?)))
            .collect(),
        ..FlashView::default()
    };
    let intents = key.map_or_else(
        || vec![SlotIntent::Keep; layout.slots.len()],
        |key| flash_intents(config, key, layout, host),
    );
    for (&Slot { button, .. }, intent) in layout.slots.iter().zip(&intents) {
        let held = state.and_then(|state| state.slot(layout, button));
        let note = match intent {
            SlotIntent::Native(_) => Some(SlotNote::OnMouse),
            SlotIntent::Agent => Some(agent_note(button, state)),
            SlotIntent::Refused(RefusedSlot::PrimaryButton) => Some(SlotNote::PrimaryKept),
            SlotIntent::Refused(RefusedSlot::HookBlind) => Some(SlotNote::Unavailable),
            SlotIntent::Keep => held.map(|held| match held.action(host.modifiers) {
                Some(action) => {
                    view.shown.insert(button, action);
                    SlotNote::OnMouse
                }
                None if matches!(held, FlashBinding::Macro { .. }) => SlotNote::Macro,
                None => SlotNote::Unknown,
            }),
        };
        view.notes.extend(note.map(|note| (button, note)));
    }
    view.status = state.and_then(|state| match &state.write {
        FlashWrite::Pending | FlashWrite::Writing => Some(FlashStatus::Writing),
        FlashWrite::Failed(error) => Some(FlashStatus::Failed(error.clone())),
        FlashWrite::Idle => state
            .profile
            .is_none()
            .then_some(FlashStatus::FactoryProfile),
    });
    view
}

/// Who runs an agent-run slot's action, as far as the last write shows.
fn agent_note(button: ButtonId, state: Option<&FlashBindingsState>) -> SlotNote {
    let Some(state) = state else {
        return SlotNote::ByOpenLogi(None);
    };
    if state.unrouted.contains(&button) {
        return SlotNote::NoFreeButton;
    }
    SlotNote::ByOpenLogi(
        state
            .routed
            .iter()
            .find_map(|(&number, &slot)| (slot == button).then_some(number)),
    )
}

/// The actions the picker adds for a flash-bindings mouse: the functions
/// only its firmware runs, and the buttons past five it can press. The flash
/// runs no per-app action, so an app profile is offered none of them.
pub(crate) fn picker_extras(app_scope: bool) -> &'static [Action] {
    static EXTRAS: LazyLock<Vec<Action>> = LazyLock::new(|| {
        MOUSE_ONLY_FUNCTIONS
            .into_iter()
            .chain(
                (6..=8)
                    .filter_map(|number| MouseButtonNumber::try_new(number).ok())
                    .map(Action::MouseButton),
            )
            .collect()
    });
    if app_scope { &[] } else { EXTRAS.as_slice() }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Binding, FlashFunction, G700};
    use openlogi_core::config::DeviceConfig;

    use super::*;

    const KEY: &str = "receiver:C531:slot:1";

    const LINUX: FlashHost = FlashHost {
        modifiers: openlogi_core::binding::ModifierHost::Other,
        hook: openlogi_core::binding::AgentButtonHook::Sees,
    };

    fn number(n: u8) -> MouseButtonNumber {
        MouseButtonNumber::try_new(n).expect("a mouse button number")
    }

    fn config(bindings: &[(ButtonId, Action)]) -> Config {
        let mut config = Config::default();
        let mut device = DeviceConfig::default();
        for (button, action) in bindings {
            device
                .bindings
                .insert(*button, Binding::Single(action.clone()));
        }
        config.devices.insert(KEY.into(), device);
        config
    }

    fn factory_state() -> FlashBindingsState {
        FlashBindingsState {
            profile: Some(0),
            slots: G700.flash_defaults.to_vec(),
            ..FlashBindingsState::default()
        }
    }

    #[test]
    fn unconfigured_slots_show_what_the_mouse_holds() {
        let mut state = factory_state();
        state.slots[9] = FlashBinding::Macro {
            page: 6,
            word_offset: 0,
        };
        state.slots[10] = FlashBinding::Function(FlashFunction::Other(0x99));
        let view = flash_view(&config(&[]), Some(KEY), &G700, Some(&state), LINUX);
        assert_eq!(
            view.shown.get(&ButtonId::G6),
            Some(&Action::MouseButton(number(6)))
        );
        assert_eq!(
            view.shown.get(&ButtonId::G8),
            Some(&Action::ShowBatteryLevel)
        );
        assert_eq!(view.notes.get(&ButtonId::G6), Some(&SlotNote::OnMouse));
        assert_eq!(
            view.defaults.get(&ButtonId::G11),
            Some(&Action::CycleOnboardProfiles),
            "the layout default is known even where the mouse holds a macro"
        );
        assert_eq!(view.defaults.len(), G700.slots.len());
        assert_eq!(view.notes.get(&ButtonId::G10), Some(&SlotNote::Macro));
        assert_eq!(view.notes.get(&ButtonId::G11), Some(&SlotNote::Unknown));
        assert!(!view.shown.contains_key(&ButtonId::G10));
        assert_eq!(view.status, None);
    }

    #[test]
    fn configured_slots_say_who_runs_them() {
        let mut state = factory_state();
        state.routed = [(number(16), ButtonId::G7)].into();
        state.unrouted = vec![ButtonId::G8];
        let config = config(&[
            (ButtonId::G6, Action::VolumeUp),
            (ButtonId::G7, Action::Copy),
            (ButtonId::G8, Action::Paste),
        ]);
        let view = flash_view(&config, Some(KEY), &G700, Some(&state), LINUX);
        assert_eq!(view.notes.get(&ButtonId::G6), Some(&SlotNote::OnMouse));
        assert_eq!(
            view.notes.get(&ButtonId::G7),
            Some(&SlotNote::ByOpenLogi(Some(number(16))))
        );
        assert_eq!(view.notes.get(&ButtonId::G8), Some(&SlotNote::NoFreeButton));
        assert!(
            !view.shown.contains_key(&ButtonId::G7),
            "a configured slot shows its configuration"
        );
    }

    #[test]
    fn refused_slots_say_why_the_mouse_keeps_them() {
        let config = config(&[
            (ButtonId::LeftClick, Action::Copy),
            (ButtonId::G7, Action::Copy),
        ]);
        let windows = FlashHost {
            hook: openlogi_core::binding::AgentButtonHook::Blind,
            ..LINUX
        };
        let view = flash_view(&config, Some(KEY), &G700, Some(&factory_state()), windows);
        assert_eq!(
            view.notes.get(&ButtonId::LeftClick),
            Some(&SlotNote::PrimaryKept)
        );
        assert_eq!(view.notes.get(&ButtonId::G7), Some(&SlotNote::Unavailable));
        let view = flash_view(&config, Some(KEY), &G700, Some(&factory_state()), LINUX);
        assert_eq!(
            view.notes.get(&ButtonId::G7),
            Some(&SlotNote::ByOpenLogi(None))
        );
    }

    #[test]
    fn nothing_is_claimed_before_the_agent_answers() {
        let view = flash_view(
            &config(&[(ButtonId::G7, Action::Copy)]),
            Some(KEY),
            &G700,
            None,
            LINUX,
        );
        assert!(view.shown.is_empty());
        assert_eq!(
            view.notes.into_iter().collect::<Vec<_>>(),
            [(ButtonId::G7, SlotNote::ByOpenLogi(None))]
        );
        assert_eq!(view.status, None);
    }

    #[test]
    fn the_status_follows_the_write_and_the_played_profile() {
        let status = |write: FlashWrite, profile: Option<u8>| {
            let state = FlashBindingsState {
                write,
                profile,
                ..factory_state()
            };
            flash_view(&config(&[]), Some(KEY), &G700, Some(&state), LINUX).status
        };
        assert_eq!(
            status(FlashWrite::Pending, Some(0)),
            Some(FlashStatus::Writing)
        );
        assert_eq!(
            status(FlashWrite::Writing, Some(0)),
            Some(FlashStatus::Writing)
        );
        assert_eq!(
            status(FlashWrite::Failed("gone".into()), Some(0)),
            Some(FlashStatus::Failed("gone".into()))
        );
        assert_eq!(
            status(FlashWrite::Idle, None),
            Some(FlashStatus::FactoryProfile)
        );
        assert_eq!(status(FlashWrite::Idle, Some(2)), None);
    }

    #[test]
    fn an_app_profile_is_offered_no_flash_only_action() {
        assert!(picker_extras(true).is_empty());
        let extras = picker_extras(false);
        assert!(extras.contains(&Action::ShowBatteryLevel));
        assert!(extras.contains(&Action::MouseButton(number(6))));
        assert!(
            !extras.contains(&Action::DpiShift),
            "a held DPI shift would run on the selected device, not the G700"
        );
    }
}
