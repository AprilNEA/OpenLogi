//! The agent's macOS AppKit loop, menu-bar item, and resume notifications.
//!
//! The always-on agent hosts the menu bar (the GUI is on-demand). The item
//! carries GUI-directed actions ("Show Main Window", Settings, About, Check for
//! Updates) and "Quit OpenLogi"; the GitHub/help links live in the GUI's own
//! menu bar, not here. Clicks fire on the main thread's AppKit run loop.
//!
//! GUI-directed actions open [`DeeplinkCommand`] `openlogi://` URLs which macOS
//! delivers to the GUI via Apple Events — works for both cold start (app
//! launched then URL delivered) and warm reactivation (URL delivered to the
//! running app).
//!
//! macOS-only. AppKit objects are `Retained<T>` (no #99-style leaks); the run
//! loop owns the main thread for the agent's lifetime.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init, action targets, and selector-based workspace notifications"
)]

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::NSStatusItem;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSRunningApplication, NSWorkspace,
    NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidWakeNotification,
    NSWorkspaceSessionDidBecomeActiveNotification,
};
use objc2_foundation::{NSNotification, NSNotificationName, NSString};
use openlogi_core::brand::{self, DeeplinkCommand};
use openlogi_core::config::AppIcon;
use tracing::{info, warn};

use crate::status_item;

thread_local! {
    /// The installed status item, kept where a later config reload can find it.
    /// A `thread_local` rather than a global: everything that touches it runs
    /// on the main thread, which is the same thread that installed it, so the
    /// affinity AppKit demands is the affinity the storage already has.
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
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
        STATUS_ITEM.with_borrow(|item| {
            if let Some(item) = item.as_ref() {
                status_item::set_png_icon(item, mtm, glyph(icon), "OpenLogi");
            }
        });
    });
}

struct ResumeTargetIvars {
    pending: Arc<AtomicBool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `ResumeTarget`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = ResumeTargetIvars]
    #[name = "OpenLogiAgentWorkspaceResumeTarget"]
    struct ResumeTarget;

    impl ResumeTarget {
        #[unsafe(method(workspaceDidResume:))]
        fn workspace_did_resume(&self, _notification: &NSNotification) {
            self.ivars().pending.store(true, Ordering::Relaxed);
        }
    }
);

impl ResumeTarget {
    fn new(pending: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ResumeTargetIvars { pending });
        // SAFETY: `init` initializes our freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }
}

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
            open_command(DeeplinkCommand::Show);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::OpenSettings);
        }

        #[unsafe(method(openAbout:))]
        fn open_about(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::OpenAbout);
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            open_command(DeeplinkCommand::CheckForUpdates);
        }

        #[unsafe(method(quitOpenLogi:))]
        fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
            quit_agent();
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
    match opener::open(url) {
        Ok(()) => info!(url, "menu-bar — opening URL"),
        Err(e) => warn!(error = %e, url, "could not open URL from menu bar"),
    }
}

/// Route a GUI-directed [`DeeplinkCommand`] through the `openlogi://` scheme.
/// macOS launches the GUI (cold start) or hands the URL to the running app.
fn open_command(command: DeeplinkCommand) {
    open_url(&command.to_url());
}

/// Menu-bar Quit: take a running GUI with us, then end the process.
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
    info!("menu-bar Quit — exiting agent");
    #[expect(
        clippy::exit,
        reason = "reached from an AppKit menu action on the main thread: the run loop owns `main`'s stack frame, so no status can travel back to it"
    )]
    std::process::exit(0)
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
/// tokio core still does all the work). The toggle takes effect on the agent's
/// next launch — a no-restart live toggle would need a main-thread hop from the
/// IPC reload path (deferred; it can't be verified headlessly).
/// `resume_pending` forwards coalesced workspace resume notifications to that core.
pub fn run_app_loop(
    show_in_menu_bar: bool,
    app_icon: AppIcon,
    resume_pending: Arc<AtomicBool>,
) -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("agent AppKit loop not started off the main thread — exiting");
        #[expect(
            clippy::exit,
            reason = "this branch means `run_app_loop` was called off the process main thread, where AppKit cannot run at all; the function is `-> !` and `main` returns `()`, so a failure status has nowhere to propagate"
        )]
        std::process::exit(1);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Bind the status item (+ its target/menu) so they outlive `run()` — the
    // menu items only weakly reference the target. `None` when hidden.
    let _tray = show_in_menu_bar.then(|| install_status_item(mtm, app_icon));
    let _resume_target = install_resume_observer(resume_pending);
    info!(show_in_menu_bar, "agent AppKit loop started");

    app.run();
    #[expect(
        clippy::exit,
        reason = "AppKit only returns from `run()` once the loop is torn down, and the agent core is still running on another thread; this function is `-> !` with no return path, so the process ends here"
    )]
    std::process::exit(0);
}

/// Observe native resume transitions that the inventory polling-gap heuristic
/// cannot see. The returned target must live for the AppKit loop's lifetime.
fn install_resume_observer(pending: Arc<AtomicBool>) -> Retained<ResumeTarget> {
    let target = ResumeTarget::new(pending);
    let workspace = NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    for name in resume_notification_names() {
        // SAFETY: `ResumeTarget` implements `workspaceDidResume:` with the
        // exact one-NSNotification argument signature, and the caller retains
        // the target for the AppKit loop's lifetime.
        unsafe {
            center.addObserver_selector_name_object(
                &target,
                sel!(workspaceDidResume:),
                Some(name),
                Some(&workspace),
            );
        }
    }
    target
}

fn resume_notification_names() -> [&'static NSNotificationName; 3] {
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let system_wake = unsafe { NSWorkspaceDidWakeNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let screen_wake = unsafe { NSWorkspaceScreensDidWakeNotification };
    // SAFETY: AppKit exports each name as an immutable process-lifetime constant.
    let session_active = unsafe { NSWorkspaceSessionDidBecomeActiveNotification };
    [system_wake, screen_wake, session_active]
}

/// Build and install the menu-bar status item, returning the objects that must
/// stay alive for the app's lifetime (the status item, the action target the
/// menu items weakly reference, and the menu itself).
fn install_status_item(
    mtm: MainThreadMarker,
    app_icon: AppIcon,
) -> (
    Retained<objc2_app_kit::NSStatusItem>,
    Retained<MenuTarget>,
    Retained<objc2_app_kit::NSMenu>,
) {
    let target = MenuTarget::new(mtm);
    let status_item = status_item::create_status_item();
    status_item::set_png_icon(&status_item, mtm, glyph(app_icon), "OpenLogi");
    STATUS_ITEM.with_borrow_mut(|slot| *slot = Some(status_item.clone()));
    let menu = status_item::new_menu(mtm);

    let show = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("Show Main Window"),
        sel!(openOpenLogi:),
        &target,
        "m",
    );
    menu.addItem(&show);
    status_item::add_separator(&menu, mtm);

    let settings = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("Settings…"),
        sel!(openSettings:),
        &target,
        ",",
    );
    menu.addItem(&settings);
    let about = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("About OpenLogi"),
        sel!(openAbout:),
        &target,
        "",
    );
    menu.addItem(&about);
    let updates = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("Check for Updates…"),
        sel!(checkForUpdates:),
        &target,
        "u",
    );
    menu.addItem(&updates);
    status_item::add_separator(&menu, mtm);

    let quit = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("Quit OpenLogi"),
        sel!(quitOpenLogi:),
        &target,
        "q",
    );
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("xmark.square"),
        Some(&NSString::from_str(&rust_i18n::t!("Quit OpenLogi"))),
    ) {
        image.setTemplate(true);
        quit.setImage(Some(&image));
    }
    menu.addItem(&quit);
    status_item.setMenu(Some(&menu));

    info!("menu-bar item installed");
    (status_item, target, menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_notifications_are_forwarded_and_coalesced() {
        let pending = Arc::new(AtomicBool::new(false));
        let target = install_resume_observer(Arc::clone(&pending));
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();

        for name in resume_notification_names() {
            // SAFETY: `workspace` is live, matches the registration filter,
            // and notification delivery completes synchronously.
            unsafe { center.postNotificationName_object(name, Some(&workspace)) };
            assert!(pending.swap(false, Ordering::Relaxed));
        }
        for name in resume_notification_names() {
            // SAFETY: Same live object and synchronous delivery as above.
            unsafe { center.postNotificationName_object(name, Some(&workspace)) };
        }
        assert!(pending.swap(false, Ordering::Relaxed));
        assert!(!pending.swap(false, Ordering::Relaxed));

        // SAFETY: This is the same live target registered with `center` above.
        unsafe { center.removeObserver(&target) };
    }
}
