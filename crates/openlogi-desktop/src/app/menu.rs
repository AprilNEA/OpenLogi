//! Application menus and actions.
//!
//! GPUI's menu support is driven by registered actions + a `Keymap`: the
//! platform layer reads bindings via `cx.set_menus` and stamps the matching
//! `keyEquivalent` onto each `NSMenuItem`. Window-level actions wrap the Kit
//! root, so both application content and overlays dispatch through them.
//!
//! On Linux/Windows the menus + key bindings are stored but never surfaced
//! in a top-of-screen bar — calling `install` there is a harmless no-op.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, KeyBinding, Menu, MenuItem,
    OsAction, ParentElement as _, Render, Styled as _, Window, actions, div,
};
use gpui_kit::base::{Root, RootPlugin};
use gpui_kit::component::input;
use openlogi_core::brand::{HELP_URL, RELEASES_URL, REPO_URL};
use url::Url;

use crate::state::AppState;
use crate::ui::battery::value_label;

/// Keymap context installed on the main application surface.
pub(crate) const APP_KEY_CONTEXT: &str = "OpenLogi";
/// Key context of the small task windows (Add Device, update consent), where
/// Escape answers the way their Cancel / Not now button does.
pub(crate) const DIALOG_KEY_CONTEXT: &str = "OpenLogiDialog";

actions!(
    openlogi,
    [
        /// Close the focused window.
        CloseWindow,
        /// Bring all OpenLogi windows to the front.
        BringAllToFront,
        /// Check for an application update.
        CheckForUpdates,
        /// Dismiss a task window the way its Cancel / Not now button does.
        Dismiss,
        /// Hide the OpenLogi window (macOS).
        Hide,
        /// Hide every other application (macOS).
        HideOthers,
        /// Minimize the active window.
        Minimize,
        /// Return from device details to the device gallery.
        NavigateBack,
        /// Open the About window.
        OpenAbout,
        /// Open the Add Device (pairing) window.
        OpenAddDevice,
        /// Open the user's OpenLogi configuration folder.
        OpenConfigFolder,
        /// Open the OpenLogi help page.
        OpenHelp,
        /// Open the latest release page.
        OpenLatestRelease,
        /// Open the OpenLogi GitHub repository.
        OpenRepository,
        /// Open the Settings window.
        OpenSettings,
        /// Quit the application.
        Quit,
        /// Reveal every hidden application (macOS).
        ShowAll,
        /// Zoom (maximize) the active window.
        Zoom,
    ]
);

struct WindowActions;

impl Render for WindowActions {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl RootPlugin for WindowActions {
    fn decorate(
        &self,
        surface: AnyElement,
        _: &Root,
        _: &mut Window,
        _: &mut App,
    ) -> impl IntoElement {
        div()
            .size_full()
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(surface)
    }
}

/// Wire global action handlers, key equivalents, and publish the menu bar.
pub fn install(cx: &mut App) {
    Root::register_plugin(cx, |_, _| WindowActions);
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &OpenSettings, cx| crate::windows::settings::open(cx));
    cx.on_action(|_: &OpenAbout, cx| {
        crate::windows::settings::open_at(crate::windows::settings::SettingsPage::About, cx);
    });
    cx.on_action(|_: &OpenAddDevice, cx| crate::windows::add_device::open(cx));
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &CheckForUpdates, cx| check_for_updates(cx));
    cx.on_action(|_: &OpenConfigFolder, cx| open_config_folder(cx));
    cx.on_action(|_: &OpenHelp, cx| cx.open_url(HELP_URL));
    cx.on_action(|_: &OpenLatestRelease, cx| cx.open_url(RELEASES_URL));
    cx.on_action(|_: &OpenRepository, cx| cx.open_url(REPO_URL));

    // `secondary` is Cmd on macOS and Ctrl elsewhere; a bare `cmd` would be
    // Super on Linux and Windows, which the window manager usually owns.
    // Kit resolves that alias through the host-aware loader, not `new`.
    let secondary_bindings: [(&str, Box<dyn gpui::Action>); 10] = [
        ("secondary-q", Box::new(Quit)),
        ("secondary-z", Box::new(input::Undo)),
        ("secondary-shift-z", Box::new(input::Redo)),
        ("secondary-x", Box::new(input::Cut)),
        ("secondary-c", Box::new(input::Copy)),
        ("secondary-v", Box::new(input::Paste)),
        ("secondary-a", Box::new(input::SelectAll)),
        ("secondary-m", Box::new(Minimize)),
        ("secondary-w", Box::new(CloseWindow)),
        ("secondary-,", Box::new(OpenSettings)),
    ];
    #[expect(
        clippy::expect_used,
        reason = "the bindings above are fixed, valid literals"
    )]
    let secondary_bindings = secondary_bindings
        .into_iter()
        .map(|(key, action)| {
            KeyBinding::load(key, action, None, false, None, cx)
                .expect("static application shortcuts are valid for the host OS")
        })
        .collect::<Vec<_>>();
    cx.bind_keys(secondary_bindings);
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-h", Hide, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("alt-left", NavigateBack, Some(APP_KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(DIALOG_KEY_CONTEXT)),
    ]);

    cx.set_menus(menus(cx));
}

/// Re-publish the menu bar with the current locale's titles. Called after a
/// live language switch — unlike [`install`] it only restamps the menu strings,
/// leaving the already-registered action handlers and key bindings untouched.
pub fn rebuild(cx: &mut App) {
    cx.set_menus(menus(cx));
}

/// Run a manual update check and show its progress: Sparkle in its own window,
/// the built-in engine on Settings → Updates. Shared by the app menu, agent
/// tray IPC commands and deep links.
pub fn check_for_updates(cx: &mut App) {
    let updater = crate::platform::updater::shared(cx);
    if let Some(updater) = &updater {
        updater.update(cx, gpui_updater::Updater::check);
    }
    if !updater.is_some_and(|updater| updater.read(cx).is_driver_backed()) {
        crate::windows::settings::open_at(crate::windows::settings::SettingsPage::Updates, cx);
    }
}

fn menus(cx: &App) -> Vec<Menu> {
    vec![
        Menu {
            // The app menu's name is the product name, not a translatable string.
            name: "OpenLogi".into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("about.about_openlogi"), OpenAbout),
                MenuItem::action(tr!("updates.check_for_updates_dialog"), CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action(tr!("app.settings_dialog"), OpenSettings),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu(tr!("app.services"), gpui::SystemMenuType::Services),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("app.hide_openlogi"), Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("app.hide_others"), HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("common.show_all"), ShowAll),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                MenuItem::action(tr!("app.quit_openlogi"), Quit),
            ],
        },
        Menu {
            name: tr!("common.edit"),
            disabled: false,
            items: vec![
                MenuItem::os_action(tr!("common.undo"), input::Undo, OsAction::Undo),
                MenuItem::os_action(tr!("common.redo"), input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action(tr!("common.cut"), input::Cut, OsAction::Cut),
                MenuItem::os_action(tr!("common.copy"), input::Copy, OsAction::Copy),
                MenuItem::os_action(tr!("common.paste"), input::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::os_action(
                    tr!("common.select_all"),
                    input::SelectAll,
                    OsAction::SelectAll,
                ),
            ],
        },
        Menu {
            name: tr!("common.view"),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("app.settings_dialog"), OpenSettings),
                MenuItem::action(tr!("about.about_openlogi"), OpenAbout),
                MenuItem::separator(),
                MenuItem::action(tr!("app.open_configuration_folder"), OpenConfigFolder),
            ],
        },
        Menu {
            name: tr!("device.device"),
            disabled: false,
            items: device_menu_items(cx),
        },
        Menu {
            name: tr!("app.window"),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("common.close_window"), CloseWindow),
                MenuItem::separator(),
                MenuItem::action(tr!("common.minimize"), Minimize),
                MenuItem::action(tr!("common.zoom"), Zoom),
                MenuItem::separator(),
                MenuItem::action(tr!("app.bring_all_to_front"), BringAllToFront),
            ],
        },
        Menu {
            name: tr!("common.help"),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("about.openlogi_help"), OpenHelp),
                MenuItem::separator(),
                MenuItem::action(tr!("about.open_github_repository"), OpenRepository),
                MenuItem::action(tr!("about.latest_release"), OpenLatestRelease),
            ],
        },
    ]
}

fn device_menu_items(cx: &App) -> Vec<MenuItem> {
    let state = AppState::try_read(cx);
    // Pairing runs in the agent: like the window's Add Device buttons, the
    // item waits for a live link. The menu follows the link because every
    // `StateEvent::AgentChanged` republishes it.
    let agent_live = state.is_some_and(super::widgets::agent_live);
    let mut items = vec![
        MenuItem::action(tr!("pairing.add_device_dialog"), OpenAddDevice).disabled(!agent_live),
        MenuItem::separator(),
    ];

    match state {
        Some(state) if !state.devices().is_empty() => {
            for record in state.devices() {
                let title = match &record.battery {
                    Some(battery) => format!("{} · {}", record.display_name, value_label(battery)),
                    None => record.display_name.clone(),
                };
                items.push(MenuItem::action(title, gpui::NoAction).disabled(true));
            }
        }
        _ => {
            items.push(
                MenuItem::action(tr!("device.no_devices_connected"), gpui::NoAction).disabled(true),
            );
        }
    }

    items
}

pub(crate) fn file_url(path: &std::path::Path) -> Option<String> {
    Url::from_file_path(path).ok().map(Into::into)
}

/// Reveal the user's configuration directory in the OS file manager.
///
/// Every in-window button that offers this calls here directly instead of
/// dispatching [`OpenConfigFolder`]. `App::dispatch_action` routes through
/// the *active* window and drops the action when that window cannot be
/// resolved — it logs "window not found" and swallows the error — which left
/// the button on the fail-closed configuration-error screen doing nothing at
/// all. Only the menu bar, which has no view to call from, goes through the
/// action.
pub(crate) fn open_config_folder(cx: &mut App) {
    if let Ok(path) = openlogi_core::paths::config_dir()
        && let Some(url) = file_url(&path)
    {
        cx.open_url(&url);
    }
}
