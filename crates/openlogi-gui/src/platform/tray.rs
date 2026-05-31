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
pub use macos::{TrayEvent, install, refresh_labels, request_refresh, set_device_status};

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

    /// A request raised by clicking a status-bar menu item, or by a live
    /// language switch asking the drain task to re-localize the whole menu.
    #[derive(Debug, Clone, Copy)]
    pub enum TrayEvent {
        Open,
        Quit,
        /// Re-title Open/Quit *and* the device line for the current locale.
        Refresh,
    }

    const VARIABLE_LENGTH: f64 = -1.0;
    const ACTIVATION_POLICY_ACCESSORY: i64 = 1;
    const TARGET_CLASS: &str = "OpenLogiMenuTarget";

    // Read by the Objective-C action callbacks, which can't capture state.
    static MENU_TX: OnceLock<mpsc::UnboundedSender<TrayEvent>> = OnceLock::new();

    /// Open/Quit item pointers, kept so a live locale switch can re-title them.
    /// Stored as `usize` because a raw `id` is not `Sync`.
    static MENU_REFS: OnceLock<MenuRefs> = OnceLock::new();

    /// The device-status line item, written by [`set_device_status`]. Stored as
    /// `usize` (a raw `id` is not `Sync`); only ever touched on the main thread.
    static DEVICE_ITEM: OnceLock<usize> = OnceLock::new();

    struct MenuRefs {
        open: usize,
        quit: usize,
    }

    /// Install the status item and switch to accessory activation, dropping the
    /// app from the Dock and app switcher. Main thread only.
    ///
    /// The status item, its menu, and the click target are all retained for the
    /// app's lifetime (a status item lives as long as the process). The target
    /// in particular *must* be retained: `NSMenuItem` does not retain its
    /// target, so without this the action callbacks would fire into freed
    /// memory.
    pub fn install(tx: mpsc::UnboundedSender<TrayEvent>) {
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
            // NSMenuItem keeps only a weak reference to its target — retain it so
            // it outlives this function and the action callbacks stay valid.
            let _: id = msg_send![target, retain];

            let menu: id = msg_send![class!(NSMenu), new];
            let _: id = msg_send![menu, retain];
            let _: () = msg_send![menu, setAutoenablesItems: NO];

            let device_item: id = msg_send![class!(NSMenuItem), new];
            let idle = rust_i18n::t!("No device connected");
            let _: () = msg_send![device_item, setTitle: nsstring(&idle)];
            let _: () = msg_send![device_item, setEnabled: NO];
            let _: () = msg_send![menu, addItem: device_item];
            let _ = DEVICE_ITEM.set(device_item as usize);

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
        }
    }

    /// Update the device line, e.g. `"MX Master 3S · 80%"`. Main thread only.
    /// A no-op until [`install`] has published the item.
    pub fn set_device_status(text: &str) {
        let Some(item) = DEVICE_ITEM.get() else {
            return;
        };
        unsafe {
            let title = nsstring(text);
            let _: () = msg_send![*item as id, setTitle: title];
        }
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
        unsafe {
            let open = refs.open as id;
            let quit = refs.quit as id;
            let _: () = msg_send![open, setTitle: nsstring(&open_title)];
            let _: () = msg_send![quit, setTitle: nsstring(&quit_title)];
        }
    }

    /// Ask the drain task to re-localize the whole menu after a live language
    /// switch. Posts through the same channel as menu clicks so the device line
    /// (recomputed from the live `AppState`, which only the task can read) is
    /// rewritten on the main thread alongside the static labels.
    pub fn request_refresh() {
        post(TrayEvent::Refresh);
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
