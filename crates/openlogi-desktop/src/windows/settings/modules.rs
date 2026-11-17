//! Key modules page: the installed modules, a switch to turn each on, and
//! the way to the modules folder.

use gpui::{InteractiveElement as _, ParentElement as _, SharedString};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{Icon, Sizable as _};
use openlogi_core::binding::ActionRingIcon;
use openlogi_core::module::ModuleId;
use openlogi_ipc::{ModuleState, ModuleStatus};
use tracing::warn;

use super::assets::action_button;
use super::{AppState, SettingField, SettingGroup, SettingItem, SettingPage};

/// The glyph a module's key shows before the module draws it, standing for
/// modules on the page too.
const MODULE_ICON: &str = ActionRingIcon::Module.asset_path();

pub(super) fn modules_page(modules: &[ModuleStatus]) -> SettingPage {
    let installed = if modules.is_empty() {
        SettingGroup::new().item(
            SettingItem::new(
                tr!("modules.none_installed"),
                SettingField::render(|_, _, _| gpui::div()),
            )
            .description(tr!("modules.none_installed_description")),
        )
    } else {
        SettingGroup::new().items(modules.iter().map(module_item))
    };
    let folder = SettingGroup::new().item(
        SettingItem::new(
            tr!("modules.folder"),
            SettingField::render(|_, _, cx| {
                let pal = crate::ui::theme::palette(cx);
                action_button("modules-open-folder", tr!("common.open"), pal, |_| {
                    open_modules_folder();
                })
            }),
        )
        .description(tr!("modules.security_note")),
    );

    SettingPage::new(tr!("modules.modules"))
        .icon(Icon::empty().path(MODULE_ICON))
        .resettable(false)
        .group(installed)
        .group(folder)
}

/// One module's row: its name, what it does and where it stands, and the
/// switch that turns it on when it can run here.
fn module_item(status: &ModuleStatus) -> SettingItem {
    let Some(module) = &status.module else {
        return SettingItem::new(
            SharedString::from(status.folder.clone()),
            SettingField::render(|_, _, _| gpui::div()),
        )
        .description(state_line(status));
    };
    let description = match &module.description {
        Some(text) => SharedString::from(format!("{text} {}", state_line(status))),
        None => state_line(status),
    };
    let item = if matches!(
        status.state,
        ModuleState::Invalid | ModuleState::Unsupported
    ) {
        SettingItem::new(
            SharedString::from(module.name.clone()),
            SettingField::render(|_, _, _| gpui::div()),
        )
    } else {
        let id = module.id.clone();
        let name = SharedString::from(module.name.clone());
        SettingItem::new(
            name.clone(),
            SettingField::render(move |options, _, cx| {
                enabled_checkbox(&id, name.clone(), options.size(), cx)
            }),
        )
    };
    item.description(description)
        .keywords([SharedString::from(module.id.to_string())])
}

/// The checkbox that turns the module `id`, called `name`, on and off. Keyed
/// by the module's id, so a row keeps its identity as modules come and go.
fn enabled_checkbox(
    id: &ModuleId,
    name: SharedString,
    size: gpui_kit::component::Size,
    cx: &gpui::App,
) -> gpui::Div {
    let checked = AppState::try_read(cx).is_some_and(|state| state.module_enabled(id));
    let selector = format!("module-enabled-{id}");
    let id = id.clone();
    gpui::div().debug_selector(move || selector).child(
        Checkbox::new(SharedString::from(format!("module-enabled-{id}")))
            .accessibility_label(name)
            .checked(checked)
            .with_size(size)
            .on_click(move |enabled: &bool, _, cx| {
                AppState::apply(cx, |state| state.commit_module_enabled(&id, *enabled));
            }),
    )
}

/// Where the module stands, as one sentence.
fn state_line(status: &ModuleStatus) -> SharedString {
    let error = status.error.clone().unwrap_or_default();
    match status.state {
        ModuleState::Invalid => tr!("modules.state_invalid", error => error),
        ModuleState::Unsupported => tr!("modules.state_unsupported"),
        ModuleState::Disabled => tr!("modules.state_off"),
        ModuleState::Starting => tr!("modules.state_starting"),
        ModuleState::Running => tr!("modules.state_running"),
        ModuleState::Restarting if error.is_empty() => tr!("modules.state_restarting"),
        ModuleState::Restarting => tr!("modules.state_restarting_after", error => error),
        ModuleState::Failed => tr!("modules.state_failed", error => error),
    }
}

/// Open the modules folder in the file manager, creating it first so a user
/// with no modules yet has somewhere to put one.
fn open_modules_folder() {
    let dir = match openlogi_core::paths::modules_dir() {
        Ok(dir) => dir,
        Err(error) => {
            warn!(%error, "no modules folder to open");
            return;
        }
    };
    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!(%error, path = %dir.display(), "could not create the modules folder");
        return;
    }
    if let Err(error) = opener::open(&dir) {
        warn!(%error, path = %dir.display(), "could not open the modules folder");
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, px};
    use openlogi_core::config::Config;
    use openlogi_ipc::ModuleInfo;

    use super::*;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::Sources;
    use crate::windows::settings::{SettingsPage, SettingsView};

    fn status(state: ModuleState, error: Option<&str>) -> ModuleStatus {
        ModuleStatus {
            folder: "spotify".into(),
            module: Some(ModuleInfo {
                id: ModuleId::try_new("com.example.spotify").unwrap(),
                name: "Spotify".into(),
                description: None,
                version: None,
                platforms: vec!["macos".into()],
            }),
            enabled: true,
            state,
            error: error.map(str::to_owned),
        }
    }

    /// Every state reads as its own sentence, carrying the error where one
    /// explains it.
    #[test]
    fn each_state_has_its_own_line() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let lines = [
            state_line(&status(ModuleState::Invalid, Some("no module.toml"))),
            state_line(&status(ModuleState::Unsupported, None)),
            state_line(&status(ModuleState::Disabled, None)),
            state_line(&status(ModuleState::Starting, None)),
            state_line(&status(ModuleState::Running, None)),
            state_line(&status(ModuleState::Restarting, None)),
            state_line(&status(
                ModuleState::Restarting,
                Some("exited with status 1"),
            )),
            state_line(&status(ModuleState::Failed, Some("did not answer hello"))),
        ];
        for (index, line) in lines.iter().enumerate() {
            assert!(!line.contains("modules."), "untranslated key: {line}");
            assert!(!line.contains("%{"), "unfilled placeholder: {line}");
            assert!(
                !lines[..index].contains(line),
                "two states read the same: {line}"
            );
        }
        assert!(lines[0].contains("no module.toml"));
        assert!(lines[6].contains("exited with status 1"));
        assert!(lines[7].contains("did not answer hello"));
    }

    /// The Modules page lists every folder, offers a checkbox only for a
    /// module that can run here, and a click on it turns the module on in
    /// the config the agent reloads.
    #[gpui::test]
    fn the_modules_page_turns_a_module_on(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
            AppState::apply(cx, |state| {
                state.apply_modules(vec![
                    ModuleStatus {
                        folder: "broken".into(),
                        module: None,
                        enabled: false,
                        state: ModuleState::Invalid,
                        error: Some("no module.toml".into()),
                    },
                    status(ModuleState::Disabled, None),
                ])
            });
        });
        let handle = cx.open_window(gpui::size(px(920.), px(640.)), |window, cx| {
            let settings = cx.new(|cx| SettingsView::new(SettingsPage::Modules, window, cx));
            gpui_kit::component::Root::new(settings, window, cx)
        });
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        while sent.try_recv().is_ok() {}

        let checkbox = visual
            .debug_bounds("module-enabled-com.example.spotify")
            .expect("a module that can run here has a checkbox");
        visual.simulate_click(checkbox.center(), Modifiers::default());
        let id = ModuleId::try_new("com.example.spotify").unwrap();
        visual.update(|_, cx| {
            assert!(AppState::global(cx).read(cx).module_enabled(&id));
        });
        let commands: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok()).collect();
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, crate::services::ipc::Command::ReloadConfig(_))),
            "the agent is asked to reload the config that turns it on"
        );

        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.simulate_click(checkbox.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert!(!AppState::global(cx).read(cx).module_enabled(&id));
        });

        rust_i18n::set_locale("zh-CN");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        rust_i18n::set_locale("en");
    }
}
