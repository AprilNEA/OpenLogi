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
mod diagnostics;
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

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use backon::{BackoffBuilder, ExponentialBuilder};
use gpui::{
    AppContext, BorrowAppContext as _, Bounds, SharedString, Size, Styled, TitlebarOptions,
    WindowBounds, WindowOptions, px,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};
use openlogi_core::brand::{APP_ID, DeeplinkCommand};
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

/// Update [`AppState`]'s agent link, refreshing the windows only when it
/// actually changed (the IPC client may repeat a notice across reconnect
/// episodes).
fn set_agent_link(link: state::AgentLink, cx: &mut gpui::App) {
    let changed = cx.update_global::<AppState, _>(|state, _| state.set_agent_link(link));
    if changed {
        cx.refresh_windows();
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

    // Manual asset actions (Settings → Assets): Refresh / Clear cache. The
    // sender is published as a global so the Settings window can drive the
    // sync that lives on the main loop below; the loop keeps a second sender
    // (`asset_ctrl_self_tx`) to re-issue a command it had to defer while a
    // sync was in flight.
    let (asset_ctrl_tx, mut asset_ctrl_rx) = tokio::sync::mpsc::unbounded_channel::<AssetCommand>();
    let asset_ctrl_self_tx = asset_ctrl_tx.clone();

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

        // The Settings → Assets buttons drive the asset sync (which lives on
        // the select loop below) through this global.
        cx.set_global(AssetControl(asset_ctrl_tx));

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

            // The asset resolver stats the cache roots and parses the (possibly
            // hundreds-of-KB) index.json, so build it once and reuse it across
            // snapshots — rebuilding only when the background sync lands new
            // assets (below). Rebuilding per snapshot was pure waste: the
            // unchanged-list early-return discarded the fresh records anyway.
            let mut cache = asset::AssetResolver::new();
            // One-time sweep of the legacy pre-rendered glow PNGs the old overlay
            // baked into the user cache; the glow is painted live now, so they're
            // dead bytes. Off-thread so it never delays the first paint.
            std::thread::spawn(asset::cleanup_legacy_glow_pngs);
            // Asset sync runs in the background, in two stages: the first
            // agent snapshot — even a deviceless one — triggers an index
            // prefetch so the registry is on disk before any device needs
            // resolving (devices used to strand on the silhouette forever
            // when the first model-info sighting was missed, #218); per-device
            // depots are fetched as models appear, and a model set that grows
            // later re-arms the sync. Failed attempts retry with a growing,
            // capped delay so a permanently-down host isn't polled every tick
            // yet a recovered one still self-heals.
            let (sync_tx, mut sync_rx) = tokio::sync::mpsc::unbounded_channel::<SyncOutcome>();
            let sync_enabled = asset::sync::should_run(cache.has_bundle_root());
            let mut sync_running = false;
            let mut sync_attempts: u32 = 0;
            let mut last_sync_at: Option<Instant> = None;
            let mut index_refreshed = false;
            let mut synced_keys: HashSet<String> = HashSet::new();
            let mut assets_dirty = false;
            // Most recent completed enumeration, kept so a manual Refresh /
            // Clear (the AssetControl arm below) can sync the current devices
            // without waiting for the next snapshot.
            let mut latest_inv: Vec<DeviceInventory> = Vec::new();
            // A manual Refresh / Clear that arrived while a sync was in
            // flight: stashed here and run by the sync-outcome arm the moment
            // that sync finishes, so a Clear's cache wipe never races the
            // in-flight fetch's writes and the manual fetch is never dropped.
            let mut deferred_manual: Option<AssetCommand> = None;
            // Cleared when the IPC update channel closes (the client thread
            // died), so the select stops polling a closed receiver.
            let mut ipc_open = true;
            loop {
                tokio::select! {
                    update = ipc_updates.recv(), if ipc_open => match update {
                        Some(ipc_client::GuiUpdate::Snapshot(update)) => {
                        // Kick off (or re-arm) the background asset sync. The
                        // index prefetch needs no devices; depot fetches fire
                        // only for models not already synced this session. The
                        // whole automatic path is skipped when the user turned
                        // auto-download off — a manual Refresh still works via
                        // the AssetControl arm below.
                        let auto_download = cx.update(|cx| {
                            cx.try_global::<AppState>()
                                .is_none_or(|s| s.app_settings().auto_download_assets)
                        });
                        let backoff_passed = last_sync_at
                            .is_none_or(|t| t.elapsed() >= sync_retry_delay(sync_attempts));
                        let pending: Vec<_> = collect_models(&update.inventory)
                            .into_iter()
                            .filter(|m| !synced_keys.contains(&model_key(m)))
                            .collect();
                        if auto_download
                            && sync_enabled
                            && !sync_running
                            && backoff_passed
                            && (!index_refreshed || !pending.is_empty())
                        {
                            sync_running = true;
                            sync_attempts = sync_attempts.saturating_add(1);
                            last_sync_at = Some(Instant::now());
                            let tx = sync_tx.clone();
                            std::thread::spawn(move || {
                                let keys = pending.iter().map(model_key).collect();
                                let ok = run_asset_sync(&pending);
                                let _ = tx.send(SyncOutcome { ok, keys });
                            });
                        }
                        // Keep the latest completed enumeration for the manual
                        // Refresh / Clear arm — a not-yet-ready agent's empty
                        // pre-enumeration list must not shrink it.
                        let inventory_ready = update.status.inventory
                            == openlogi_agent_core::ipc::InventoryHealth::Ready;
                        if inventory_ready {
                            latest_inv.clone_from(&update.inventory);
                        }
                        // A completed sync may have put real photos where
                        // silhouettes were resolved: the resolver was rebuilt
                        // when its outcome landed; force this merge through
                        // the unchanged-list early-return so the fresh records
                        // become visible.
                        let force_refresh = std::mem::take(&mut assets_dirty);
                        cx.update(|cx| {
                            let changed = cx.update_global::<AppState, _>(|state, _| {
                                // Merge only *completed* enumerations. A not-yet-ready
                                // agent can only serve an empty pre-enumeration list, and
                                // counting those as misses would wipe the device list (and
                                // pop an open detail page) on every agent restart: at the
                                // 250 ms reconnect cadence the miss grace burns in ~750 ms
                                // while a fresh enumeration takes 1.5–5 s. The diagnostics
                                // snapshot shares the gate so a report copied during that
                                // window keeps the receivers the UI is still showing; the
                                // manual-sync arm reuses `inventory_ready` above.
                                let merged = inventory_ready
                                    && state.refresh_inventories(&update.inventory, &cache, force_refresh);
                                if inventory_ready {
                                    state.store_inventory_snapshot(&update.inventory);
                                }
                                // Bitwise `|`: the link must be set even when the
                                // merge already reported a change.
                                merged | state.set_agent_link(state::AgentLink::Ready(update.status))
                            });
                            // The steady poll mostly repeats an identical snapshot;
                            // skip the full-window invalidation for those.
                            if changed {
                                cx.refresh_windows();
                            }
                        });
                        }
                        Some(ipc_client::GuiUpdate::Unreachable) => {
                            cx.update(|cx| set_agent_link(state::AgentLink::Unreachable, cx));
                        }
                        Some(ipc_client::GuiUpdate::OutdatedGui) => {
                            cx.update(|cx| set_agent_link(state::AgentLink::OutdatedGui, cx));
                        }
                        // The IPC client thread is gone (runtime / thread spawn
                        // failure) — without this the window would show its
                        // connecting spinner forever.
                        None => {
                            ipc_open = false;
                            warn!("IPC update channel closed — agent state unavailable");
                            cx.update(|cx| set_agent_link(state::AgentLink::Unreachable, cx));
                        }
                    },
                    Some(cmd) = asset_ctrl_rx.recv() => {
                        // Manual Refresh / Clear from Settings → Assets. Both
                        // force a fresh fetch for the current devices —
                        // bypassing the auto-download setting and the
                        // release-bundle `sync_enabled` gate — and Clear wipes
                        // the per-user cache first, resetting the retry backoff
                        // so the next automatic attempt isn't delayed.
                        if sync_running {
                            // A sync is writing the cache right now. Stash the
                            // command; the outcome arm re-issues it the moment
                            // that sync finishes, so a Clear never wipes the
                            // cache out from under the running fetch and a
                            // second writer is never spawned alongside it. A
                            // queued Clear wins over a later Refresh — Clear
                            // re-fetches too, so collapsing to Refresh would
                            // silently drop the wipe.
                            if matches!(cmd, AssetCommand::ClearCache)
                                || !matches!(deferred_manual, Some(AssetCommand::ClearCache))
                            {
                                deferred_manual = Some(cmd);
                            }
                        } else {
                            if matches!(cmd, AssetCommand::ClearCache) {
                                if let Err(e) = asset::clear_cache() {
                                    warn!(error = %e, "could not clear asset cache");
                                }
                                // The on-disk cache is gone: drop the bookkeeping
                                // that says otherwise (so the automatic sync can
                                // re-fetch a device that reconnects later), rebuild
                                // the resolver, and repaint so cleared art falls
                                // back to the silhouette (or bundled art)
                                // immediately.
                                synced_keys.clear();
                                index_refreshed = false;
                                cache = asset::AssetResolver::new();
                                cx.update(|cx| {
                                    let changed = cx.update_global::<AppState, _>(|state, _| {
                                        state.refresh_inventories(&latest_inv, &cache, true)
                                    });
                                    if changed {
                                        cx.refresh_windows();
                                    }
                                });
                            }
                            sync_running = true;
                            sync_attempts = 0;
                            last_sync_at = None;
                            let models = collect_models(&latest_inv);
                            let tx = sync_tx.clone();
                            std::thread::spawn(move || {
                                let keys = models.iter().map(model_key).collect();
                                let ok = run_asset_sync(&models);
                                let _ = tx.send(SyncOutcome { ok, keys });
                            });
                        }
                    }
                    // Guarded so this branch is *disabled* while no sync is in
                    // flight — we hold a live `sync_tx`, so an unguarded recv
                    // would pend forever and keep the `else => break` exit
                    // from ever firing once the other channels close.
                    Some(outcome) = sync_rx.recv(), if sync_running => {
                        sync_running = false;
                        if outcome.ok {
                            // Success resets the backoff so a device appearing
                            // later syncs immediately instead of waiting out a
                            // stale failure delay.
                            sync_attempts = 0;
                            last_sync_at = None;
                            index_refreshed = true;
                            synced_keys.extend(outcome.keys);
                            cache = asset::AssetResolver::new();
                            assets_dirty = true;
                        }
                        // A manual Refresh / Clear that landed mid-sync waited
                        // for this moment: re-issue it now that the cache is no
                        // longer being written. The arm above runs it (sync is
                        // idle again) — or re-defers if a new sync already
                        // started in the meantime.
                        if let Some(cmd) = deferred_manual.take() {
                            let _ = asset_ctrl_self_tx.send(cmd);
                        }
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

/// Result of one background asset-sync run, reported back to the select
/// loop: whether the run succeeded, and which model keys it covered (folded
/// into the synced set on success so the same device doesn't re-sync every
/// snapshot).
struct SyncOutcome {
    ok: bool,
    keys: Vec<String>,
}

/// Session-stable identity for a synced model: the HID++ model ids plus the
/// extended-model byte (the colour-variant selector) and the codename the
/// depot match falls back on. Models that collapse to one key would resolve
/// to the same depot files anyway.
fn model_key((model, codename): &(DeviceModelInfo, Option<String>)) -> String {
    format!(
        "{:02x}:{:04x}:{:04x}:{:04x}:{}",
        model.extended_model_id,
        model.model_ids[0],
        model.model_ids[1],
        model.model_ids[2],
        codename.as_deref().unwrap_or_default()
    )
}

/// A manual asset action requested from the Settings → Assets tab, pushed to
/// the main event loop via [`AssetControl`].
pub enum AssetCommand {
    /// Force-fetch assets for the connected devices now, bypassing the
    /// automatic download policy.
    Refresh,
    /// Delete the per-user cache, then re-fetch.
    ClearCache,
}

/// Global handle the Settings window uses to push [`AssetCommand`]s into the
/// main loop, mirroring how the Add Device window drives pairing.
pub struct AssetControl(pub tokio::sync::mpsc::UnboundedSender<AssetCommand>);

impl gpui::Global for AssetControl {}

/// Minimum gap before re-attempting a failed sync, doubling with each
/// consecutive attempt and capped at a minute. The first attempt is
/// immediate (`last_sync_at` is `None`); after that a permanently-down host
/// is polled ever more slowly (1s, 2s, 4s … 60s) instead of on every tick,
/// while a recovered host still self-heals on the next attempt.
fn sync_retry_delay(attempts: u32) -> Duration {
    ExponentialBuilder::default()
        .without_max_times()
        .build()
        .nth(attempts.saturating_sub(1).min(6) as usize)
        .unwrap_or(Duration::from_secs(60))
}

/// Refresh the asset cache: the shared index always, plus the depots for
/// `models`. Returns `true` when the sync completed and `false` when it
/// failed and should be retried. Runs on a dedicated background thread —
/// the HTTP layer's blocking retries are fine here. (Whether sync runs at
/// all is the caller's gate: the automatic path checks `should_run` once at
/// startup plus the auto-download setting; the Settings → Assets manual
/// actions always fetch, even in a release build that would otherwise serve
/// only bundled art.)
fn run_asset_sync(models: &[(DeviceModelInfo, Option<String>)]) -> bool {
    let server =
        std::env::var("OPENLOGI_ASSETS").unwrap_or_else(|_| asset::sync::DEFAULT_BASE.to_string());
    match asset::sync::sync(&server, models) {
        Ok(()) => true,
        Err(e) => {
            warn!(error = ?e, "asset sync failed — will retry with backoff");
            false
        }
    }
}

fn main_window_options(cx: &mut gpui::App) -> WindowOptions {
    let bounds = Bounds::centered(None, Size::new(px(1100.), px(750.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Advertise a Wayland xdg-toplevel app_id (and X11 WM_CLASS). Without it
        // the window ships no app_id, so GNOME's `get_wm_class()` returns empty
        // and our own `gnome_shell` frontmost backend reports OpenLogi as `None`
        // (and the dash can't group the window under its launcher icon). The id
        // is the shared `brand::APP_ID`, matching the desktop file's
        // `StartupWMClass` and the macOS bundle-id family.
        app_id: Some(APP_ID.into()),
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

        let view = cx.new(|cx| AppView::new(inventories, window, cx));

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
