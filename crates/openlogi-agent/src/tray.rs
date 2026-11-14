//! The agent's macOS AppKit loop, menu-bar item, and login-session observer.
//!
//! The always-on agent hosts the menu bar (the GUI is on-demand). The menu
//! shows what needs attention, every device, the current application and Flow
//! ([`TrayMenu`] decides which rows and what they say), then GUI-directed
//! actions ("Show Main Window", Settings, About, Check for Updates) and "Quit
//! OpenLogi"; the GitHub/help links live in the GUI's own menu bar, not here.
//! Clicks fire on the main thread's AppKit run loop.
//!
//! The menu is rebuilt every time it is about to open, from the agent's
//! in-memory snapshot and the config the core last handed over
//! ([`set_config`]), so opening it waits on no device and no lock. The icon is
//! the one thing pushed: the core marks it when something needs the user
//! ([`watch_attention`]).
//!
//! A device's submenu also changes its settings. Opening it shows what the
//! tray last read from the device, starts a fresh read on the core's runtime,
//! and fills in the answer when it lands, while the submenu is open. A click
//! runs its change on the core the same way — through the layer the IPC
//! handlers call, not the IPC — and the submenu shows a change that did not
//! take the next time it opens. Webcams are listed from a scan on a blocking
//! thread, refreshed each time the menu opens; the scan reads metadata only, so
//! the agent never asks for the Camera permission.
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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSEventModifierFlags, NSImage, NSMenu, NSMenuDelegate, NSMenuItem,
    NSRunningApplication, NSStatusItem, NSWorkspace, NSWorkspaceSessionDidBecomeActiveNotification,
    NSWorkspaceSessionDidResignActiveNotification,
};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSString};
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{SettingError, SharedHandles};
use openlogi_core::brand::{self, DeeplinkCommand};
use openlogi_core::config::{AppIcon, Config};
use openlogi_core::device::{BatteryAttention, BatteryInfo, DeviceKind};
use openlogi_hid::{DeviceIoSignal, DeviceRoute, WriteError};
use openlogi_ipc::Observation;
use rust_i18n::t;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::activity_macos::{self, ActivityGate};
use crate::server::{self, AgentServer};
use crate::shutdown::{self, ShutdownRequestSender};
use crate::status_item::{self, Trailing, TrailingTitles};
use crate::tray_model::{
    AppRow, BatteryFill, CameraRow, Change, Detail, DeviceMemory, DeviceRow, Entry, Failure,
    FlowRow, Issue, Permission, Read, Reading, Reads, RoutedDevice, SeenCamera, TrayMenu,
    battery_text, is_charging, needs_attention,
};

/// What the menu needs from the async core, handed over when the agent arms.
#[derive(Clone)]
pub(crate) struct Backend {
    /// The agent state the menu is built from.
    pub(crate) observable: Arc<ObservableState>,
    /// The core's runtime, where every device read and write runs.
    pub(crate) runtime: tokio::runtime::Handle,
    /// The IPC server, whose device settings and backlight writes the menu
    /// shares.
    pub(crate) server: AgentServer,
}

/// The installed menu-bar item, which a later config reload restyles or
/// removes, plus the target its menu and menu items only weakly reference.
struct TrayState {
    item: Retained<NSStatusItem>,
    /// The app icon whose glyph the item shows.
    icon: AppIcon,
    /// The item's menu, rebuilt when a camera scan changes the list.
    menu: Retained<NSMenu>,
    /// The menu's delegate and the items' action target, held so it lives
    /// exactly as long as the item.
    target: Retained<MenuTarget>,
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
    /// The config the menu reads, as the core last loaded it. A copy kept on
    /// the main thread, so opening the menu takes no lock; [`set_config`]
    /// replaces it.
    static CONFIG: RefCell<Arc<Config>> = RefCell::default();
    /// Whether the icon carries its attention mark. Kept apart from [`TRAY`]
    /// so an item installed later starts with the current mark.
    static ATTENTION: Cell<bool> = const { Cell::new(false) };
    /// What an item installed by a later reload is built from.
    static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
    /// What the menu remembers of each device, so a submenu opens on the
    /// last reading instead of waiting for the device.
    static MEMORY: RefCell<HashMap<RoutedDevice, DeviceMemory>> = RefCell::default();
    /// The webcams the last scan found, and whether a scan is under way.
    static CAMERAS: RefCell<(Vec<SeenCamera>, bool)> = const { RefCell::new((Vec::new(), false)) };
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

/// Draw `state`'s icon: its app icon's glyph, marked when something needs
/// the user.
fn draw_icon(state: &TrayState, mtm: MainThreadMarker) {
    status_item::set_png_icon(
        &state.item,
        mtm,
        glyph(state.icon),
        ATTENTION.get(),
        "OpenLogi",
    );
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
        TRAY.with_borrow_mut(|state| {
            if let Some(state) = state.as_mut() {
                state.icon = icon;
                draw_icon(state, mtm);
            }
        });
    });
}

/// Hand the menu the config the core just loaded. Callable from anywhere: the
/// copy hops to the main queue, where the menu reads it the next time it
/// opens.
pub fn set_config(config: Arc<Config>) {
    DispatchQueue::main().exec_async(move || CONFIG.set(config));
}

/// Keep the icon's attention mark in step with the agent state for as long as
/// the state lives: the mark shows while [`needs_attention`] says so, and the
/// main thread redraws the icon only when that verdict changes.
pub async fn watch_attention(mut changes: watch::Receiver<Observation>) {
    // What `ATTENTION` starts as.
    let mut marked = false;
    loop {
        let attention = needs_attention(&changes.borrow_and_update().snapshot);
        if attention != marked {
            marked = attention;
            DispatchQueue::main().exec_async(move || {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                ATTENTION.set(attention);
                TRAY.with_borrow(|state| {
                    if let Some(state) = state.as_ref() {
                        draw_icon(state, mtm);
                    }
                });
            });
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// Show or hide the menu-bar item to match the "Show in menu bar" setting,
/// after a config reload — so the toggle applies without an agent restart.
/// Same shape as [`set_icon`]: the work hops to the main queue. Installing
/// an already-shown item or removing a hidden one is a no-op.
pub fn set_visible(show: bool, app_icon: AppIcon) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let installed = TRAY.with_borrow(Option::is_some);
        if show && !installed {
            if let Some(backend) = BACKEND.with_borrow(Clone::clone) {
                install_status_item(mtm, app_icon, backend);
            }
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
    /// What the menu is built from, and where its reads and writes run.
    backend: Backend,
    /// What each row built from [`TrayMenu`] does when clicked, indexed by
    /// the row's tag. Refilled with the menu.
    actions: RefCell<Vec<Action>>,
    /// Each device submenu of the current menu, with the row it was built
    /// for. Refilled with the menu.
    submenus: RefCell<Vec<(Retained<NSMenu>, DeviceRow)>>,
}

/// What clicking a row built from [`TrayMenu`] does.
#[derive(Clone)]
enum Action {
    /// Open the settings app at this link.
    Open(DeeplinkCommand),
    /// Open this permission's System Settings pane.
    Pane(Permission),
    /// Show the log directory in Finder.
    ShowLogs,
    /// Make this change to this device.
    Change(RoutedDevice, Change),
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

        #[unsafe(method(performAction:))]
        fn perform_action(&self, sender: &NSMenuItem) {
            self.perform(sender.tag());
        }

        #[unsafe(method(quitOpenLogi:))]
        fn quit_openlogi(&self, _sender: Option<&AnyObject>) {
            quit_agent();
        }
    }

    // SAFETY: `NSObjectProtocol` asks nothing beyond being an `NSObject`,
    // which `MenuTarget` subclasses.
    unsafe impl NSObjectProtocol for MenuTarget {}

    // SAFETY: both methods match the protocol's `menuNeedsUpdate:` and
    // `menuDidClose:` signatures, and `MenuTarget` is main-thread-only, as
    // the protocol requires.
    unsafe impl NSMenuDelegate for MenuTarget {
        // AppKit calls this just before a menu opens, so the rebuild picks
        // up the state and the locale of that moment. A device's submenu
        // asks its device for a fresh reading at the same time.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            if let Some(row) = self.row_of(menu) {
                self.read(&row);
                fill_device_menu(menu, self, &row);
            } else {
                scan_cameras(&self.ivars().backend.runtime);
                menu.removeAllItems();
                populate_menu(menu, self);
            }
        }

        // A change that did not take is said once: the submenu that showed
        // it forgets it as it closes.
        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, menu: &NSMenu) {
            if let Some(routed) = self.row_of(menu).and_then(|row| row.routed()) {
                MEMORY.with_borrow_mut(|memory| {
                    if let Some(device) = memory.get_mut(&routed) {
                        device.failure = None;
                    }
                });
            }
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, backend: Backend) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars {
            backend,
            actions: RefCell::default(),
            submenus: RefCell::default(),
        });
        // SAFETY: `init` initializes our freshly-allocated NSObject subclass and
        // returns it (the two-phase construction objc2's `define_class!` uses).
        unsafe { msg_send![super(this), init] }
    }

    /// An item titled `title` that does `action` when clicked.
    fn action_item(&self, title: &str, action: Action) -> Retained<NSMenuItem> {
        self.keyed_action_item(title, action, "")
    }

    /// [`Self::action_item`] with the key equivalent `key` (⌘ plus `key`).
    fn keyed_action_item(&self, title: &str, action: Action, key: &str) -> Retained<NSMenuItem> {
        let item = status_item::new_action_item(self.mtm(), title, sel!(performAction:), self, key);
        let mut actions = self.ivars().actions.borrow_mut();
        // A menu holds a few dozen items, far inside `NSInteger`.
        item.setTag(isize::try_from(actions.len()).unwrap_or(isize::MAX));
        actions.push(action);
        item
    }

    /// Do what the item tagged `tag` was built to do.
    fn perform(&self, tag: isize) {
        let action = usize::try_from(tag)
            .ok()
            .and_then(|index| self.ivars().actions.borrow().get(index).cloned());
        match action {
            Some(Action::Open(command)) => open_command(&command),
            Some(Action::Pane(permission)) => openlogi_permissions::open_pane(match permission {
                Permission::Accessibility => openlogi_permissions::Permission::Accessibility,
                Permission::InputMonitoring => openlogi_permissions::Permission::InputMonitoring,
            }),
            Some(Action::ShowLogs) => show_logs(),
            Some(Action::Change(device, change)) => self.change(device, change),
            None => warn!(tag, "menu-bar item with no action"),
        }
    }

    /// The device row `menu` is the submenu of, or `None` for the menu
    /// itself.
    fn row_of(&self, menu: &NSMenu) -> Option<DeviceRow> {
        self.ivars()
            .submenus
            .borrow()
            .iter()
            .find(|(submenu, _)| std::ptr::eq(Retained::as_ptr(submenu), menu))
            .map(|(_, row)| row.clone())
    }

    /// Read `row`'s device on the core, unless a read is under way, and
    /// refill its submenus with the answer.
    fn read(&self, row: &DeviceRow) {
        let reads = row.reads();
        let Some(device) = row.routed().filter(|_| reads.any()) else {
            return;
        };
        let started = MEMORY
            .with_borrow_mut(|memory| memory.entry(device.clone()).or_default().start_reading());
        if !started {
            return;
        }
        let shared = self.ivars().backend.server.shared.clone();
        self.ivars().backend.runtime.spawn(async move {
            let fresh = Box::pin(read_device(&shared, &device.route, reads)).await;
            DispatchQueue::main().exec_async(move || {
                MEMORY.with_borrow_mut(|memory| {
                    memory
                        .entry(device.clone())
                        .or_default()
                        .finish_reading(reads, fresh);
                });
                refill(&device);
            });
        });
    }

    /// Make `change` to `device` on the core, then remember it — or why it
    /// did not take — for the next opening of its submenu.
    fn change(&self, device: RoutedDevice, change: Change) {
        let server = self.ivars().backend.server.clone();
        self.ivars().backend.runtime.spawn(async move {
            let outcome = apply(&server, &device, change).await;
            DispatchQueue::main().exec_async(move || {
                MEMORY.with_borrow_mut(|memory| {
                    memory.entry(device.clone()).or_default().changed(outcome);
                });
                refill(&device);
            });
        });
    }
}

/// Read what `reads` asks of the device at `route`, all at once.
async fn read_device(shared: &SharedHandles, route: &DeviceRoute, reads: Reads) -> Reading {
    async fn when<T>(wanted: bool, read: impl Future<Output = Result<T, WriteError>>) -> Read<T> {
        if wanted {
            Read::of(read.await)
        } else {
            Read::Unread
        }
    }
    let (dpi, wheel, fn_lock, backlight, disabled_keys, sidetone) = tokio::join!(
        when(reads.dpi, async {
            shared.read_dpi(route).await.map(|info| info.current)
        }),
        when(reads.wheel, shared.read_smartshift(route)),
        when(reads.fn_lock, async {
            shared.read_fn_lock(route).await.map(|state| state.fn_lock)
        }),
        when(reads.backlight, shared.read_backlight(route)),
        when(reads.disabled_keys, shared.read_disable_keys(route)),
        when(reads.sidetone, shared.read_sidetone(route)),
    );
    Reading {
        dpi,
        wheel,
        fn_lock,
        backlight,
        disabled_keys,
        sidetone,
    }
}

/// Make `change` to `device` as the settings app would: saved settings
/// through the agent's device settings, the backlight and sidetone, which
/// the device keeps itself, on the device alone. Answers the change the
/// device took, or `None` when a newer write superseded it. A change reaches
/// only `device`: another device the agent knows on its route, or that
/// answers there when the change is written, such as one paired into the
/// receiver slot since the menu listed `device`, takes none of it.
async fn apply(
    server: &AgentServer,
    device: &RoutedDevice,
    change: Change,
) -> Result<Option<Change>, Failure> {
    let route = &device.route;
    let settings = server.device_settings();
    let saved = match change {
        Change::Dpi(dpi) => settings.set_dpi(device, dpi).await,
        Change::Wheel(status) => settings.set_smartshift(device, status).await,
        Change::Managed(managed) => settings.set_enabled(device, managed).await,
        Change::FnLock(fn_lock) => settings.set_fn_lock(device, fn_lock).await,
        Change::DisabledKeys(disabled) => settings.set_disabled_keys(device, disabled).await,
        Change::Light(command) => settings.set_light(device, command).await,
        Change::Backlight(backlight) => {
            let orchestrator = Arc::clone(&server.orchestrator);
            return server::backlight::write(
                orchestrator,
                &server.shared.reaching(device.unit),
                route.clone(),
                backlight,
            )
            .await
            .map(|_| Some(change))
            .map_err(|error| {
                warn!(%route, %error, "menu-bar backlight change failed");
                Failure::Device
            });
        }
        Change::Sidetone(level) => {
            let written = server
                .shared
                .reaching(device.unit)
                .set_sidetone(route, level)
                .await;
            // The settings app re-reads it, as it does after a backlight
            // change the agent made.
            server
                .orchestrator
                .lock()
                .await
                .announce_settings_changed(route);
            // A superseded write answers the level the headset kept.
            return written
                .map(|level| Some(Change::Sidetone(level)))
                .map_err(|error| {
                    warn!(%route, %error, "menu-bar sidetone change failed");
                    Failure::Device
                });
        }
    };
    match saved {
        Ok(()) => Ok(Some(change)),
        Err(SettingError::Write(WriteError::WriteSuperseded { .. })) => {
            debug!(%route, ?change, "menu-bar setting change superseded by a newer write");
            Ok(None)
        }
        Err(error) => {
            warn!(%route, %error, ?change, "menu-bar setting change failed");
            Err(Failure::of(&error))
        }
    }
}

/// Refill every submenu of `device` from what the menu now remembers of it
/// — while it is open, too.
fn refill(device: &RoutedDevice) {
    let Some(target) = TRAY.with_borrow(|state| state.as_ref().map(|state| state.target.clone()))
    else {
        return;
    };
    let submenus: Vec<_> = target
        .ivars()
        .submenus
        .borrow()
        .iter()
        .filter(|(_, row)| row.routed().as_ref() == Some(device))
        .cloned()
        .collect();
    for (submenu, row) in submenus {
        fill_device_menu(&submenu, &target, &row);
    }
}

/// Scan for webcams on a blocking thread of the core, unless a scan is under
/// way, and rebuild the menu when the list changed. AVFoundation's device
/// list and its in-use flag are metadata: no capture, so no Camera prompt.
fn scan_cameras(runtime: &tokio::runtime::Handle) {
    let started = CAMERAS.with_borrow_mut(|(_, scanning)| !std::mem::replace(scanning, true));
    if !started {
        return;
    }
    runtime.spawn_blocking(|| {
        let cameras = openlogi_camera::enumerate_cameras();
        let in_use = openlogi_camera::cameras_in_use();
        let seen: Vec<SeenCamera> = cameras
            .into_iter()
            .map(|camera| SeenCamera {
                in_use: in_use.contains(&camera.unique_id),
                key: camera.config_key(),
                name: camera.name,
            })
            .collect();
        DispatchQueue::main().exec_async(move || {
            let changed = CAMERAS.with_borrow_mut(|(cameras, scanning)| {
                *scanning = false;
                let changed = *cameras != seen;
                *cameras = seen;
                changed
            });
            if changed {
                TRAY.with_borrow(|state| {
                    if let Some(state) = state.as_ref() {
                        state.menu.removeAllItems();
                        populate_menu(&state.menu, &state.target);
                    }
                });
            }
        });
    });
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

/// Show the directory the agent's logs are written to in Finder.
fn show_logs() {
    match openlogi_core::paths::state_dir() {
        Ok(dir) => match opener::open(&dir) {
            Ok(()) => info!(dir = %dir.display(), "menu-bar — showing logs"),
            Err(e) => warn!(error = %e, dir = %dir.display(), "could not show logs"),
        },
        Err(e) => warn!(error = %e, "no log directory to show"),
    }
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
/// `config` is the config the agent started with. Its "Show in menu bar"
/// preference decides whether the item shows: when `false`, the same
/// Accessory loop runs with no status item (the agent stays fully headless; the
/// tokio core still does all the work). A later config reload flips it live
/// through [`set_visible`], and hands the menu the new config through
/// [`set_config`].
/// `backend` is what the menu is built from and where its reads and writes
/// run. `device_io_signal` is the hardware gate; it opens only while the Mac
/// is in a full wake and this login session owns the console
/// (`activity_macos`).
pub fn run_app_loop(
    config: Arc<Config>,
    backend: Backend,
    device_io_signal: DeviceIoSignal,
    shutdown_tx: ShutdownRequestSender,
) -> ! {
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    BACKEND.set(Some(backend.clone()));
    let show_in_menu_bar = config.app_settings.show_in_menu_bar;
    let app_icon = config.app_settings.app_icon;
    CONFIG.set(config);
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
        install_status_item(mtm, app_icon, backend);
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
/// The first camera scan starts here, so the first opening lists them.
fn install_status_item(mtm: MainThreadMarker, app_icon: AppIcon, backend: Backend) {
    scan_cameras(&backend.runtime);
    let target = MenuTarget::new(mtm, backend);
    let status_item = status_item::create_status_item();
    let menu = status_item::new_menu(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    status_item.setMenu(Some(&menu));
    let state = TrayState {
        item: status_item,
        icon: app_icon,
        menu,
        target,
    };
    draw_icon(&state, mtm);
    TRAY.set(Some(state));

    info!("menu-bar item installed");
}

/// Fill the empty tray `menu` from the agent's current state, in the current
/// locale, each item sending its action to `target`.
fn populate_menu(menu: &NSMenu, target: &MenuTarget) {
    let mtm = target.mtm();
    let snapshot = target.ivars().backend.observable.snapshot();
    let (model, cameras) = CONFIG.with_borrow(|config| {
        let cameras = CAMERAS.with_borrow(|(cameras, _)| CameraRow::all(cameras, config));
        (TrayMenu::new(&snapshot, config), cameras)
    });
    target.ivars().actions.borrow_mut().clear();
    target.ivars().submenus.borrow_mut().clear();
    // Every trailing part of this menu ends on one right edge.
    let mut titles = TrailingTitles::default();

    for issue in &model.issues {
        menu.addItem(&issue_item(target, issue, &mut titles));
    }
    if !model.issues.is_empty() {
        status_item::add_separator(menu, mtm);
    }

    menu.addItem(&status_item::section_header(mtm, &t!("device.devices")));
    if model.devices.is_empty() && cameras.is_empty() {
        menu.addItem(&status_item::info_item(
            mtm,
            &t!("device.no_devices_connected"),
        ));
    }
    for row in &model.devices {
        let item = device_item(target, row);
        if let Some(detail) = row.detail() {
            titles.push(&item, row.name.clone(), trailing(detail));
        }
        menu.addItem(&item);
        // Holding ⌥ swaps in how the device is connected.
        let link = device_item(target, row);
        titles.push(&link, row.name.clone(), Trailing::text(row.link.text()));
        alternate(&link, NSEventModifierFlags::Option);
        menu.addItem(&link);
    }
    for camera in &cameras {
        let item = camera_item(target, camera);
        titles.push(
            &item,
            camera.name.clone(),
            Trailing::text(camera.state_text()),
        );
        menu.addItem(&item);
    }
    status_item::add_separator(menu, mtm);

    if let Some(app) = &model.app {
        let item = app_item(target, app);
        titles.push(
            &item,
            app.current.name.clone(),
            Trailing::text(app.current.buttons.text()),
        );
        menu.addItem(&item);
    }
    if let Some(flow) = &model.flow {
        let title = t!("flow.title");
        let item = flow_item(target, flow, &title);
        titles.push(&item, title.into_owned(), Trailing::text(flow.detail()));
        menu.addItem(&item);
    }
    menu.addItem(&target.action_item(
        &t!("pairing.add_device_dialog"),
        Action::Open(DeeplinkCommand::AddDevice),
    ));
    status_item::add_separator(menu, mtm);

    add_window_items(menu, target, &model);

    titles.apply();
}

/// Add the items every menu ends with: the GUI's windows, each with its ⌥
/// alternate, then Quit.
fn add_window_items(menu: &NSMenu, target: &MenuTarget, model: &TrayMenu) {
    let mtm = target.mtm();
    let show = status_item::new_action_item(
        mtm,
        &rust_i18n::t!("app.show_main_window"),
        sel!(openOpenLogi:),
        target,
        "m",
    );
    menu.addItem(&show);
    let logs = target.keyed_action_item(&t!("tray.show_logs"), Action::ShowLogs, "m");
    alternate(
        &logs,
        NSEventModifierFlags::Command | NSEventModifierFlags::Option,
    );
    menu.addItem(&logs);
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
    let version = status_item::info_item(
        mtm,
        &t!("tray.version", version => &model.version, protocol => model.protocol),
    );
    alternate(&version, NSEventModifierFlags::Option);
    menu.addItem(&version);
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

/// Make `item` the alternate of the item before it, shown in its place while
/// `modifiers` are held. AppKit pairs the two by their key equivalent.
fn alternate(item: &NSMenuItem, modifiers: NSEventModifierFlags) {
    item.setKeyEquivalentModifierMask(modifiers);
    item.setAlternate(true);
}

/// The row for `issue`, which opens where it is fixed or, when nothing fixes
/// it, only explains.
fn issue_item(
    target: &MenuTarget,
    issue: &Issue,
    titles: &mut TrailingTitles,
) -> Retained<NSMenuItem> {
    let title = issue.title();
    let action = match issue {
        Issue::MissingPermission(permission) => Some(Action::Pane(*permission)),
        // The settings app explains both causes: another app holding the
        // devices, or a grant that needs a fresh login.
        Issue::DevicesUnavailable => Some(Action::Open(DeeplinkCommand::OpenSettings)),
        Issue::SecureInput(_) => None,
    };
    let item = match action {
        Some(action) => target.action_item(&title, action),
        None => status_item::info_item(target.mtm(), &title),
    };
    if let Some(image) = status_item::symbol("exclamationmark.triangle", None) {
        item.setImage(Some(&image));
    }
    if let Some(detail) = issue.detail() {
        titles.push(&item, title, Trailing::text(detail));
    }
    item
}

/// The row for one device, with its submenu, which fills itself each time it
/// opens ([`fill_device_menu`]).
fn device_item(target: &MenuTarget, row: &DeviceRow) -> Retained<NSMenuItem> {
    let mtm = target.mtm();
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(&row.name));
    if let Some(image) = status_item::symbol(kind_symbol(row.kind), None) {
        item.setImage(Some(&image));
    }
    let submenu = status_item::new_menu(mtm);
    submenu.setDelegate(Some(ProtocolObject::from_ref(target)));
    fill_device_menu(&submenu, target, row);
    target
        .ivars()
        .submenus
        .borrow_mut()
        .push((submenu.clone(), row.clone()));
    item.setSubmenu(Some(&submenu));
    item
}

/// Fill a device's empty or stale `submenu`: what the menu knows about the
/// device, what it can change as the menu remembers it, then the way to its
/// settings.
fn fill_device_menu(submenu: &NSMenu, target: &MenuTarget, row: &DeviceRow) {
    let mtm = target.mtm();
    submenu.removeAllItems();
    for line in row.info() {
        submenu.addItem(&status_item::info_item(mtm, &line));
    }
    let routed = row.routed();
    let memory = routed
        .as_ref()
        .and_then(|device| MEMORY.with_borrow(|memory| memory.get(device).copied()))
        .unwrap_or_default();
    let entries = row.entries(&memory);
    if !entries.is_empty() {
        status_item::add_separator(submenu, mtm);
    }
    for entry in entries {
        let item = match entry {
            Entry::Header(title) => status_item::section_header(mtm, &title),
            Entry::Note(text) => status_item::info_item(mtm, &text),
            Entry::Choice {
                title,
                checked,
                change,
            } => {
                let item = match (change, &routed) {
                    (Some(change), Some(device)) => {
                        target.action_item(&title, Action::Change(device.clone(), change))
                    }
                    _ => status_item::info_item(mtm, &title),
                };
                item.setState(if checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                item
            }
            Entry::Open { title, link } => target.action_item(&title, Action::Open(link)),
            Entry::Separator => {
                status_item::add_separator(submenu, mtm);
                continue;
            }
        };
        submenu.addItem(&item);
    }
    status_item::add_separator(submenu, mtm);
    submenu.addItem(&target.action_item(
        &t!("tray.open_device_settings"),
        Action::Open(row.settings.clone()),
    ));
}

/// The row for one webcam, with its submenu: whether it is in use, then the
/// ways to its preview and its settings.
fn camera_item(target: &MenuTarget, camera: &CameraRow) -> Retained<NSMenuItem> {
    let mtm = target.mtm();
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(&camera.name));
    if let Some(image) = status_item::symbol(kind_symbol(DeviceKind::Camera), None) {
        item.setImage(Some(&image));
    }
    let submenu = status_item::new_menu(mtm);
    submenu.addItem(&status_item::info_item(mtm, &camera.state_text()));
    status_item::add_separator(&submenu, mtm);
    submenu.addItem(&target.action_item(
        &t!("tray.open_camera_preview"),
        Action::Open(camera.preview.clone()),
    ));
    submenu.addItem(&target.action_item(
        &t!("tray.open_device_settings"),
        Action::Open(camera.settings.clone()),
    ));
    item.setSubmenu(Some(&submenu));
    item
}

/// What a device row's trailing part shows for `detail`.
fn trailing(detail: Detail) -> Trailing {
    match detail {
        Detail::Battery(battery) => {
            Trailing::glyph(battery_glyph(&battery), battery_text(&battery))
        }
        Detail::Text(text) => Trailing::text(text),
    }
}

/// The battery glyph for `battery`: filled to its reading, bolted while it
/// charges, orange when low and red when critical.
fn battery_glyph(battery: &BatteryInfo) -> Option<Retained<NSImage>> {
    let name = if is_charging(battery) {
        "battery.100.bolt"
    } else {
        match BatteryFill::of(battery) {
            BatteryFill::Empty => "battery.0",
            BatteryFill::Quarter => "battery.25",
            BatteryFill::Half => "battery.50",
            BatteryFill::ThreeQuarters => "battery.75",
            BatteryFill::Full => "battery.100",
        }
    };
    let glyph = status_item::symbol(name, Some(&battery_text(battery)))?;
    let fill = match battery.attention() {
        Some(BatteryAttention::Critical) => NSColor::systemRedColor(),
        Some(BatteryAttention::Low) => NSColor::systemOrangeColor(),
        None => NSColor::secondaryLabelColor(),
    };
    Some(status_item::tinted(
        &glyph,
        &[&fill, &NSColor::secondaryLabelColor()],
    ))
}

/// The SF Symbol that stands for a device of `kind`.
const fn kind_symbol(kind: DeviceKind) -> &'static str {
    if kind.is_keyboard_family() {
        return "keyboard";
    }
    match kind {
        DeviceKind::Mouse | DeviceKind::Trackball => "computermouse",
        // The keyboard family returned above.
        DeviceKind::Keyboard | DeviceKind::Numpad | DeviceKind::Dialpad | DeviceKind::Unknown => {
            "questionmark.circle"
        }
        DeviceKind::Presenter | DeviceKind::Remote => "av.remote",
        DeviceKind::Touchpad => "hand.point.up.left",
        DeviceKind::Tablet => "pencil.tip",
        DeviceKind::Gamepad | DeviceKind::Joystick => "gamecontroller",
        DeviceKind::Headset => "headphones",
        DeviceKind::Camera => "web.camera",
        DeviceKind::Light => "lightbulb",
    }
}

/// The current-application row, with its submenu: which buttons it runs
/// with, the way to edit them, and the applications used before it.
fn app_item(target: &MenuTarget, app: &AppRow) -> Retained<NSMenuItem> {
    let mtm = target.mtm();
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(&app.current.name));
    if let Some(image) = status_item::symbol("macwindow", None) {
        item.setImage(Some(&image));
    }
    let submenu = status_item::new_menu(mtm);
    submenu.addItem(&status_item::info_item(mtm, &app.summary()));
    submenu.addItem(&target.action_item(
        &t!("tray.edit_app_buttons", app => app.current.name),
        Action::Open(app.current.profile.clone()),
    ));
    if !app.recent.is_empty() {
        status_item::add_separator(&submenu, mtm);
        submenu.addItem(&status_item::section_header(mtm, &t!("tray.recent_apps")));
        let mut titles = TrailingTitles::default();
        for recent in &app.recent {
            let row = target.action_item(&recent.name, Action::Open(recent.profile.clone()));
            titles.push(
                &row,
                recent.name.clone(),
                Trailing::text(recent.buttons.text()),
            );
            submenu.addItem(&row);
        }
        titles.apply();
    }
    item.setSubmenu(Some(&submenu));
    item
}

/// The Flow row titled `title`, with its submenu: each paired computer's link,
/// then the way to Flow's settings.
fn flow_item(target: &MenuTarget, flow: &FlowRow, title: &str) -> Retained<NSMenuItem> {
    let mtm = target.mtm();
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    if let Some(image) = status_item::symbol("display.2", None) {
        item.setImage(Some(&image));
    }
    let submenu = status_item::new_menu(mtm);
    if flow.peers.is_empty() {
        submenu.addItem(&status_item::info_item(mtm, &t!("flow.status_no_peers")));
    } else {
        submenu.addItem(&status_item::section_header(
            mtm,
            &t!("tray.paired_computers"),
        ));
        let mut titles = TrailingTitles::default();
        for peer in &flow.peers {
            let row = status_item::info_item(mtm, &peer.name);
            titles.push(&row, peer.name.clone(), Trailing::text(peer.state_text()));
            submenu.addItem(&row);
        }
        titles.apply();
    }
    status_item::add_separator(&submenu, mtm);
    submenu.addItem(&target.action_item(
        &t!("tray.flow_settings"),
        Action::Open(DeeplinkCommand::OpenFlow),
    ));
    item.setSubmenu(Some(&submenu));
    item
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
