//! OpenLogi GPUI desktop window.
//!
//! Initial HID++ inventory is collected synchronously on startup (GPUI owns
//! the main thread, so we can't move it onto a tokio runtime). Live polling
//! lands when there's something to react to.

// Without this Windows runs the exe as a console app and pops a terminal
// window behind the UI. Debug builds keep the console so logs stay visible.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

/// Translate `key` (an English msgid) to the current locale and wrap it as a
/// [`gpui::SharedString`], ready for `.child(...)` / `.label(...)` / menu items.
/// Forwards `rust_i18n` interpolation, e.g. `tr!("Bind %{name}", name => x)`.
///
/// Defined before the `mod` declarations so every submodule can use it without
/// an import (textual macro scope). Pairs with the `rust_i18n::i18n!` below.
macro_rules! tr {
    ($($args:tt)*) => {
        // `t!` yields `Cow<'static, str>`. A borrowed hit — the common case: a
        // found translation or the English-key fallback — wraps into a
        // `SharedString` with no copy; only owned (interpolated) results allocate.
        match ::rust_i18n::t!($($args)*) {
            ::std::borrow::Cow::Borrowed(s) => ::gpui::SharedString::from(s),
            ::std::borrow::Cow::Owned(s) => ::gpui::SharedString::from(s),
        }
    };
}

mod app;
mod app_assets;
mod app_menu;
mod asset;
mod components;
mod data;
mod i18n;
mod ipc_client;
mod mouse_model;
mod platform;
mod state;
mod theme;
mod windows;

// Loads the Crowdin-managed `crates/openlogi-gui/locales/*.yml` files at compile
// time and generates the `t!`/`tr!` lookup backend for this crate. `fallback =
// "en"` matches the codes gpui-component ships, so the framework's own widgets
// localize alongside ours.
rust_i18n::i18n!("locales", fallback = "en");

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{
    AppContext, BorrowAppContext as _, Bounds, SharedString, Size, Styled, TitlebarOptions,
    WindowBounds, WindowOptions, px,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::Config;
use openlogi_core::device::{DeviceInventory, DeviceModelInfo};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app::AppView;
use crate::state::AppState;

fn dispatch_gui_command(command: DeeplinkCommand, cx: &mut gpui::App) {
    use DeeplinkCommand as Cmd;
    match command {
        Cmd::Quit => cx.quit(),
        // Always route Show through `open_main_window`: it re-focuses (and
        // deminiaturizes) an existing window or opens a fresh one, so the tray's
        // "Show Main Window" works whether or not a window is already up.
        Cmd::Show => open_main_window(&[], cx),
        // The aux windows are standalone; open the main window first as the
        // session anchor (no-op when one is already open) so closing the aux
        // window doesn't leave the app windowless — and quitting — by surprise.
        Cmd::OpenSettings => {
            ensure_main_window(cx);
            windows::settings::open(cx);
        }
        Cmd::OpenAbout => {
            ensure_main_window(cx);
            windows::about::open(cx);
        }
        Cmd::CheckForUpdates => {
            ensure_main_window(cx);
            app_menu::check_for_updates(cx);
        }
    }
}

/// Open the main window as the session anchor when no window is currently open.
fn ensure_main_window(cx: &mut gpui::App) {
    if cx.windows().is_empty() {
        open_main_window(&[], cx);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "startup orchestration: watcher spawns + the GPUI run/event loop read most clearly inline"
)]
fn main() -> Result<()> {
    init_tracing();

    let _guard = match openlogi_core::single_instance::acquire("openlogi.lock") {
        Ok(g) => g,
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            info!(
                path = %path.display(),
                "another OpenLogi instance is already running — exiting"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::from(e).context("single-instance check")),
    };

    // Start with no devices and never block startup on HID enumeration — a
    // sleeping or unresponsive device must not be able to wedge the main thread
    // before the window opens. The inventory watcher (spawned below) enumerates
    // on its first tick and `AppState::refresh_inventories` wires up devices,
    // bindings, and the hook live; asset sync is kicked off in the background
    // when the first devices appear (see the `inventory_rx` arm).
    let inventories: Vec<DeviceInventory> = Vec::new();

    let initial_config = Config::load_or_default().unwrap_or_else(|e| {
        warn!(error = %e, "could not load config.toml; using defaults");
        Config::default()
    });

    // Resolve the UI locale before any menu or window is built so the first
    // frame already renders in the right language.
    i18n::apply(&initial_config.app_settings);

    // The always-on agent owns the hook, the HID++ capture, and all device I/O.
    // The GUI is a client: it polls inventory + status and forwards device
    // commands over IPC. Started here so the first poll is already in flight.
    let ipc_client::IpcClient {
        updates: mut ipc_updates,
        commands: ipc_commands,
        pairing: mut ipc_pairing,
    } = ipc_client::spawn(std::time::Duration::from_secs(2));

    // `with_assets` registers the embedded app logo ([`app_assets`]) plus the
    // lucide SVGs that back `gpui_component::IconName`; without it `img()` /
    // `Icon` would fail to load.
    let app = gpui_platform::application().with_assets(app_assets::AppAssets);

    // URL scheme: `open openlogi://open-settings` from the agent's tray or
    // external apps. Works for both cold start (macOS launches the app then
    // delivers the URL) and warm reactivation (delivered to the running app).
    let (gui_cmd_tx, mut gui_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<DeeplinkCommand>();
    app.on_open_urls({
        let tx = gui_cmd_tx.clone();
        move |urls| {
            for url in &urls {
                if let Some(cmd) = DeeplinkCommand::parse_url(url) {
                    let _ = tx.send(cmd);
                } else {
                    warn!(url, "unknown openlogi:// command — ignoring");
                }
            }
        }
    });

    // Reopen the window when the app is relaunched with none open (dock click).
    app.on_reopen(|cx| open_main_window(&[], cx));

    app.run(move |cx| {
        gpui_component::init(cx);
        app_menu::install(cx);

        // Seed the Add Device window's initial state. Its buttons drive pairing
        // through the agent over IPC; the agent's pairing long-poll feeds events
        // back into this global via the select loop below.
        cx.set_global(windows::add_device::PairingUi::Idle);

        // Publish the shared updater and, if the user opted in, run one
        // check on launch. Done before `initial_config` is moved into the
        // window-opening task below.
        platform::updater::install(cx, &initial_config.app_settings);

        // On-demand GUI: quit when the last window closes. The agent stays
        // resident and keeps remapping (and hosts the menu-bar item from which
        // the GUI is reopened), so nothing needs the GUI process to linger.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            // Install the hook-shared AppState up front, then open the window at
            // launch; closing it leaves the app live in the menu bar.
            cx.update(|cx| {
                if !cx.has_global::<AppState>() {
                    let cache = asset::AssetResolver::new();
                    cx.set_global(AppState::with_runtime(
                        initial_config,
                        &inventories,
                        &cache,
                        ipc_commands,
                    ));
                }
                open_main_window(&inventories, cx);
            });

            // First launch only: offer to opt in to the update check, since it
            // defaults to off. Marked seen either way so it shows just once.
            cx.update(|cx| {
                let show = cx
                    .try_global::<AppState>()
                    .is_some_and(|s| !s.app_settings().update_prompt_seen);
                if show {
                    windows::update_consent::open(cx);
                }
            });

            // Asset depots are fetched in the background when devices with
            // model info first appear — startup no longer blocks on it. The
            // sync runs once on success; a failed attempt is retried (see
            // SYNC_*) with a growing, capped delay so a permanently-down host
            // isn't polled every tick yet a recovered one still self-heals.
            let sync_state = Arc::new(AtomicU8::new(SYNC_IDLE));
            let mut sync_attempts: u32 = 0;
            let mut last_sync_at: Option<Instant> = None;
            loop {
                tokio::select! {
                    Some(update) = ipc_updates.recv() => {
                        // Kick off (or retry) the one-shot asset sync once a
                        // snapshot carries model info (an empty / unresolved one
                        // would strand the device on the silhouette).
                        let state = sync_state.load(Ordering::Acquire);
                        let backoff_passed = last_sync_at
                            .is_none_or(|t| t.elapsed() >= sync_retry_delay(sync_attempts));
                        if matches!(state, SYNC_IDLE | SYNC_FAILED)
                            && backoff_passed
                            && !collect_models(&update.inventory).is_empty()
                        {
                            sync_attempts = sync_attempts.saturating_add(1);
                            last_sync_at = Some(Instant::now());
                            sync_state.store(SYNC_RUNNING, Ordering::Release);
                            let inv = update.inventory.clone();
                            let state = Arc::clone(&sync_state);
                            std::thread::spawn(move || {
                                let next = if sync_assets_if_needed(&inv) {
                                    SYNC_DONE
                                } else {
                                    SYNC_FAILED
                                };
                                state.store(next, Ordering::Release);
                            });
                        }
                        cx.update(|cx| {
                            let cache = asset::AssetResolver::new();
                            cx.update_global::<AppState, _>(|state, _| {
                                // Merge only *completed* enumerations. A not-yet-ready
                                // agent can only serve an empty pre-enumeration list, and
                                // counting those as misses would wipe the device list (and
                                // pop an open detail page) on every agent restart: at the
                                // 250 ms reconnect cadence the miss grace burns in ~750 ms
                                // while a fresh enumeration takes 1.5–5 s.
                                if update.status.inventory
                                    == openlogi_agent_core::ipc::InventoryHealth::Ready
                                {
                                    state.refresh_inventories(&update.inventory, &cache);
                                }
                                state.set_agent_link(state::AgentLink::Ready(update.status));
                            });
                            cx.refresh_windows();
                        });
                    }
                    Some(update) = ipc_pairing.recv() => {
                        cx.update(|cx| {
                            windows::add_device::apply_update(cx, update);
                        });
                    }
                    Some(cmd) = gui_cmd_rx.recv() => {
                        cx.update(|cx| dispatch_gui_command(cmd, cx));
                    }
                    else => break,
                }
            }
        })
        .detach();
    });

    Ok(())
}

/// Asset-sync state, stored in an [`AtomicU8`] and polled on each inventory
/// snapshot. A failed run flips back to [`SYNC_FAILED`] so the next tick
/// retries, rather than latching the sync off for the whole session.
const SYNC_IDLE: u8 = 0;
const SYNC_RUNNING: u8 = 1;
const SYNC_DONE: u8 = 2;
const SYNC_FAILED: u8 = 3;

/// Minimum gap before re-attempting a failed sync, doubling with each
/// consecutive attempt and capped at a minute. The first attempt is
/// immediate (`last_sync_at` is `None`); after that a permanently-down host
/// is polled ever more slowly (1s, 2s, 4s … 60s) instead of on every tick,
/// while a recovered host still self-heals on the next attempt.
fn sync_retry_delay(attempts: u32) -> Duration {
    const CAP: Duration = Duration::from_secs(60);
    // Cap the shift so `1 << exp` can't overflow, then clamp the result.
    let exp = attempts.saturating_sub(1).min(6);
    Duration::from_secs(1u64 << exp).min(CAP)
}

/// Refresh the asset cache for the connected devices. Returns `true` when the
/// sync completed (or wasn't needed) and `false` when it failed and should be
/// retried. Runs on a dedicated background thread — the HTTP layer's blocking
/// retries are fine here.
fn sync_assets_if_needed(inventories: &[DeviceInventory]) -> bool {
    let probe_cache = asset::AssetResolver::new();
    if !asset::sync::should_run(probe_cache.has_bundle_root()) {
        return true;
    }
    let server =
        std::env::var("OPENLOGI_ASSETS").unwrap_or_else(|_| asset::sync::DEFAULT_BASE.to_string());
    let models = collect_models(inventories);
    match asset::sync::sync(&server, &models) {
        Ok(()) => true,
        Err(e) => {
            warn!(error = ?e, "asset sync failed — will retry on the next device snapshot");
            false
        }
    }
}

fn main_window_options(cx: &mut gpui::App) -> WindowOptions {
    let bounds = Bounds::centered(None, Size::new(px(1100.), px(750.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Min height keeps the buttons tab's mouse model above its scale floor
        // (`MODEL_MIN_H` + the chrome/padding reserve) so its side labels never
        // overlap; below this the model can't shrink further without crowding.
        window_min_size: Some(Size::new(px(720.), px(680.))),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("OpenLogi")),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..WindowOptions::default()
    }
}

/// Open the main window — or focus the one already open. The handle is parked
/// in [`windows::WindowRegistry`] so the dock-icon reopen handler (and any
/// repeat call) re-focuses the live window instead of stacking a duplicate, and
/// a window closed while the app kept running can be brought back.
fn open_main_window(inventories: &[DeviceInventory], cx: &mut gpui::App) {
    let existing = cx.default_global::<windows::WindowRegistry>().main;
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let options = main_window_options(cx);
    let opened = cx.open_window(options, |window, cx| {
        Theme::change(ThemeMode::from(window.appearance()), Some(window), cx);

        let view = cx.new(|cx| AppView::new(inventories, cx));

        let appearance_obs = window.observe_window_appearance(|window, cx| {
            Theme::change(ThemeMode::from(window.appearance()), Some(window), cx);
        });
        view.update(cx, |v, _| v.set_appearance_obs(appearance_obs));

        cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
    });

    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            cx.default_global::<windows::WindowRegistry>().main = Some(handle);
            cx.activate(true);
        }
        Err(e) => warn!(error = %e, "could not open the main window"),
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

/// Flatten every paired device's HID++ model snapshot — that's what the
/// asset sync feeds into the registry lookup.
fn collect_models(inventories: &[DeviceInventory]) -> Vec<(DeviceModelInfo, Option<String>)> {
    inventories
        .iter()
        .flat_map(|inv| inv.paired.iter())
        .filter_map(|p| p.model_info.clone().map(|m| (m, p.codename.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sync_retry_delay;
    use std::time::Duration;

    #[test]
    fn retry_delay_doubles_then_caps() {
        assert_eq!(sync_retry_delay(1), Duration::from_secs(1));
        assert_eq!(sync_retry_delay(2), Duration::from_secs(2));
        assert_eq!(sync_retry_delay(3), Duration::from_secs(4));
        assert_eq!(sync_retry_delay(5), Duration::from_secs(16));
        // Caps at 60s and never overflows the shift for large attempt counts.
        assert_eq!(sync_retry_delay(7), Duration::from_secs(60));
        assert_eq!(sync_retry_delay(u32::MAX), Duration::from_secs(60));
    }
}
