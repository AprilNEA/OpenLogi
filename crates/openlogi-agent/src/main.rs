//! OpenLogi background agent — headless, always-on.
//!
//! Owns the CGEventTap hook and the HID++ device path (gesture capture, DPI,
//! SmartShift), serves the GUI over a Unix-socket tarpc IPC, reconciles its own
//! launchd autostart, and (macOS) hosts the menu-bar status item. The async
//! core runs on a tokio runtime; on macOS the process main thread hosts the
//! AppKit run loop the menu bar requires.

mod launch_agent;
mod pairing;
mod server;
#[cfg(target_os = "macos")]
mod status_item;
#[cfg(target_os = "macos")]
mod tray;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openlogi_agent_core::orchestrator::Orchestrator;
use openlogi_agent_core::{hook_runtime, watchers};
use openlogi_core::config::Config;
use openlogi_hook::Hook;
use tokio::sync::Mutex;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::server::AgentServer;

fn main() {
    init_tracing();

    // Single-instance guard: the agent owns all device I/O, the CGEventTap, and
    // the IPC socket, so a second agent must never start — launchd's KeepAlive
    // racing the GUI's one-shot auto-spawn could otherwise bring up two, and the
    // loser would steal the socket and install a duplicate event tap. Held for
    // the whole process; the OS releases it on exit (crash-recovery is free).
    let _guard = match openlogi_core::single_instance::acquire("agent.lock") {
        Ok(g) => g,
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            info!(path = %path.display(), "another openlogi-agent is already running — exiting");
            return;
        }
        Err(e) => {
            warn!(error = %e, "single-instance check failed — exiting");
            return;
        }
    };

    let config = Config::load_or_default().unwrap_or_else(|e| {
        warn!(error = %e, "could not load config.toml; using defaults");
        Config::default()
    });

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
    #[cfg(target_os = "macos")]
    {
        // Read the menu-bar preference before `config` moves into the core
        // thread; the main thread hosts the tray.
        let show_in_menu_bar = config.app_settings.show_in_menu_bar;
        if let Err(e) = std::thread::Builder::new()
            .name("openlogi-agent-core".into())
            .spawn(move || runtime.block_on(run(config)))
        {
            warn!(error = %e, "could not spawn the agent core thread; exiting");
            return;
        }
        tray::run_app_loop(show_in_menu_bar);
    }
    #[cfg(not(target_os = "macos"))]
    runtime.block_on(run(config));
}

async fn run(config: Config) {
    // Reconcile the agent's launch-at-login autostart and clear the legacy GUI
    // LaunchAgent, before `config` moves into the orchestrator.
    launch_agent::reconcile(config.app_settings.launch_at_login);

    // The agent owns the CGEventTap, so it must be the binary the user authorizes
    // for Accessibility. Fire the prompt at startup when we're not yet trusted so
    // openlogi-agent appears (named correctly) in System Settings even on a
    // launchd start with no GUI. macOS only shows the dialog when we're not
    // already in the list, so this doesn't nag on every login. The GUI's grant
    // button drives the same prompt over IPC (`request_accessibility_prompt`).
    if !Hook::has_accessibility() {
        Hook::prompt_accessibility();
    }

    // The orchestrator is shared with the IPC server (which serves inventory /
    // reload / status) and mutated by the watcher select loop, so it lives
    // behind an async mutex. Locks are brief (a map rebuild or a clone).
    let orchestrator = Arc::new(Mutex::new(Orchestrator::new(config)));
    let shared = orchestrator.lock().await.shared();
    let hook_installed = Arc::new(AtomicBool::new(false));

    // Pairing runs in the agent (it owns device I/O); the GUI drives it over IPC.
    let pairing = Arc::new(pairing::PairingManager::new(shared.clone()));

    // The HID++ control watcher (gesture button, DPI/ModeShift button, thumb
    // wheel) needs no Accessibility permission — start it up front. It reads the
    // shared maps and dispatches bound actions itself; the two pairing flags let
    // it release its capture session while a pairing session owns the receiver.
    watchers::gesture::spawn(
        shared.hook_bindings.clone(),
        shared.gesture_bindings.clone(),
        shared.dpi_cycle.clone(),
        shared.capture_channel.clone(),
        shared.thumbwheel_sensitivity.clone(),
        shared.pairing_active.clone(),
        shared.capture_idle.clone(),
    );

    let mut inventory_rx = watchers::inventory::spawn(Duration::from_secs(2));
    let mut app_rx = watchers::foreground_app::spawn(Duration::from_secs(1));
    let mut accessibility_rx = watchers::accessibility::spawn(Duration::from_millis(1200));

    // IPC server: the GUI connects here for device state + "apply now" commands.
    match openlogi_core::paths::agent_socket_path() {
        Ok(socket_path) => {
            let server = AgentServer {
                orchestrator: Arc::clone(&orchestrator),
                shared: shared.clone(),
                hook_installed: Arc::clone(&hook_installed),
                pairing: Arc::clone(&pairing),
            };
            tokio::spawn(server::run(server, socket_path));
        }
        Err(e) => warn!(error = %e, "could not resolve IPC socket path; IPC disabled"),
    }

    // The CGEventTap hook is installed once Accessibility is granted and dropped
    // if it's revoked (the tap self-disables on revoke regardless; dropping the
    // handle stops its thread).
    let mut hook: Option<Hook> = None;

    info!("openlogi-agent started");
    loop {
        tokio::select! {
            Some(inventories) = inventory_rx.recv() => {
                orchestrator.lock().await.refresh_inventory(&inventories);
            }
            Some(bundle) = app_rx.recv() => {
                orchestrator.lock().await.set_current_app(bundle);
            }
            Some(granted) = accessibility_rx.recv() => {
                if !granted {
                    hook = None;
                    hook_installed.store(false, Ordering::Relaxed);
                }
                if granted && hook.is_none() {
                    info!("accessibility granted — installing OS mouse hook");
                    hook = hook_runtime::start(
                        shared.hook_bindings.clone(),
                        shared.dpi_cycle.clone(),
                        shared.capture_channel.clone(),
                    );
                    hook_installed.store(hook.is_some(), Ordering::Relaxed);
                }
            }
            else => break,
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}
