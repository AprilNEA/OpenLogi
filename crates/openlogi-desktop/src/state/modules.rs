//! Key modules: what the agent reports about the installed ones, and the
//! `[modules]` switch that turns each on.

use openlogi_core::binding::Action;
use openlogi_core::module::ModuleId;
use openlogi_ipc::{ModuleState, ModuleStatus};

use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    /// Every folder in the modules folder and where its module stands, as
    /// the agent last reported, in folder order.
    #[must_use]
    pub fn modules(&self) -> &[ModuleStatus] {
        &self.modules
    }

    /// Adopt the agent's latest module list.
    pub fn apply_modules(&mut self, modules: Vec<ModuleStatus>) -> StateEvents {
        if self.modules == modules {
            return StateEvents::none();
        }
        self.modules = modules;
        StateEvent::ModulesChanged.into()
    }

    /// Ask the agent for its module list again; the answer arrives through
    /// [`Self::apply_modules`].
    pub fn refresh_modules(&self) {
        self.send_ipc(crate::services::ipc::ReadModules);
    }

    /// Whether `config.toml` turns the module `id` on.
    #[must_use]
    pub fn module_enabled(&self, id: &ModuleId) -> bool {
        self.config.module_enabled(id)
    }

    /// Turn the module `id` on or off: persist it and have the agent reload,
    /// which starts or stops the module. A value already set writes nothing
    /// and is still reported.
    pub fn commit_module_enabled(&mut self, id: &ModuleId, enabled: bool) -> StateEvents {
        if self.config.module_enabled(id) == enabled {
            return StateEvent::SettingsChanged.into();
        }
        self.config
            .edit(|config| config.set_module_enabled(id, enabled));
        if self.persist_and_reload("module setting") {
            self.refresh_modules();
        }
        StateEvent::SettingsChanged.into()
    }

    /// One [`Action::Module`] per installed module that can run here, for a
    /// keypad key's action picker.
    #[must_use]
    pub fn module_actions(&self) -> Vec<Action> {
        self.modules
            .iter()
            .filter(|status| {
                !matches!(
                    status.state,
                    ModuleState::Invalid | ModuleState::Unsupported
                )
            })
            .filter_map(|status| status.module.as_ref())
            .map(|module| Action::Module(module.id.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::config::Config;
    use openlogi_ipc::ModuleInfo;

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
    use crate::state::Sources;

    fn status(folder: &str, id: Option<&str>, state: ModuleState) -> ModuleStatus {
        ModuleStatus {
            folder: folder.into(),
            module: id.map(|id| ModuleInfo {
                id: ModuleId::try_new(id).unwrap(),
                name: format!("{folder} module"),
                description: None,
                version: None,
                platforms: vec!["macos".into()],
            }),
            enabled: false,
            state,
            error: None,
        }
    }

    fn state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let (commands, received) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::new(Sources::in_memory(
            Config::ephemeral(),
            &AssetResolver::new(),
            commands,
        ));
        (state, received)
    }

    #[test]
    fn a_new_module_list_is_announced_once() {
        let (mut state, _commands) = state();
        let list = vec![status(
            "spotify",
            Some("com.example.spotify"),
            ModuleState::Running,
        )];
        assert_eq!(
            state.apply_modules(list.clone()),
            [StateEvent::ModulesChanged]
        );
        assert_eq!(state.apply_modules(list.clone()), StateEvents::none());
        assert_eq!(state.modules(), list.as_slice());
        let label = |id: &str| {
            crate::ui::action::action_label(
                &Action::Module(ModuleId::try_new(id).unwrap()),
                state.modules(),
            )
        };
        assert_eq!(
            label("com.example.spotify"),
            "spotify module",
            "a bound module is labelled by its name"
        );
        assert_eq!(
            label("com.example.gone"),
            "com.example.gone",
            "and one no longer installed by its id"
        );
    }

    #[test]
    fn only_modules_that_can_run_here_are_offered_as_actions() {
        let (mut state, _commands) = state();
        let _ = state.apply_modules(vec![
            status("broken", None, ModuleState::Invalid),
            status(
                "elsewhere",
                Some("com.example.elsewhere"),
                ModuleState::Unsupported,
            ),
            status("off", Some("com.example.off"), ModuleState::Disabled),
            status("on", Some("com.example.on"), ModuleState::Running),
        ]);
        assert_eq!(
            state.module_actions(),
            [
                Action::Module(ModuleId::try_new("com.example.off").unwrap()),
                Action::Module(ModuleId::try_new("com.example.on").unwrap()),
            ]
        );
    }

    #[test]
    fn turning_a_module_on_saves_it_and_asks_the_agent_again() {
        let (mut state, mut commands) = state();
        let id = ModuleId::try_new("com.example.spotify").unwrap();
        while commands.try_recv().is_ok() {}

        assert_eq!(
            state.commit_module_enabled(&id, true),
            [StateEvent::SettingsChanged]
        );
        assert!(state.module_enabled(&id));
        let sent: Vec<Command> = std::iter::from_fn(|| commands.try_recv().ok()).collect();
        assert!(
            sent.iter()
                .any(|command| matches!(command, Command::ReloadConfig(_))),
            "the agent reloads the saved config"
        );
        assert!(
            sent.iter()
                .any(|command| matches!(command, Command::ReadModules(_))),
            "and is asked for the module list again"
        );

        let _ = state.commit_module_enabled(&id, false);
        assert!(!state.module_enabled(&id));
    }
}
