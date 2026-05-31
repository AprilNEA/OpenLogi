//! macOS application menu bar.
//!
//! GPUI's menu support is driven by registered actions + a `Keymap`: the
//! platform layer reads bindings via `cx.set_menus` and stamps the matching
//! `keyEquivalent` onto each `NSMenuItem`. App-level actions (Hide, Quit)
//! get global listeners; window-level actions (Minimize, Zoom) are attached
//! to the root view in [`crate::app`].
//!
//! On Linux/Windows the menus + key bindings are stored but never surfaced
//! in a top-of-screen bar — calling `install` there is a harmless no-op.

use gpui::{App, KeyBinding, Menu, MenuItem, actions};

actions!(
    openlogi,
    [
        /// Close the focused window.
        CloseWindow,
        /// Hide the OpenLogi window (macOS).
        Hide,
        /// Hide every other application (macOS).
        HideOthers,
        /// Minimize the active window.
        Minimize,
        /// Open the About window.
        OpenAbout,
        /// Open the Add Device (pairing) window.
        OpenAddDevice,
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

/// Wire global action handlers, key equivalents, and publish the menu bar.
pub fn install(cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &OpenSettings, cx| crate::windows::settings::open(cx));
    cx.on_action(|_: &OpenAbout, cx| crate::windows::about::open(cx));
    cx.on_action(|_: &OpenAddDevice, cx| crate::windows::add_device::open(cx));

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-h", Hide, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);

    cx.set_menus(menus());
}

/// Re-publish the menu bar with the current locale's titles. Called after a
/// live language switch — unlike [`install`] it only restamps the menu strings,
/// leaving the already-registered action handlers and key bindings untouched.
pub fn rebuild(cx: &mut App) {
    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            // The app menu's name is the product name, not a translatable string.
            name: "OpenLogi".into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("About OpenLogi"), OpenAbout),
                MenuItem::separator(),
                MenuItem::action(tr!("Add Device…"), OpenAddDevice),
                MenuItem::action(tr!("Settings…"), OpenSettings),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("Hide OpenLogi"), Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("Hide Others"), HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action(tr!("Show All"), ShowAll),
                #[cfg(target_os = "macos")]
                MenuItem::separator(),
                MenuItem::action(tr!("Quit OpenLogi"), Quit),
            ],
        },
        Menu {
            name: tr!("Window"),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("Close Window"), CloseWindow),
                MenuItem::separator(),
                MenuItem::action(tr!("Minimize"), Minimize),
                MenuItem::action(tr!("Zoom"), Zoom),
            ],
        },
    ]
}
