//! System-tray / status-item presence. macOS-only today, via `NSStatusItem`
//! (which lives in the menu bar) over raw Cocoa FFI — GPUI exposes no
//! status-bar API.
//!
//! `tray` is the cross-platform-neutral name: macOS has the menu-bar status
//! item, Windows the system tray / notification area, Linux the
//! StatusNotifierItem spec. Only macOS is implemented, so the module carries no
//! stub — every caller gates on `cfg(target_os = "macos")` instead.
//!
//! Menu clicks can't reach GPUI's `App`, so they post a [`TrayEvent`] on a
//! channel that a dedicated task in `main.rs` drains.

#[cfg(target_os = "macos")]
pub use macos::{
    TrayEvent, hide_from_dock, install, refresh_labels, request_refresh, set_device_status,
    set_visible, show_in_dock, uninstall,
};

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    };

    use cocoa::base::id;
    use objc::runtime::{Object, Sel};
    use objc::{sel, sel_impl};
    use tokio::sync::mpsc;
    use tracing::warn;

    use super::super::status_item::{
        self, ActionCallback, ActionTarget, ActivationPolicy, Menu, MenuItem, StatusItem,
    };

    /// A request raised by clicking a status-bar menu item, or by a live
    /// language switch asking the drain task to re-localize the whole menu.
    #[derive(Debug, Clone, Copy)]
    pub enum TrayEvent {
        Open,
        Quit,
        /// Re-title Open/Quit *and* the device line for the current locale.
        Refresh,
    }

    const TARGET_CLASS: &str = "OpenLogiMenuTarget";

    // Read by the Objective-C action callbacks, which can't capture state.
    static MENU_TX: OnceLock<mpsc::UnboundedSender<TrayEvent>> = OnceLock::new();

    /// Open/Quit item pointers, kept so a live locale switch can re-title them.
    /// Stored as opaque menu-item handles; only touched on the main thread.
    static MENU_REFS: OnceLock<MenuRefs> = OnceLock::new();

    /// The device-status line item, written by [`set_device_status`]. Only ever
    /// touched on the main thread.
    static DEVICE_ITEM: OnceLock<MenuItem> = OnceLock::new();

    /// The `NSStatusItem` itself, so [`set_visible`] can show / hide the icon.
    static STATUS_ITEM: OnceLock<StatusItem> = OnceLock::new();

    /// Whether the status item is currently installed in `NSStatusBar`.
    static INSTALLED: AtomicBool = AtomicBool::new(false);

    struct MenuRefs {
        open: MenuItem,
        quit: MenuItem,
    }

    struct InstalledMenu {
        menu: Menu,
        refs: MenuRefs,
        device_item: MenuItem,
    }

    /// Install the status item. Main thread only.
    ///
    /// The activation policy (Dock + menu-bar visibility) is *not* set here —
    /// [`show_in_dock`] / [`hide_from_dock`] manage it as windows open and
    /// close. The status item, its menu, and the click target are all retained
    /// for the app's lifetime (a status item lives as long as the process); the
    /// target in particular *must* be retained, since `NSMenuItem` keeps only a
    /// weak reference to it.
    pub fn install(tx: mpsc::UnboundedSender<TrayEvent>) {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return;
        }

        let _ = MENU_TX.set(tx);

        let status_item = StatusItem::new();
        let _ = STATUS_ITEM.set(status_item);
        status_item.set_symbol_icon("computermouse.fill", "OpenLogi", "OpenLogi");

        let installed_menu = build_menu();
        let _ = DEVICE_ITEM.set(installed_menu.device_item);
        let _ = MENU_REFS.set(installed_menu.refs);
        status_item.set_menu(installed_menu.menu);
    }

    /// Remove the status item from the system status bar during app teardown.
    ///
    /// `NSStatusItem`s normally disappear when the process exits, but GPUI's
    /// graceful quit can leave background workers winding down briefly. Removing
    /// it explicitly avoids a stale, non-clickable menu-bar gap during teardown
    /// and makes repeated calls harmless.
    pub fn uninstall() {
        if !INSTALLED.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(item) = STATUS_ITEM.get() else {
            return;
        };
        item.remove_from_status_bar();
    }

    fn build_menu() -> InstalledMenu {
        let target = action_target();
        let menu = Menu::new();

        let idle = rust_i18n::t!("No devices connected");
        let device_item = MenuItem::disabled(&idle);
        menu.add_item(device_item);

        menu.add_separator();

        let open_selector = sel!(openOpenLogi:);
        let quit_selector = sel!(quitOpenLogi:);
        let open_title = rust_i18n::t!("Open OpenLogi");
        let open_item = MenuItem::action(&open_title, open_selector, &target);
        menu.add_item(open_item);
        let quit_title = rust_i18n::t!("Quit OpenLogi");
        let quit_item = MenuItem::action(&quit_title, quit_selector, &target);
        menu.add_item(quit_item);

        InstalledMenu {
            menu,
            refs: MenuRefs {
                open: open_item,
                quit: quit_item,
            },
            device_item,
        }
    }

    fn action_target() -> ActionTarget {
        let open_selector = sel!(openOpenLogi:);
        let quit_selector = sel!(quitOpenLogi:);
        let target_methods = [
            (open_selector, open_action as ActionCallback),
            (quit_selector, quit_action as ActionCallback),
        ];
        ActionTarget::new(TARGET_CLASS, &target_methods)
    }

    /// Show the app in the Dock + menu bar — called when a window opens, so the
    /// app menu (⌘Q, Settings, …) is available while the window is up.
    pub fn show_in_dock() {
        status_item::set_activation_policy(ActivationPolicy::Regular);
    }

    /// Drop the app out of the Dock + menu bar, leaving only the status item —
    /// called when the last window closes (and on a `--minimized` launch).
    pub fn hide_from_dock() {
        status_item::set_activation_policy(ActivationPolicy::Accessory);
    }

    /// Show or hide the status-item icon without tearing it down — backs the
    /// "Show in menu bar" setting. A no-op until [`install`] has run.
    pub fn set_visible(visible: bool) {
        let Some(item) = STATUS_ITEM.get() else {
            return;
        };
        item.set_visible(visible);
    }

    /// Update the device line, e.g. `"MX Master 3S · 80%"`. Main thread only.
    /// A no-op until [`install`] has published the item.
    pub fn set_device_status(text: &str) {
        let Some(item) = DEVICE_ITEM.get() else {
            return;
        };
        item.set_title(text);
    }

    /// Re-title the Open/Quit items for the current locale. Main-thread only,
    /// like every status-item write. The device line is refreshed separately via
    /// [`set_device_status`].
    pub fn refresh_labels() {
        let Some(refs) = MENU_REFS.get() else {
            return;
        };
        let open_title = rust_i18n::t!("Open OpenLogi");
        let quit_title = rust_i18n::t!("Quit OpenLogi");
        refs.open.set_title(&open_title);
        refs.quit.set_title(&quit_title);
    }

    /// Ask the drain task to re-localize the whole menu after a live language
    /// switch. Posts through the same channel as menu clicks so the device line
    /// (recomputed from the live `AppState`, which only the task can read) is
    /// rewritten on the main thread alongside the static labels.
    pub fn request_refresh() {
        post(TrayEvent::Refresh);
    }

    extern "C" fn open_action(_this: &Object, _cmd: Sel, _sender: id) {
        post(TrayEvent::Open);
    }

    extern "C" fn quit_action(_this: &Object, _cmd: Sel, _sender: id) {
        post(TrayEvent::Quit);
    }

    fn post(event: TrayEvent) {
        if let Some(tx) = MENU_TX.get()
            && tx.send(event).is_err()
        {
            warn!(?event, "menu-bar event dropped — GPUI loop gone");
        }
    }
}
