//! OpenLogi GPUI desktop window.
//!
//! Initial HID++ inventory is collected synchronously on startup (GPUI owns
//! the main thread, so we can't move it onto a tokio runtime). Live polling
//! lands when there's something to react to.

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
mod hardware;
mod hook_runtime;
mod i18n;
mod mouse_model;
mod platform;
mod state;
mod theme;
mod watchers;
mod windows;

// Loads `crates/openlogi-gui/locales/*.yml` at compile time and generates the
// `t!`/`tr!` lookup backend for this crate. `fallback = "en"` matches the codes
// gpui-component ships, so the framework's own widgets localize alongside ours.
rust_i18n::i18n!("locales", fallback = "en");

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{
    AppContext, BorrowAppContext as _, Bounds, SharedString, Size, Styled, TitlebarOptions,
    WindowBounds, WindowOptions, px,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};
use openlogi_core::config::Config;
use openlogi_core::device::{DeviceInventory, DeviceModelInfo};
use openlogi_hook::Hook;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app::AppView;
use crate::hook_runtime::BindingMap;
use crate::state::{AppState, DpiCycleState};

#[allow(
    clippy::too_many_lines,
    reason = "startup orchestration: watcher spawns + the GPUI run/event loop read most clearly inline"
)]
fn main() -> Result<()> {
    init_tracing();

    let _guard = match platform::single_instance::acquire() {
        Ok(g) => g,
        Err(platform::single_instance::InstanceError::AlreadyRunning { path }) => {
            info!(
                path = %path.display(),
                "another OpenLogi instance is already running — exiting"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::from(e).context("single-instance check")),
    };

    reconcile_early_config();

    // Start with no devices and never block startup on HID enumeration — a
    // sleeping or unresponsive device must not be able to wedge the main thread
    // before the window opens. The inventory watcher (spawned below) enumerates
    // on its first tick and `AppState::refresh_inventories` wires up devices,
    // bindings, and the hook live; asset sync is kicked off in the background
    // when the first devices appear (see the `inventory_rx` arm).
    let inventories: Vec<DeviceInventory> = Vec::new();

    let (hook_bindings, gesture_bindings, dpi_cycle, initial_config) =
        load_config_and_bindings(&inventories);
    // The capture session publishes its open HID++ channel here so DPI /
    // SmartShift writes reuse it instead of opening their own.
    let capture_channel: openlogi_hid::CaptureChannel = Arc::new(RwLock::new(None));
    // Thumb-wheel sensitivity, shared between the gesture watcher (reader) and
    // the GPUI-owned `AppState` (writer). Created here so both sides hold the
    // same atomic from the first event.
    let thumbwheel_sensitivity = Arc::new(std::sync::atomic::AtomicI32::new(
        initial_config.app_settings.thumbwheel_sensitivity,
    ));
    let hook_arcs = (
        Arc::clone(&hook_bindings),
        Arc::clone(&dpi_cycle),
        Arc::clone(&capture_channel),
    );

    // Resolve the UI locale before any menu or window is built so the first
    // frame already renders in the right language.
    i18n::apply(&initial_config.app_settings);

    // HID++ control capture (gesture button, DPI/ModeShift button, thumb wheel)
    // runs independently of the CGEventTap hook — it needs no Accessibility
    // permission — so start it up front for the active device.
    watchers::gesture::spawn(
        Arc::clone(&hook_bindings),
        Arc::clone(&gesture_bindings),
        Arc::clone(&dpi_cycle),
        Arc::clone(&capture_channel),
        Arc::clone(&thumbwheel_sensitivity),
    );

    let mut inventory_rx = watchers::inventory::spawn(std::time::Duration::from_secs(2));
    let mut app_rx = watchers::foreground_app::spawn(std::time::Duration::from_secs(1));
    let mut accessibility_rx =
        watchers::accessibility::spawn(std::time::Duration::from_millis(1200));
    let (pairing_ctrl_tx, mut pairing_evt_rx) = watchers::pairing::spawn();

    // Status-item (tray) click events (Open / Quit), drained by a dedicated
    // task below. macOS-only: there is no status item on other platforms.
    #[cfg(target_os = "macos")]
    let (tray_tx, mut tray_rx) =
        tokio::sync::mpsc::unbounded_channel::<platform::tray::TrayEvent>();

    // Whether the menu-bar (status item) icon is shown. Read once here for the
    // initial install/visibility; live toggles go through `set_show_in_menu_bar`.
    #[cfg(target_os = "macos")]
    let show_in_menu_bar = initial_config.app_settings.show_in_menu_bar;

    // macOS autostart passes `--minimized` (see launch_agent.rs) to come up in
    // the tray with no window — only meaningful when the tray is on. No tray
    // elsewhere (or with it off), so the window always opens.
    #[cfg(target_os = "macos")]
    let start_minimized = show_in_menu_bar && std::env::args().any(|a| a == "--minimized");
    #[cfg(not(target_os = "macos"))]
    let start_minimized = false;

    // `with_assets` registers the embedded app logo ([`app_assets`]) plus the
    // lucide SVGs that back `gpui_component::IconName`; without it `img()` /
    // `Icon` would fail to load.
    let app = gpui_platform::application().with_assets(app_assets::AppAssets);

    // Reopen the window when the app is relaunched with none open (dock click).
    app.on_reopen(|cx| open_main_window(&[], cx));

    app.run(move |cx| {
        gpui_component::init(cx);
        app_menu::install(cx);

        // Publish the pairing control sender + initial UI state so the Add
        // Device window's buttons can drive the watcher via globals.
        cx.set_global(windows::add_device::PairingControl(pairing_ctrl_tx));
        cx.set_global(windows::add_device::PairingUi::Idle);

        if !Hook::has_accessibility() {
            Hook::prompt_accessibility();
        }

        // Publish the shared updater and, if the user opted in, run one
        // check on launch. Done before `initial_config` is moved into the
        // window-opening task below.
        platform::updater::install(cx, &initial_config.app_settings);

        // Status-item / tray (macOS only). Always created so the "Show in menu
        // bar" setting can show / hide it live; its initial visibility follows
        // the stored setting. The window opens at launch and on demand from its
        // menu.
        #[cfg(target_os = "macos")]
        {
            platform::tray::install(tray_tx);
            platform::tray::set_visible(show_in_menu_bar);
        }

        #[cfg(target_os = "macos")]
        cx.on_app_quit(|_| async {
            platform::tray::uninstall();
        })
        .detach();

        // Keep the activation policy in step with window presence — but only
        // while the menu-bar icon is on. Last window closed + tray on → drop to
        // accessory (no Dock/menu bar); tray off → stay a regular Dock app so
        // there's still a way back in. `open_main_window` restores Regular
        // whenever a window opens.
        #[cfg(target_os = "macos")]
        cx.on_window_closed(|cx, _| {
            let tray_on = cx
                .try_global::<AppState>()
                .is_some_and(|s| s.app_settings().show_in_menu_bar);
            if tray_on && cx.windows().is_empty() {
                platform::tray::hide_from_dock();
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            // Install the hook-shared AppState up front, then open the window at
            // launch; closing it leaves the app live in the menu bar.
            cx.update(|cx| {
                if !cx.has_global::<AppState>() {
                    let cache = asset::AssetResolver::new();
                    cx.set_global(AppState::with_runtime_shared(
                        initial_config,
                        &inventories,
                        &cache,
                        hook_bindings,
                        gesture_bindings,
                        dpi_cycle,
                        thumbwheel_sensitivity,
                    ));
                }
                if !start_minimized {
                    open_main_window(&inventories, cx);
                }
                #[cfg(target_os = "macos")]
                if start_minimized {
                    // Autostart: live in the menu-bar tray with no window.
                    platform::tray::hide_from_dock();
                }
                #[cfg(target_os = "macos")]
                platform::tray::set_device_lines(&tray_device_lines(cx));
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

            let mut hook_handle = None;
            // Asset depots are fetched in the background when devices first
            // appear — startup no longer blocks on it. The sync runs once on
            // success; a failed attempt is retried (see SYNC_*) with a
            // growing, capped delay so a permanently-down host isn't polled
            // every tick yet a recovered one still self-heals.
            let sync_state = Arc::new(AtomicU8::new(SYNC_IDLE));
            let mut sync_attempts: u32 = 0;
            let mut last_sync_at: Option<Instant> = None;
            loop {
                tokio::select! {
                    Some(new_inv) = inventory_rx.recv() => {
                        // Kick off (or retry) the one-shot asset sync. Gate on a
                        // snapshot that actually carries model info — `!is_empty()`
                        // alone could fire on a device whose DeviceInformation read
                        // hasn't resolved yet, leaving its art un-synced. `RUNNING`
                        // blocks a second concurrent sync; `DONE` latches it off;
                        // `FAILED` retries once its backoff window elapses, so a
                        // transient network error no longer strands the device on
                        // the silhouette until an app restart.
                        let state = sync_state.load(Ordering::Acquire);
                        let backoff_passed = last_sync_at
                            .is_none_or(|t| t.elapsed() >= sync_retry_delay(sync_attempts));
                        if matches!(state, SYNC_IDLE | SYNC_FAILED)
                            && backoff_passed
                            && !collect_models(&new_inv).is_empty()
                        {
                            sync_attempts = sync_attempts.saturating_add(1);
                            last_sync_at = Some(Instant::now());
                            sync_state.store(SYNC_RUNNING, Ordering::Release);
                            let inv = new_inv.clone();
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
                                state.refresh_inventories(&new_inv, &cache);
                                state.scanning = false;
                            });
                            #[cfg(target_os = "macos")]
                            platform::tray::set_device_lines(&tray_device_lines(cx));
                        });
                    }
                    Some(bundle) = app_rx.recv() => {
                        cx.update(|cx| {
                            cx.update_global::<AppState, _>(|state, _| {
                                state.set_current_app(bundle);
                            });
                        });
                    }
                    Some(granted) = accessibility_rx.recv() => {
                        if !granted {
                            hook_handle = None;
                        }
                        cx.update(|cx| {
                            if cx.has_global::<AppState>() {
                                cx.update_global::<AppState, _>(|state, _| {
                                    state.accessibility_granted = granted;
                                });
                            }
                            cx.refresh_windows();
                        });
                        if granted && hook_handle.is_none() {
                            info!("accessibility granted — installing OS mouse hook");
                            hook_handle = hook_runtime::start(
                                Arc::clone(&hook_arcs.0),
                                Arc::clone(&hook_arcs.1),
                                Arc::clone(&hook_arcs.2),
                            );
                        }
                    }
                    Some(event) = pairing_evt_rx.recv() => {
                        cx.update(|cx| {
                            windows::add_device::apply_event(cx, event);
                        });
                    }
                    else => break,
                }
            }
        })
        .detach();

        // Drain status-item menu clicks (macOS only). Kept off the main select
        // loop above because `tokio::select!` branches can't be `#[cfg]`-gated,
        // and the whole status item is macOS-only anyway.
        #[cfg(target_os = "macos")]
        cx.spawn(async move |cx| {
            while let Some(event) = tray_rx.recv().await {
                cx.update(|cx| match event {
                    platform::tray::TrayEvent::Open => open_main_window(&[], cx),
                    platform::tray::TrayEvent::Quit => cx.quit(),
                    platform::tray::TrayEvent::Refresh => {
                        platform::tray::refresh_labels();
                        platform::tray::set_device_lines(&tray_device_lines(cx));
                    }
                });
            }
        })
        .detach();
    });

    Ok(())
}

fn reconcile_early_config() {
    let early_config = Config::load_or_default().ok();
    if let Some(cfg) = early_config.as_ref() {
        platform::launch_agent::reconcile(cfg.app_settings.launch_at_login);
    }
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
        window_min_size: Some(Size::new(px(720.), px(520.))),
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
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            #[cfg(target_os = "macos")]
            platform::tray::show_in_dock();
            return;
        }
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
            #[cfg(target_os = "macos")]
            platform::tray::show_in_dock();
        }
        Err(e) => warn!(error = %e, "could not open the main window"),
    }
}

/// Format the status-item device line from the live [`AppState`], e.g.
/// `"MX Master 3S · 80%"`, or a placeholder when nothing is connected.
#[cfg(target_os = "macos")]
fn tray_device_lines(cx: &gpui::App) -> Vec<String> {
    cx.try_global::<AppState>().map_or_else(Vec::new, |state| {
        state
            .device_list
            .iter()
            .map(|record| match &record.battery {
                Some(battery) => format!("{} · {}%", record.display_name, battery.percentage),
                None => record.display_name.clone(),
            })
            .collect()
    })
}

/// Load config from disk and build the initial hook-shared state using the
/// same selection and binding rules as [`AppState::with_runtime_shared`].
/// Pre-populating these `Arc`s here means the hook and gesture watcher see the
/// right bindings, gestures, *and* DPI presets from the very first event, well
/// before the GPUI global is installed.
fn load_config_and_bindings(
    inventories: &[DeviceInventory],
) -> (
    BindingMap,
    watchers::gesture::GestureBindings,
    Arc<RwLock<DpiCycleState>>,
    Config,
) {
    let config = match Config::load_or_default() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "could not load config.toml; using default bindings");
            Config::default()
        }
    };

    let cache = asset::AssetResolver::new();
    let (bindings, gesture_bindings, dpi_cycle) =
        AppState::initial_hook_state(&config, inventories, &cache);
    let bindings_arc = Arc::new(RwLock::new(bindings));
    let gesture_arc = Arc::new(RwLock::new(gesture_bindings));
    let dpi_cycle_arc = Arc::new(RwLock::new(dpi_cycle));

    (bindings_arc, gesture_arc, dpi_cycle_arc, config)
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
