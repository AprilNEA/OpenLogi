//! The agent's macOS AppKit loop, menu-bar item, and login-session observer.
//!
//! The always-on agent hosts the menu bar (the GUI is on-demand). The item
//! carries GUI-directed actions ("Show Main Window", Settings, About, Check for
//! Updates) and "Quit OpenLogi"; the GitHub/help links live in the GUI's own
//! menu bar, not here. Clicks fire on the main thread's AppKit run loop.
//!
//! The menu is rebuilt every time it is about to open, so it always shows the
//! current locale's titles; nothing has to push changes into it.
//!
//! GUI-directed actions open [`DeeplinkCommand`] `openlogi://` URLs which macOS
//! delivers to the GUI via Apple Events — works for both cold start (app
//! launched then URL delivered) and warm reactivation (URL delivered to the
//! running app).
//!
//! The device-I/O gate itself lives in `activity_macos`; this file only
//! forwards the `NSWorkspace` session edges (fast user switching) to it and
//! sequences the launch hold around `finishLaunching`.
//!
//! macOS-only. AppKit objects are `Retained<T>` (no #99-style leaks); the run
//! loop owns the main thread for the agent's lifetime.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init, action targets, and selector-based workspace notifications"
)]

use std::cell::RefCell;
use std::sync::Arc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuDelegate,
    NSRunningApplication, NSStatusItem, NSWorkspace, NSWorkspaceSessionDidBecomeActiveNotification,
    NSWorkspaceSessionDidResignActiveNotification,
};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSString};
use openlogi_agent_core::observable::ObservableState;
use openlogi_core::brand::{self, DeeplinkCommand};
use openlogi_core::config::AppIcon;
use openlogi_hid::DeviceIoSignal;
use tracing::{info, warn};

use crate::activity_macos::{self, ActivityGate};
use crate::shutdown::{self, ShutdownRequestSender};
use crate::status_item;

/// The installed menu-bar item, which a later config reload restyles or
/// removes, plus the target its menu and menu items only weakly reference.
struct TrayState {
    item: Retained<NSStatusItem>,
    /// Never read: held so the menu's delegate and the items' action target
    /// live exactly as long as the item.
    _target: Retained<MenuTarget>,
}

thread_local! {
    /// The installed tray state, kept where a later config reload can find it.
    /// A `thread_local` rather than a global: everything that touches it runs
    /// on the main thread, which is the same thread that installed it, so the
    /// affinity AppKit demands is the affinity the storage already has.
    static TRAY: RefCell<Option<TrayState>> = const { RefCell::new(None) };
    /// Where menu actions hand process termination to the async lifecycle.
    /// Kept separately because the AppKit loop still exists when the status
    /// item is hidden by preference.
    static SHUTDOWN_TX: RefCell<Option<ShutdownRequestSender>> = const { RefCell::new(None) };
}

/// The menu-bar glyph for `icon`: a monochrome template the system tints for
/// the current menu bar, not the app icon itself — which is why these are
/// hand-drawn silhouettes rather than renders of the Icon Composer documents.
const fn glyph(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::Openlogi => include_bytes!("../assets/tray-icon@2x.png"),
        AppIcon::Prism => include_bytes!("../assets/tray-icon-prism@2x.png"),
    }
}

/// Point the menu-bar item at `icon`'s glyph, so picking an app icon changes
/// every surface that shows one rather than all but this.
///
/// Callable from anywhere: the work hops to the main queue, where AppKit lives
/// and where the status item was installed. A no-op when the item is hidden or
/// the loop never started.
pub fn set_icon(icon: AppIcon) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        TRAY.with_borrow(|state| {
            if let Some(state) = state.as_ref() {
                status_item::set_png_icon(&state.item, mtm, glyph(icon), "OpenLogi");
            }
        });
    });
}

/// Show or hide the menu-bar item to match the "Show in menu bar" setting,
/// after a config reload — so the toggle applies without an agent restart.
/// Same shape as [`set_icon`]: the work hops to the main queue. Installing
/// an already-shown item or removing a hidden one is a no-op. `observable` is
/// the state an installed item's menu is built from.
pub fn set_visible(show: bool, app_icon: AppIcon, observable: Arc<ObservableState>) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let installed = TRAY.with_borrow(Option::is_some);
        if show && !installed {
            install_status_item(mtm, app_icon, observable);
        } else if !show && let Some(state) = TRAY.take() {
            status_item::remove_status_item(&state.item);
            info!("menu-bar item removed");
        }
    });
}

struct SessionTargetIvars {
    gate: Arc<ActivityGate>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `SessionTarget`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = SessionTargetIvars]
    #[name = "OpenLogiAgentWorkspaceSessionTarget"]
    struct SessionTarget;

    impl SessionTarget {
        #[unsafe(method(workspaceSessionDidResignActive:))]
        fn workspace_session_did_resign_active(&self, _notification: &NSNotification) {
            self.ivars().gate.set_on_console(false);
        }

        #[unsafe(method(workspaceSessionDidBecomeActive:))]
        fn workspace_session_did_become_active(&self, _notification: &NSNotification) {
            self.ivars().gate.set_on_console(true);
        }
    }
);

impl SessionTarget {
    fn new(gate: Arc<ActivityGate>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(SessionTargetIvars { gate });
        // SAFETY: `init` initializes our freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }
}

struct MenuTargetIvars {
    /// The agent state the menu is built from.
    #[expect(
        dead_code,
        reason = "held for the menu's rebuild; no row reads agent state yet"
    )]
    observable: Arc<ObservableState>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    #[name = "OpenLogiAgentMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openOpenLogi:))]
        fn open_openlogi(&self, _sender: Option<&AnyObject>) {
            open_command(&DeeplinkCommand::Show);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            open_command(&DeeplinkCommand::OpenSettings);
        }

        #[unsafe(method(openAbout:))]
        fn open_about(&self, _sender: Option<&AnyObject>) {
            open_command(&DeeplinkCommand::OpenAbout);
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            open_command(&DeeplinkCommand::CheckForUpdates);
        }

        #[unsafe(method(quitOpenLogi:))]
        fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
            quit_agent();
        }
    }

    // SAFETY: `NSObjectProtocol` asks nothing beyond being an `NSObject`,
    // which `MenuTarget` subclasses.
    unsafe impl NSObjectProtocol for MenuTarget {}

    // SAFETY: the one method implemented matches the protocol's
    // `menuNeedsUpdate:` signature, and `MenuTarget` is main-thread-only, as
    // the protocol requires.
    unsafe impl NSMenuDelegate for MenuTarget {
        // AppKit calls this just before the menu opens, so the rebuild picks
        // up the locale of that moment.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            menu.removeAllItems();
            populate_menu(menu, self);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, observable: Arc<ObservableState>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { observable });
        // SAFETY: `init` initializes our freshly-allocated NSObject subclass and
        // returns it (the two-phase construction objc2's `define_class!` uses).
        unsafe { msg_send![super(this), init] }
    }
}

fn open_url(url: &str) {
    match opener::open(url) {
        Ok(()) => info!(url, "menu-bar — opening URL"),
        Err(e) => warn!(error = %e, url, "could not open URL from menu bar"),
    }
}

/// Route a GUI-directed [`DeeplinkCommand`] through the `openlogi://` scheme.
/// macOS launches the GUI (cold start) or hands the URL to the running app.
fn open_command(command: &DeeplinkCommand) {
    open_url(&command.to_url());
}

/// Menu-bar Quit: take a running GUI with us, then hand process termination to
/// the lifecycle that owns firmware capture and the input hook.
///
/// Kept out of `define_class!` so the lint set actually sees the exit — clippy
/// does not look inside macro expansions.
fn quit_agent() -> ! {
    // Tell a *running* GUI to quit too, but don't let `open` cold-launch one
    // just to immediately quit it (it would flash a window — and on first run
    // the update-consent prompt — before exiting). The gate keeps the target
    // warm in the common case, so the blocking `.output()` (which guarantees
    // Apple-Event delivery) returns at once; a GUI that races to exit after the
    // check was quitting anyway.
    if gui_is_running() {
        let _ = std::process::Command::new("open")
            .arg(DeeplinkCommand::Quit.to_url())
            .output();
    }
    crate::overlay::evict_on_quit();
    info!("menu-bar Quit — requesting graceful agent shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0)
}

/// Whether an OpenLogi GUI process is currently running (prod or dev bundle).
/// Used to avoid cold-launching the GUI from the Quit handler just to quit it.
fn gui_is_running() -> bool {
    // Release and dev; the agent's own id is `brand::AGENT_ID`, so neither
    // matches the agent itself.
    let dev = brand::dev_id(brand::APP_ID);
    [brand::APP_ID, dev.as_str()].iter().any(|id| {
        let running =
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(id));
        !running.is_empty()
    })
}

/// Run the agent's AppKit main loop: an `Accessory` `NSApplication` (no Dock
/// icon) optionally hosting the menu-bar status item. Must be called on the
/// process's main thread; blocks for the agent's lifetime (the agent exits via
/// Quit).
///
/// `show_in_menu_bar` honors the user's preference: when `false`, the same
/// Accessory loop runs with no status item (the agent stays fully headless; the
/// tokio core still does all the work). A later config reload flips it live
/// through [`set_visible`].
/// `observable` is the agent state the menu is built from.
/// `device_io_signal` is the hardware gate; it opens only while the Mac is in
/// a full wake and this login session owns the console (`activity_macos`).
pub fn run_app_loop(
    show_in_menu_bar: bool,
    app_icon: AppIcon,
    observable: Arc<ObservableState>,
    device_io_signal: DeviceIoSignal,
    shutdown_tx: ShutdownRequestSender,
) -> ! {
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("agent AppKit loop not started off the main thread — exiting");
        let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
        shutdown::request_tray_quit(requests.as_ref(), 1);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let gate = ActivityGate::new(device_io_signal);
    let _session = install_session_observer(Arc::clone(&gate));
    // Before `run()`: AppKit remaps the probes for the keyboard layout only
    // once the loop has turned with them in the main menu.
    openlogi_inject::prepare_menu_shortcuts(mtm);
    // `TRAY` keeps the status item (which retains its menu) and the target
    // the menu and its items only weakly reference alive past `run()`.
    if show_in_menu_bar {
        install_status_item(mtm, app_icon, observable);
    }

    // AppKit documents that an app launched into an inactive session receives
    // `NSWorkspaceSessionDidResignActiveNotification` between its will- and
    // did-finish-launching notifications. Finish that lifecycle while the
    // launch hold still keeps the hardware gate closed, then read both levels
    // — the console from CoreGraphics, the power state from powerd, which
    // also starts the transition stream — before releasing the hold. Every
    // one of those is a level, so the order they land in cannot matter.
    app.finishLaunching();
    let _power = activity_macos::connect(&gate);
    gate.set_on_console(activity_macos::session_is_on_console());
    gate.finish_startup();
    info!(show_in_menu_bar, "agent AppKit loop started");

    app.run();
    info!("agent AppKit loop ended — requesting graceful core shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0);
}

/// Forward the login session's console edges to the gate. These are the fast
/// user switching notifications; sleep and wake are deliberately not observed
/// here — powerd reports them as levels (`activity_macos`).
fn install_session_observer(gate: Arc<ActivityGate>) -> Retained<SessionTarget> {
    let target = SessionTarget::new(gate);
    let workspace = NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let session_inactive = unsafe { NSWorkspaceSessionDidResignActiveNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let session_active = unsafe { NSWorkspaceSessionDidBecomeActiveNotification };
    // SAFETY: Both selectors take exactly one `NSNotification` argument, and
    // the caller retains the target for the AppKit loop's lifetime.
    unsafe {
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceSessionDidResignActive:),
            Some(session_inactive),
            Some(&workspace),
        );
        center.addObserver_selector_name_object(
            &target,
            sel!(workspaceSessionDidBecomeActive:),
            Some(session_active),
            Some(&workspace),
        );
    }
    target
}

/// Build and install the menu-bar status item into [`TRAY`], which keeps it
/// (and, through it, its menu) and the target the menu and its items weakly
/// reference alive until [`set_visible`] removes it.
///
/// The menu starts empty: its delegate fills it each time it is about to open.
fn install_status_item(mtm: MainThreadMarker, app_icon: AppIcon, observable: Arc<ObservableState>) {
    let target = MenuTarget::new(mtm, observable);
    let status_item = status_item::create_status_item();
    status_item::set_png_icon(&status_item, mtm, glyph(app_icon), "OpenLogi");
    let menu = status_item::new_menu(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    status_item.setMenu(Some(&menu));
    TRAY.set(Some(TrayState {
        item: status_item,
        _target: target,
    }));

    info!("menu-bar item installed");
}

/// Fill the empty tray `menu` with the current locale's titles, each item
/// sending its action to `target`.
fn populate_menu(menu: &NSMenu, target: &MenuTarget) {
    let mtm = target.mtm();

    let show = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.show_main_window"),
        sel!(openOpenLogi:),
        target,
        "m",
    );
    menu.addItem(&show);
    status_item::add_separator(menu, mtm);

    let settings = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.settings_dialog"),
        sel!(openSettings:),
        target,
        ",",
    );
    menu.addItem(&settings);
    let about = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("about.about_openlogi"),
        sel!(openAbout:),
        target,
        "",
    );
    menu.addItem(&about);
    let updates = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("updates.check_for_updates_dialog"),
        sel!(checkForUpdates:),
        target,
        "u",
    );
    menu.addItem(&updates);
    status_item::add_separator(menu, mtm);

    let quit = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.quit_openlogi"),
        sel!(quitOpenLogi:),
        target,
        "q",
    );
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("xmark.square"),
        Some(&NSString::from_str(&rust_i18n::t!("app.quit_openlogi"))),
    ) {
        image.setTemplate(true);
        quit.setImage(Some(&image));
    }
    menu.addItem(&quit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity_macos::PowerState;
    use openlogi_hid::device_io_channel;

    #[test]
    fn a_session_switch_closes_the_gate_and_the_switch_back_reopens_it() {
        let (signal, io) = device_io_channel();
        let gate = ActivityGate::new(signal);
        let target = install_session_observer(Arc::clone(&gate));
        gate.set_power(PowerState::FullWake);
        gate.set_on_console(true);
        gate.finish_startup();
        assert!(io.allows_io());

        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let session_inactive = unsafe { NSWorkspaceSessionDidResignActiveNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(session_inactive, Some(&workspace)) };
        assert!(!io.allows_io());

        // SAFETY: AppKit exports the name as an immutable process-lifetime constant.
        let session_active = unsafe { NSWorkspaceSessionDidBecomeActiveNotification };
        // SAFETY: `workspace` is live, matches the registration filter, and
        // notification delivery completes synchronously.
        unsafe { center.postNotificationName_object(session_active, Some(&workspace)) };
        assert!(io.allows_io());

        // SAFETY: This is the same live target registered with `center` above.
        unsafe { center.removeObserver(&target) };
    }
}
