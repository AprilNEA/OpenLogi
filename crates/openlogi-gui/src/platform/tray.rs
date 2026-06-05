//! System-tray / status-item presence. macOS-only today, via `NSStatusItem`
//! (which lives in the menu bar) over `objc2` — GPUI exposes no status-bar API.
//!
//! `tray` is the cross-platform-neutral name: macOS has the menu-bar status
//! item, Windows the system tray / notification area, Linux the
//! StatusNotifierItem spec. Only macOS is implemented, so the module carries no
//! stub — every caller gates on `cfg(target_os = "macos")` instead.
//!
//! All AppKit objects are owned as `Retained<T>` (issue #99: the old raw-`id`
//! path leaked a `CFString` on every refresh). `NSMenu`/`NSMenuItem` are
//! `MainThreadOnly`, hence `!Send`, so the tray's state lives in a main-thread
//! `thread_local` rather than a `Sync` static — the "main thread only" contract
//! is now enforced by the type system instead of a doc comment.
//!
//! Menu clicks can't reach GPUI's `App`, so the [`MenuTarget`] action methods
//! post a [`TrayEvent`] on a channel that a dedicated task in `main.rs` drains.

#[cfg(target_os = "macos")]
pub use macos::{
    TrayEvent, hide_from_dock, install, refresh_labels, request_refresh, set_device_lines,
    set_visible, show_in_dock, uninstall,
};

#[cfg(target_os = "macos")]
mod macos {
    #![expect(
        unsafe_code,
        reason = "define_class! menu-target subclass + its super-init; GPUI has no menu-bar API"
    )]

    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSMenuItem, NSStatusItem};
    use objc2_foundation::NSString;
    use tokio::sync::mpsc;
    use tracing::warn;

    use super::super::status_item::{self, ActivationPolicy};

    /// A request raised by clicking a status-bar menu item, or by a live
    /// language switch asking the drain task to re-localize the whole menu.
    #[derive(Debug, Clone, Copy)]
    pub enum TrayEvent {
        Open,
        Quit,
        /// Re-title Open/Quit *and* the device line for the current locale.
        Refresh,
    }

    /// How many device rows the tray menu can show at once.
    const MAX_DEVICE_ROWS: usize = 8;

    /// Instance state for [`MenuTarget`]: the channel back to the drain task.
    struct MenuTargetIvars {
        tx: mpsc::UnboundedSender<TrayEvent>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and `MenuTarget`
        // does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "OpenLogiMenuTarget"]
        #[ivars = MenuTargetIvars]
        struct MenuTarget;

        impl MenuTarget {
            #[unsafe(method(openOpenLogi:))]
            fn open_openlogi(&self, _sender: Option<&AnyObject>) {
                self.post(TrayEvent::Open);
            }

            #[unsafe(method(quitOpenLogi:))]
            fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
                self.post(TrayEvent::Quit);
            }
        }
    );

    impl MenuTarget {
        fn new(mtm: MainThreadMarker, tx: mpsc::UnboundedSender<TrayEvent>) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { tx });
            // SAFETY: `init` initializes our freshly-allocated, ivar-set NSObject
            // subclass and returns it — the two-phase construction objc2's
            // `define_class!` is designed around.
            unsafe { msg_send![super(this), init] }
        }

        fn post(&self, event: TrayEvent) {
            if self.ivars().tx.send(event).is_err() {
                warn!(?event, "menu-bar event dropped — GPUI loop gone");
            }
        }
    }

    /// Every retained AppKit object the tray needs. `MainThreadOnly` objects are
    /// `!Send`, so this can only live in a main-thread `thread_local`.
    struct TrayState {
        status_item: Retained<NSStatusItem>,
        /// Open/Quit items, kept so a live locale switch can re-title them.
        open_item: Retained<NSMenuItem>,
        quit_item: Retained<NSMenuItem>,
        /// One per device row; spare rows hidden.
        device_items: Vec<Retained<NSMenuItem>>,
        /// A clone of the click channel, so `request_refresh` can post without
        /// reaching into the (weakly-referenced) target.
        sender: mpsc::UnboundedSender<TrayEvent>,
        #[expect(
            dead_code,
            reason = "kept alive: NSMenuItem stores only a weak reference to its target"
        )]
        target: Retained<MenuTarget>,
    }

    thread_local! {
        /// The live tray, or `None` before [`install`] / after [`uninstall`].
        /// Main-thread only by construction (it holds `!Send` AppKit objects).
        static TRAY: RefCell<Option<TrayState>> = const { RefCell::new(None) };
    }

    /// Debug-time guard for the tray mutators: GPUI drives them on the main
    /// thread. A future off-main caller is a bug — it would silently no-op
    /// (the `!Send` `TrayState` only exists in the main thread's TLS), so make
    /// that loud in debug builds while staying free in release.
    #[inline]
    fn debug_assert_main_thread() {
        debug_assert!(
            MainThreadMarker::new().is_some(),
            "tray function called off the main thread"
        );
    }

    /// Build and show the status item + its menu. A no-op if already installed
    /// or if called off the main thread (it never is — GPUI drives the tray).
    pub fn install(tx: mpsc::UnboundedSender<TrayEvent>) {
        let Some(mtm) = MainThreadMarker::new() else {
            warn!("tray install requested off the main thread — skipped");
            return;
        };
        TRAY.with_borrow_mut(|slot| {
            if slot.is_some() {
                return;
            }

            let status_item = status_item::create_status_item();
            status_item::set_symbol_icon(
                &status_item,
                mtm,
                "computermouse.fill",
                "OpenLogi",
                "OpenLogi",
            );

            let target = MenuTarget::new(mtm, tx.clone());
            let menu = status_item::new_menu(mtm);

            let idle = rust_i18n::t!("No devices connected");
            let mut device_items = Vec::with_capacity(MAX_DEVICE_ROWS);
            for i in 0..MAX_DEVICE_ROWS {
                let title = if i == 0 { idle.as_ref() } else { "" };
                let item = status_item::new_disabled_item(mtm, title);
                item.setHidden(i != 0);
                menu.addItem(&item);
                device_items.push(item);
            }

            status_item::add_separator(&menu, mtm);

            let open_title = rust_i18n::t!("Open OpenLogi");
            let open_item =
                status_item::new_action_item(mtm, &open_title, sel!(openOpenLogi:), &target);
            menu.addItem(&open_item);
            let quit_title = rust_i18n::t!("Quit OpenLogi");
            let quit_item =
                status_item::new_action_item(mtm, &quit_title, sel!(quitOpenLogi:), &target);
            menu.addItem(&quit_item);

            // The status item retains the menu, so `menu` may drop after this.
            status_item.setMenu(Some(&menu));

            *slot = Some(TrayState {
                status_item,
                open_item,
                quit_item,
                device_items,
                sender: tx,
                target,
            });
        });
    }

    /// Remove the status item from the system status bar during teardown.
    /// Dropping [`TrayState`] releases every retained object.
    pub fn uninstall() {
        debug_assert_main_thread();
        TRAY.with_borrow_mut(|slot| {
            if let Some(state) = slot.take() {
                status_item::remove_status_item(&state.status_item);
            }
        });
    }

    /// Show or hide the status-item icon without tearing it down — backs the
    /// "Show in menu bar" setting. A no-op until [`install`] has run.
    pub fn set_visible(visible: bool) {
        debug_assert_main_thread();
        TRAY.with_borrow(|slot| {
            if let Some(state) = slot.as_ref() {
                state.status_item.setVisible(visible);
            }
        });
    }

    /// Update the device rows — one per connected device (e.g.
    /// `"MX Master 3S · 80%"`). Spare rows are hidden; an empty list shows the
    /// "No devices connected" placeholder. A no-op until [`install`] has run.
    ///
    /// Each title is a fresh `Retained<NSString>` that releases when the
    /// statement ends — no leak, no autorelease pool (the issue-#99 fix).
    pub fn set_device_lines(lines: &[String]) {
        debug_assert_main_thread();
        TRAY.with_borrow(|slot| {
            let Some(state) = slot.as_ref() else {
                return;
            };
            if lines.is_empty() {
                let idle = rust_i18n::t!("No devices connected");
                if let Some(first) = state.device_items.first() {
                    first.setTitle(&NSString::from_str(&idle));
                    first.setHidden(false);
                }
                for item in state.device_items.iter().skip(1) {
                    item.setHidden(true);
                }
                return;
            }
            for (i, item) in state.device_items.iter().enumerate() {
                if let Some(line) = lines.get(i) {
                    item.setTitle(&NSString::from_str(line));
                    item.setHidden(false);
                } else {
                    item.setHidden(true);
                }
            }
        });
    }

    /// Re-title the Open/Quit items for the current locale. The device rows are
    /// refreshed separately via [`set_device_lines`].
    pub fn refresh_labels() {
        debug_assert_main_thread();
        TRAY.with_borrow(|slot| {
            if let Some(state) = slot.as_ref() {
                state
                    .open_item
                    .setTitle(&NSString::from_str(&rust_i18n::t!("Open OpenLogi")));
                state
                    .quit_item
                    .setTitle(&NSString::from_str(&rust_i18n::t!("Quit OpenLogi")));
            }
        });
    }

    /// Ask the drain task to re-localize the whole menu after a live language
    /// switch. Posts through the same channel as menu clicks so the device line
    /// (recomputed from the live `AppState`, which only the task can read) is
    /// rewritten on the main thread alongside the static labels.
    pub fn request_refresh() {
        debug_assert_main_thread();
        TRAY.with_borrow(|slot| {
            if let Some(state) = slot.as_ref()
                && state.sender.send(TrayEvent::Refresh).is_err()
            {
                warn!("tray refresh dropped — GPUI loop gone");
            }
        });
    }

    /// Show the app in the Dock + menu bar — called when a window opens, so the
    /// app menu (⌘Q, Settings, …) is available while the window is up.
    pub fn show_in_dock() {
        if let Some(mtm) = MainThreadMarker::new() {
            status_item::set_activation_policy(mtm, ActivationPolicy::Regular);
        }
    }

    /// Drop the app out of the Dock + menu bar, leaving only the status item —
    /// called when the last window closes (and on a `--minimized` launch).
    pub fn hide_from_dock() {
        if let Some(mtm) = MainThreadMarker::new() {
            status_item::set_activation_policy(mtm, ActivationPolicy::Accessory);
        }
    }
}
