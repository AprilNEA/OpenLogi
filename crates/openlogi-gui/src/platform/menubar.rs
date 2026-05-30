//! macOS menu-bar (`NSStatusItem`) presence via raw Cocoa FFI — GPUI exposes
//! no status-bar API. Menu clicks can't reach GPUI's `App`, so they post a
//! [`MenuBarEvent`] on a channel the `main.rs` watcher loop drains.

/// A request raised by clicking a status-bar menu item, or by a live language
/// switch asking the spawn loop to re-localize the whole menu.
#[derive(Debug, Clone, Copy)]
pub enum MenuBarEvent {
    Open,
    Quit,
    /// Re-title Open/Quit *and* the device line for the current locale.
    Refresh,
}

#[cfg(target_os = "macos")]
pub use macos::{MenuBarHandle, install, refresh_labels, request_refresh};
#[cfg(not(target_os = "macos"))]
pub use stub::{MenuBarHandle, install, refresh_labels, request_refresh};

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "Cocoa NSStatusItem/NSMenu FFI; GPUI has no menu-bar API"
)]
mod macos {
    use std::sync::{Once, OnceLock};

    use cocoa::base::{NO, YES, id, nil};
    use cocoa::foundation::NSString;
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use tokio::sync::mpsc;
    use tracing::warn;

    use super::MenuBarEvent;

    const VARIABLE_LENGTH: f64 = -1.0;
    const ACTIVATION_POLICY_ACCESSORY: i64 = 1;
    const TARGET_CLASS: &str = "OpenLogiMenuTarget";

    // Read by the Objective-C action callbacks, which can't capture state.
    static MENU_TX: OnceLock<mpsc::UnboundedSender<MenuBarEvent>> = OnceLock::new();

    /// Open/Quit item pointers, kept so a live locale switch can re-title them.
    /// Stored as `usize` because a raw `id` is not `Sync`.
    static MENU_REFS: OnceLock<MenuRefs> = OnceLock::new();

    struct MenuRefs {
        open: usize,
        quit: usize,
    }

    /// Cocoa objects retained for the app's lifetime; only ever touched on the
    /// main thread, so the raw `id`s never cross threads.
    pub struct MenuBarHandle {
        device_item: id,
        _status_item: id,
        _target: id,
        _menu: id,
    }

    impl MenuBarHandle {
        /// Update the device line, e.g. `"MX Master 3S · 80%"`. Main thread only.
        pub fn set_device_status(&self, text: &str) {
            unsafe {
                let title = nsstring(text);
                let _: () = msg_send![self.device_item, setTitle: title];
            }
        }
    }

    /// Install the status item and switch to accessory activation, dropping the
    /// app from the Dock and app switcher.
    #[must_use]
    pub fn install(tx: mpsc::UnboundedSender<MenuBarEvent>) -> MenuBarHandle {
        let _ = MENU_TX.set(tx);
        ensure_target_class();

        unsafe {
            let app: id = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, setActivationPolicy: ACTIVATION_POLICY_ACCESSORY];

            let status_bar: id = msg_send![class!(NSStatusBar), systemStatusBar];
            let status_item: id = msg_send![status_bar, statusItemWithLength: VARIABLE_LENGTH];
            let _: id = msg_send![status_item, retain];

            let button: id = msg_send![status_item, button];
            set_button_icon(button);

            let target_cls = Class::get(TARGET_CLASS).unwrap_or_else(|| class!(NSObject));
            let target: id = msg_send![target_cls, new];

            let menu: id = msg_send![class!(NSMenu), new];
            let _: () = msg_send![menu, setAutoenablesItems: NO];

            let device_item: id = msg_send![class!(NSMenuItem), new];
            let idle = rust_i18n::t!("No device connected");
            let _: () = msg_send![device_item, setTitle: nsstring(&idle)];
            let _: () = msg_send![device_item, setEnabled: NO];
            let _: () = msg_send![menu, addItem: device_item];

            let separator: id = msg_send![class!(NSMenuItem), separatorItem];
            let _: () = msg_send![menu, addItem: separator];

            let open_title = rust_i18n::t!("Open OpenLogi");
            let open_item = action_item(&open_title, sel!(openOpenLogi:), target);
            let _: () = msg_send![menu, addItem: open_item];
            let quit_title = rust_i18n::t!("Quit OpenLogi");
            let quit_item = action_item(&quit_title, sel!(quitOpenLogi:), target);
            let _: () = msg_send![menu, addItem: quit_item];

            let _ = MENU_REFS.set(MenuRefs {
                open: open_item as usize,
                quit: quit_item as usize,
            });

            let _: () = msg_send![status_item, setMenu: menu];

            MenuBarHandle {
                device_item,
                _status_item: status_item,
                _target: target,
                _menu: menu,
            }
        }
    }

    /// Re-title the Open/Quit items for the current locale. Main-thread only,
    /// like every status-item write. The device line is owned by the spawn
    /// loop's [`MenuBarHandle`], so [`request_refresh`] drives that side; this
    /// only touches the items reachable through `MENU_REFS`.
    pub fn refresh_labels() {
        let Some(refs) = MENU_REFS.get() else {
            return;
        };
        let open_title = rust_i18n::t!("Open OpenLogi");
        let quit_title = rust_i18n::t!("Quit OpenLogi");
        unsafe {
            let open = refs.open as id;
            let quit = refs.quit as id;
            let _: () = msg_send![open, setTitle: nsstring(&open_title)];
            let _: () = msg_send![quit, setTitle: nsstring(&quit_title)];
        }
    }

    /// Ask the spawn loop to re-localize the whole menu after a live language
    /// switch. The device line is recomputed from the live `AppState`, which
    /// only the loop can read, so we post through the same channel as menu
    /// clicks instead of writing the status item from the settings view.
    pub fn request_refresh() {
        post(MenuBarEvent::Refresh);
    }

    fn nsstring(s: &str) -> id {
        unsafe { NSString::alloc(nil).init_str(s) }
    }

    fn action_item(title: &str, action: Sel, target: id) -> id {
        unsafe {
            let item: id = msg_send![class!(NSMenuItem), alloc];
            let item: id = msg_send![item, initWithTitle: nsstring(title) action: action keyEquivalent: nsstring("")];
            let _: () = msg_send![item, setTarget: target];
            item
        }
    }

    // Template image adapts to the light/dark menu bar; text title as fallback.
    fn set_button_icon(button: id) {
        unsafe {
            let symbol = nsstring("computermouse.fill");
            let description = nsstring("OpenLogi");
            let image: id = msg_send![class!(NSImage), imageWithSystemSymbolName: symbol accessibilityDescription: description];
            if image == nil {
                let _: () = msg_send![button, setTitle: nsstring("OpenLogi")];
            } else {
                let _: () = msg_send![image, setTemplate: YES];
                let _: () = msg_send![button, setImage: image];
            }
        }
    }

    extern "C" fn open_action(_this: &Object, _cmd: Sel, _sender: id) {
        post(MenuBarEvent::Open);
    }

    extern "C" fn quit_action(_this: &Object, _cmd: Sel, _sender: id) {
        post(MenuBarEvent::Quit);
    }

    fn post(event: MenuBarEvent) {
        if let Some(tx) = MENU_TX.get()
            && tx.send(event).is_err()
        {
            warn!(?event, "menu-bar event dropped — GPUI loop gone");
        }
    }

    fn ensure_target_class() {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            if let Some(mut decl) = ClassDecl::new(TARGET_CLASS, class!(NSObject)) {
                unsafe {
                    decl.add_method(
                        sel!(openOpenLogi:),
                        open_action as extern "C" fn(&Object, Sel, id),
                    );
                    decl.add_method(
                        sel!(quitOpenLogi:),
                        quit_action as extern "C" fn(&Object, Sel, id),
                    );
                }
                decl.register();
            }
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod stub {
    use tokio::sync::mpsc;

    use super::MenuBarEvent;

    pub struct MenuBarHandle;

    impl MenuBarHandle {
        pub fn set_device_status(&self, _text: &str) {}
    }

    #[must_use]
    pub fn install(_tx: mpsc::UnboundedSender<MenuBarEvent>) -> MenuBarHandle {
        MenuBarHandle
    }

    pub fn refresh_labels() {}

    pub fn request_refresh() {}
}
