//! OpenLogi background agent — headless, always-on.
//!
//! Owns the CGEventTap hook and the HID++ device path (gesture capture, DPI,
//! SmartShift), serves the GUI over a Unix-socket tarpc IPC, reconciles its own
//! launchd autostart, and (macOS) hosts the menu-bar status item. The async
//! core walks the state machine in `lifecycle` on a tokio runtime; on macOS
//! the process main thread hosts the AppKit run loop the menu bar requires.

// Without this Windows runs the exe as a console app and pops a terminal
// window whenever the GUI's sibling spawn or the Run-key autostart starts the
// agent — "headless" must mean no window of any kind. Debug builds keep the
// console so logs stay visible (matching the GUI's arrangement).
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod autostart;
mod binary_watch;
mod lifecycle;
mod logging;
mod overlay;
mod pairing;
#[cfg(target_os = "linux")]
mod resume_linux;
#[cfg(target_os = "windows")]
mod resume_windows;
// The shared locale catalogs live in `openlogi-ui`; the negotiation that picks
// one is `openlogi_core::locale`. `t!` resolves against a backend each binary
// generates itself, hence the relative path — see
// `tests::the_shared_catalog_is_wired_up` for why a wrong path is silent.
rust_i18n::i18n!("../openlogi-ui/locales", fallback = "en");
mod server;
mod shutdown;
mod startup;
#[cfg(target_os = "macos")]
mod status_item;
mod takeover;
#[cfg(target_os = "macos")]
mod tray;
#[cfg(target_os = "windows")]
mod tray_windows;

use openlogi_core::config::Config;
use tracing::{info, warn};

fn main() {
    logging::init();

    // Single-instance guard: the agent owns all device I/O, the CGEventTap, and
    // the IPC socket, so a second agent must never start — launchd's KeepAlive
    // racing the GUI's one-shot auto-spawn could otherwise bring up two, and the
    // loser would steal the socket and install a duplicate event tap. Held for
    // the whole process; the OS releases it on exit (crash-recovery is free).
    let _guard = match openlogi_core::single_instance::acquire("agent.lock") {
        Ok(g) => g,
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            // The holder may be a leftover from before this binary's update —
            // a pre-self-restart agent never exits on its own, and it would
            // wedge the (newer) GUI on its connecting screen forever. If it
            // provably speaks an older protocol, replace it; otherwise exit
            // as the duplicate we are.
            let Some(g) = takeover::try_replace_stale() else {
                info!(path = %path.display(), "another openlogi-agent is already running — exiting");
                return;
            };
            info!("replaced a stale agent — continuing as the new one");
            g
        }
        Err(e) => {
            warn!(error = %e, "single-instance check failed — exiting");
            return;
        }
    };

    // Watch our own executable and restart as the new image when an app update
    // replaces it — see `binary_watch`. Only the lock-holding (real) agent
    // watches, so a losing duplicate can't restart anything. The overlay is
    // spawned later, once the lifecycle decides the agent is actually wanted —
    // a dormant agent must not bring a helper up.
    let uninstalled = binary_watch::spawn();

    let config = Config::load_or_default().unwrap_or_else(|e| {
        warn!(error = %e, "could not load config.toml; using defaults");
        Config::default()
    });
    // The tray renders localized strings; resolve the stored preference (or
    // the system locale) before any menu is built. A live language switch
    // reaches the running agent through `reload_config`.
    openlogi_core::locale::activate(config.app_settings.language.as_deref());

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!(error = %e, "tokio runtime init failed; agent exiting");
            return;
        }
    };

    // macOS hosts the menu-bar item, which needs an NSApplication run loop on
    // the process main thread — so the async core (orchestrator, IPC, watchers,
    // hook) runs on the tokio runtime on a dedicated thread, and the main thread
    // runs AppKit. Elsewhere there is no tray, so just block on the core.
    let device_io_signal = openlogi_hid::host::device_io_signal();
    let device_io_gate = openlogi_hid::host::device_io_gate();
    #[cfg(target_os = "macos")]
    {
        // Fail closed before the core thread can enumerate or open HID devices.
        // AppKit releases this startup hold only after its workspace observers
        // have received the initial session state and Core Graphics has
        // reported whether the display is already asleep.
        let _ = device_io_signal.suspend();
        // Read the menu-bar preference before `config` moves into the core
        // thread; the main thread hosts the tray.
        let show_in_menu_bar = config.app_settings.show_in_menu_bar;
        let app_icon = config.app_settings.app_icon;
        let hook_stop = std::sync::Arc::new(lifecycle::HookStopRequest::default());
        let core_hook_stop = std::sync::Arc::clone(&hook_stop);
        // The tray waits for the core to declare the agent *armed*: a dormant
        // agent (launch_at_login off, started at login, no client yet) must
        // not put an icon in the menu bar only to vanish seconds later. A
        // dropped sender means the core exited without arming — fall through
        // and let the process end.
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
        if let Err(e) = std::thread::Builder::new()
            .name("openlogi-agent-core".into())
            .spawn(move || {
                runtime.block_on(lifecycle::run(
                    config,
                    uninstalled,
                    armed_tx,
                    core_hook_stop,
                    device_io_gate,
                ));
            })
        {
            warn!(error = %e, "could not spawn the agent core thread; exiting");
            return;
        }
        if armed_rx.recv().is_ok() {
            tray::run_app_loop(show_in_menu_bar, app_icon, device_io_signal, hook_stop);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows hosts the notification-area icon on its own win32 thread
        // (message pump included); the async core keeps the main thread.
        #[cfg(target_os = "windows")]
        {
            tray_windows::spawn(config.app_settings.show_in_menu_bar);
            // Native resume notifications feed the same event seam as macOS
            // and Linux: inventory wakes immediately and replays volatile
            // settings on its settled authoritative snapshot.
            resume_windows::register(device_io_signal.clone());
        }
        #[cfg(target_os = "linux")]
        resume_linux::register(device_io_signal.clone());
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        drop(device_io_signal);
        runtime.block_on(lifecycle::run(config, uninstalled, device_io_gate));
    }
}

#[cfg(test)]
mod tests;
