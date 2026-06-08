//! The agent's menu-bar status item.
//!
//! The always-on agent hosts the menu bar (the GUI is on-demand). The item
//! carries "Open OpenLogi", GUI-directed actions, help links, and "Quit
//! OpenLogi". Clicks fire on the main thread's AppKit run loop.
//!
//! GUI-directed actions open `openlogi://` URLs which macOS delivers to the
//! GUI via Apple Events — works for both cold start (app launched then URL
//! delivered) and warm reactivation (URL delivered to running app).
//!
//! macOS-only. AppKit objects are `Retained<T>` (no #99-style leaks); the run
//! loop owns the main thread for the agent's lifetime.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init, init-with-action/set-target — localized here and in status_item"
)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage};
use objc2_foundation::NSString;
use tracing::{info, warn};

use crate::status_item;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLogiAgentMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openOpenLogi:))]
        fn open_openlogi(&self, _sender: Option<&AnyObject>) {
            open_url("openlogi://show");
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            open_url("openlogi://open-settings");
        }

        #[unsafe(method(openAbout:))]
        fn open_about(&self, _sender: Option<&AnyObject>) {
            open_url("openlogi://open-about");
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            open_url("openlogi://check-for-updates");
        }

        #[unsafe(method(openHelp:))]
        fn open_help(&self, _sender: Option<&AnyObject>) {
            open_url("https://github.com/AprilNEA/OpenLogi#readme");
        }

        #[unsafe(method(openRepository:))]
        fn open_repository(&self, _sender: Option<&AnyObject>) {
            open_url("https://github.com/AprilNEA/OpenLogi");
        }

        #[unsafe(method(openLatestRelease:))]
        fn open_latest_release(&self, _sender: Option<&AnyObject>) {
            open_url("https://github.com/AprilNEA/OpenLogi/releases/latest");
        }

        #[unsafe(method(quitOpenLogi:))]
        fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
            // Blocking wait is fine here — the agent is about to exit anyway.
            let _ = std::process::Command::new("open")
                .arg("openlogi://quit")
                .output();
            info!("menu-bar Quit — exiting agent");
            std::process::exit(0);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` initializes our freshly-allocated NSObject subclass and
        // returns it (the two-phase construction objc2's `define_class!` uses).
        unsafe { msg_send![super(this), init] }
    }
}

fn open_url(url: &str) {
    match std::process::Command::new("open").arg(url).spawn() {
        Ok(_) => info!(url, "menu-bar — opening URL"),
        Err(e) => warn!(error = %e, url, "could not open URL from menu bar"),
    }
}

/// Run the agent's AppKit main loop: an `Accessory` `NSApplication` (no Dock
/// icon) optionally hosting the menu-bar status item. Must be called on the
/// process's main thread; blocks for the agent's lifetime (the agent exits via
/// Quit).
///
/// `show_in_menu_bar` honors the user's preference: when `false`, the same
/// Accessory loop runs with no status item (the agent stays fully headless; the
/// tokio core still does all the work). The toggle takes effect on the agent's
/// next launch — a no-restart live toggle would need a main-thread hop from the
/// IPC reload path (deferred; it can't be verified headlessly).
pub fn run_app_loop(show_in_menu_bar: bool) -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("agent AppKit loop not started off the main thread — exiting");
        std::process::exit(1);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Bind the status item (+ its target/menu) so they outlive `run()` — the
    // menu items only weakly reference the target. `None` when hidden.
    let _tray = show_in_menu_bar.then(|| install_status_item(mtm));
    info!(show_in_menu_bar, "agent AppKit loop started");

    app.run();
    std::process::exit(0);
}

/// Build and install the menu-bar status item, returning the objects that must
/// stay alive for the app's lifetime (the status item, the action target the
/// menu items weakly reference, and the menu itself).
fn install_status_item(
    mtm: MainThreadMarker,
) -> (
    Retained<objc2_app_kit::NSStatusItem>,
    Retained<MenuTarget>,
    Retained<objc2_app_kit::NSMenu>,
) {
    let target = MenuTarget::new(mtm);
    let status_item = status_item::create_status_item();
    status_item::set_symbol_icon(
        &status_item,
        mtm,
        "computermouse.fill",
        "OpenLogi",
        "OpenLogi",
    );
    let menu = status_item::new_menu(mtm);

    let show =
        status_item::new_action_item(mtm, "Show Main Window", sel!(openOpenLogi:), &target, "m");
    menu.addItem(&show);
    status_item::add_separator(&menu, mtm);

    let settings =
        status_item::new_action_item(mtm, "Settings…", sel!(openSettings:), &target, ",");
    menu.addItem(&settings);
    let about = status_item::new_action_item(mtm, "About OpenLogi", sel!(openAbout:), &target, "");
    menu.addItem(&about);
    let updates = status_item::new_action_item(
        mtm,
        "Check for Updates…",
        sel!(checkForUpdates:),
        &target,
        "u",
    );
    menu.addItem(&updates);
    status_item::add_separator(&menu, mtm);

    let help = status_item::new_action_item(mtm, "OpenLogi Help", sel!(openHelp:), &target, "");
    menu.addItem(&help);
    let repository = status_item::new_action_item(
        mtm,
        "Open GitHub Repository",
        sel!(openRepository:),
        &target,
        "",
    );
    menu.addItem(&repository);
    let release =
        status_item::new_action_item(mtm, "Latest Release", sel!(openLatestRelease:), &target, "");
    menu.addItem(&release);
    status_item::add_separator(&menu, mtm);

    let quit =
        status_item::new_action_item(mtm, "Quit OpenLogi", sel!(quitOpenLogi:), &target, "q");
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("xmark.square"),
        Some(&NSString::from_str("Quit")),
    ) {
        image.setTemplate(true);
        quit.setImage(Some(&image));
    }
    menu.addItem(&quit);
    status_item.setMenu(Some(&menu));

    info!("menu-bar item installed");
    (status_item, target, menu)
}
