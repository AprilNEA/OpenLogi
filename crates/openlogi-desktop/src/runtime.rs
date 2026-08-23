//! The GUI's event loop: everything the app does that isn't a render.
//!
//! One task, spawned onto GPUI's executor, owning the state that outlives any
//! single event — the merged device set, the asset resolver, the asset cache —
//! and one `select!` arm per source that can change it: the agent's IPC
//! updates, the camera scan, the Settings → Assets commands, finished
//! downloads, and `openlogi://` deeplinks.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AsyncApp, BorrowAppContext as _, Task};
use openlogi_camera::Camera;
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::{AssetSourcePreference, Config};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_ipc::AgentSnapshot;
use swr_core::SwrClient;
use swr_gpui::GpuiRuntime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::warn;

use crate::services::assets::sync::{AssetCommand, AssetTarget};
use crate::services::assets::{self, sync};
use crate::services::ipc;
use crate::state::{self, AppState, ConfigPersistence};
use crate::{app, windows};

/// How often the UI re-enumerates USB cameras. They are UVC devices the agent
/// never opens, so nothing tells the GUI when one is plugged in — this is the
/// one thing here that still has to be asked on a timer.
const CAMERA_SCAN_PERIOD: Duration = Duration::from_secs(2);

/// What process startup hands the event loop.
pub(crate) struct Startup {
    pub(crate) config: Config,
    pub(crate) persistence: ConfigPersistence,
    /// Device commands the UI sends the agent.
    pub(crate) ipc_commands: UnboundedSender<ipc::Command>,
    /// Agent state, pushed by the IPC client thread.
    pub(crate) updates: UnboundedReceiver<ipc::GuiUpdate>,
    /// Manual asset actions from Settings → Assets.
    pub(crate) asset_commands: UnboundedReceiver<AssetCommand>,
    /// `openlogi://` URLs, from the tray or another app.
    pub(crate) deeplinks: UnboundedReceiver<DeeplinkCommand>,
}

/// Start the event loop. Returns once it is spawned; the task runs for the life
/// of the process.
pub(crate) fn spawn(startup: Startup, cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        let Startup {
            config,
            persistence,
            ipc_commands,
            mut updates,
            mut asset_commands,
            mut deeplinks,
        } = startup;

        // Enumerate webcams off the UI thread: AVFoundation discovery can
        // stall for hundreds of ms on first touch, which must never block
        // the first paint (or, below, a snapshot merge mid-render).
        let cams = cx
            .background_executor()
            .spawn(async { openlogi_camera::enumerate_cameras() })
            .await;

        // Install the hook-shared AppState up front, then open the window at
        // launch; closing it leaves the app live in the menu bar. Start with no
        // devices and never block startup on HID enumeration — a sleeping or
        // unresponsive device must not be able to wedge the main thread before
        // the window opens. The agent's first snapshot wires up devices,
        // bindings and the hook live.
        cx.update(|cx| {
            if !cx.has_global::<AppState>() {
                let cache = assets::AssetResolver::new();
                cx.set_global(AppState::with_runtime(
                    config,
                    &[],
                    &[],
                    &cache,
                    &cams,
                    persistence,
                    ipc_commands,
                ));
            }
            windows::main_window::open(&[], cx);
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

        // One-time sweep of the legacy pre-rendered glow PNGs the old overlay
        // baked into the user cache; the glow is painted live now, so they're
        // dead bytes. Off-thread so it never delays the first paint.
        std::thread::spawn(assets::cleanup_legacy_glow_pngs);

        let (sync_tx, mut sync_done) = tokio::sync::mpsc::unbounded_channel::<bool>();
        let mut rt = cx.update(|cx| Runtime::new(cams, sync_tx, cx));
        let mut camera_scan = Box::pin(cx.background_executor().timer(CAMERA_SCAN_PERIOD));
        // Cleared when the IPC update channel closes (the client thread died),
        // so the select stops polling a closed receiver.
        let mut ipc_open = true;
        loop {
            tokio::select! {
                update = updates.recv(), if ipc_open => {
                    // `None` means the IPC client thread is gone (runtime /
                    // thread spawn failure) — without this the window would
                    // show its connecting spinner forever.
                    let Some(update) = update else {
                        ipc_open = false;
                        warn!("IPC update channel closed — agent state unavailable");
                        cx.update(|cx| set_agent_link(state::AgentLink::Unreachable, cx));
                        continue;
                    };
                    rt.on_agent_update(update, cx);
                }
                // GPUI drives this task on its own executor, never inside a
                // Tokio runtime, so a `tokio::time::interval` panics the moment
                // it is built ("there is no reactor running"). The background
                // executor's timer is what the rest of the app schedules on. It
                // is armed once and re-armed only after it fires, so a busy IPC
                // arm cannot keep starving the scan the way a per-iteration
                // timer would, and a late tick simply runs late instead of
                // catching up — the same behaviour `MissedTickBehavior::Delay`
                // asked for.
                () = &mut camera_scan => {
                    camera_scan.set(cx.background_executor().timer(CAMERA_SCAN_PERIOD));
                    rt.rescan_cameras(cx).await;
                }
                Some(cmd) = asset_commands.recv() => rt.on_asset_command(cmd, cx),
                // Unguarded: we hold a live `sync_tx`, so this arm simply pends
                // between downloads. The old guard existed to let `else => break`
                // fire, which the always-armed camera timer above already makes
                // unreachable. Manual commands no longer queue behind a sync —
                // per-key in-flight state makes the exclusion the cache's job.
                Some(ok) = sync_done.recv() => rt.on_sync_finished(ok, cx),
                Some(cmd) = deeplinks.recv() => {
                    cx.update(|cx| app::deeplink::dispatch(cmd, cx));
                }
                else => break,
            }
        }
    })
    .detach();
}

/// State the event loop carries between events.
struct Runtime {
    /// The camera set last merged into the UI.
    cams: Vec<Camera>,
    /// Consecutive empty camera scans while cameras were showing — see the
    /// grace in [`Runtime::rescan_cameras`].
    camera_misses: u8,
    /// The agent's last reported state, so a camera hotplug can re-run the
    /// merge without waiting for the agent to change something of its own.
    snapshot: Option<AgentSnapshot>,
    /// The asset resolver stats the cache roots and parses the (possibly
    /// hundreds-of-KB) index.json, so it is built once and reused across
    /// snapshots — rebuilt only when a sync lands new assets. Rebuilding per
    /// snapshot was pure waste: the unchanged-list early-return discarded the
    /// fresh records anyway.
    cache: assets::AssetResolver,
    /// The asset tier's cache. Owns the mirror probe and every depot download
    /// as keyed entries — see [`assets::queries`].
    swr: SwrClient,
    /// Whether the *automatic* download path runs in this build at all: a
    /// release bundle already ships the art. Manual actions ignore it.
    auto_sync: bool,
    /// One live cache subscription per device model, keyed by
    /// [`sync::model_key`]. Holding the task is what holds the subscription;
    /// dropping it unsubscribes.
    asset_subs: HashMap<String, Task<()>>,
    /// The registry's own subscription, which no device owns.
    index_sub: Option<Task<()>>,
    sync_tx: UnboundedSender<bool>,
    /// Most recent completed enumeration, kept so a manual Refresh / Clear can
    /// sync the current devices without waiting for the next snapshot.
    inventories: Vec<DeviceInventory>,
    standalone: Vec<StandaloneDevice>,
}

impl Runtime {
    fn new(cams: Vec<Camera>, sync_tx: UnboundedSender<bool>, cx: &App) -> Self {
        let cache = assets::AssetResolver::new();
        let auto_sync = sync::should_run(cache.has_bundle_root());
        let swr = SwrClient::builder()
            .default_options(assets::queries::default_options())
            .build(Arc::new(GpuiRuntime::new(cx)));
        Self {
            cams,
            camera_misses: 0,
            snapshot: None,
            cache,
            swr,
            auto_sync,
            asset_subs: HashMap::new(),
            index_sub: None,
            sync_tx,
            inventories: Vec::new(),
            standalone: Vec::new(),
        }
    }

    /// One update from the agent.
    fn on_agent_update(&mut self, update: ipc::GuiUpdate, cx: &AsyncApp) {
        match update {
            ipc::GuiUpdate::Snapshot(snapshot) => {
                self.apply_snapshot(&snapshot, cx);
                self.snapshot = Some(snapshot);
            }
            ipc::GuiUpdate::Unreachable => {
                cx.update(|cx| set_agent_link(state::AgentLink::Unreachable, cx));
            }
            ipc::GuiUpdate::OutdatedGui => {
                cx.update(|cx| set_agent_link(state::AgentLink::OutdatedGui, cx));
            }
            ipc::GuiUpdate::LightCommandResult {
                key,
                request_id,
                command,
                result,
            } => {
                let changed = cx.update_global::<AppState, _>(|state, _| {
                    state.apply_light_command_result(key, request_id, command, result)
                });
                if changed {
                    cx.update(gpui::App::refresh_windows);
                }
            }
            ipc::GuiUpdate::PairingUndeliverable(failure) => {
                cx.update(|cx| windows::add_device::apply_undeliverable(cx, failure));
                cx.update(gpui::App::refresh_windows);
            }
            ipc::GuiUpdate::ConfigReloadResult(result) => {
                let changed = cx.update_global::<AppState, _>(|state, _| {
                    state.apply_config_reload_result(result)
                });
                if changed {
                    cx.update(gpui::App::refresh_windows);
                }
            }
        }
    }

    /// Merge one agent snapshot plus the locally enumerated camera set into the
    /// UI state, then offer the result to the background asset sync.
    ///
    /// Called from two places on purpose. The agent tells us when *its* half of
    /// the device list changed; cameras are UVC rather than HID++, so they
    /// never come over IPC and the UI scans for them itself — but they feed the
    /// same merge, so a camera hotplug has to be able to drive it with the last
    /// known snapshot.
    fn apply_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &AsyncApp) {
        // Keep the latest completed enumeration for the manual Refresh / Clear
        // arm — a not-yet-ready agent's empty pre-enumeration list must not
        // shrink it.
        let inventory_ready = snapshot.status.inventory == openlogi_ipc::InventoryHealth::Ready;
        if inventory_ready {
            self.inventories.clone_from(&snapshot.inventory);
            self.standalone.clone_from(&snapshot.standalone);
        }
        let pairing_changed =
            cx.update(|cx| windows::add_device::apply_state(cx, snapshot.pairing.clone()));
        let (auto_download, asset_source, models) = cx.update(|cx| {
            let (changed, merged, auto_download, asset_source, models) = cx
                .update_global::<AppState, _>(|state, _| {
                    // Merge only completed enumerations. A scanning agent serves
                    // an empty pre-enumeration list, which must not burn the GUI's
                    // miss grace or replace the last known device set.
                    let merged = inventory_ready
                        && state.refresh_inventories(
                            &snapshot.inventory,
                            &snapshot.standalone,
                            &self.cache,
                            &self.cams,
                        );
                    if inventory_ready {
                        state.store_inventory_snapshot(&snapshot.inventory);
                    }
                    // Bitwise `|`: the link must be set even when the
                    // merge already reported a change.
                    let changed = merged
                        | state.set_agent_link(state::AgentLink::Ready(snapshot.status.clone()))
                        | state.set_camera_active(snapshot.camera_active);
                    let settings = state.app_settings();
                    (
                        changed,
                        merged,
                        settings.auto_download_assets,
                        settings.asset_source,
                        state.asset_models(),
                    )
                });
            if changed || pairing_changed {
                cx.refresh_windows();
            }
            if merged {
                app::menu::rebuild(cx);
            }
            (auto_download, asset_source, models)
        });
        // Offer the merged set to the cache on every snapshot and let it decide:
        // a model synced this session is fresh and answers instantly, and two
        // snapshots racing the same model join one request. Use the UI's merged
        // device set so persisted identities are covered when a live probe
        // temporarily lacks model info.
        if auto_download && self.auto_sync {
            let targets: Vec<_> = models
                .into_iter()
                .chain(camera_targets(&self.cams))
                .collect();
            self.ensure_assets(asset_source, targets.into_iter(), cx);
        }
    }

    /// Re-enumerate cameras and, when the set changed, re-run the merge with
    /// the agent's last snapshot.
    async fn rescan_cameras(&mut self, cx: &AsyncApp) {
        // Nothing to show it to. The app runs from the menu bar with every
        // window closed, and this scan is the only work left that is not driven
        // by an agent change — so idling in the tray should cost nothing. The
        // first tick after a window opens picks up whatever changed.
        if cx.update(|cx| cx.windows().is_empty()) {
            return;
        }
        // Off the UI thread: AVFoundation discovery is far too slow for the
        // render path.
        let scanned = cx
            .background_executor()
            .spawn(async { openlogi_camera::enumerate_cameras() })
            .await;
        // An empty scan gets a two-tick grace before it evicts anything: a USB
        // control seize (e.g. another process's CLI) blinks the camera out of
        // discovery for a moment, and one blink must not tear down the card —
        // or the detail page — the user is looking at.
        if scanned.is_empty() && !self.cams.is_empty() && self.camera_misses < 2 {
            self.camera_misses += 1;
            return;
        }
        self.camera_misses = 0;
        if scanned == self.cams {
            return;
        }
        self.cams = scanned;
        // Taken and put back rather than borrowed: the merge needs the rest of
        // `self` mutably, and cloning a snapshot copies the whole device list.
        if let Some(snapshot) = self.snapshot.take() {
            self.apply_snapshot(&snapshot, cx);
            self.snapshot = Some(snapshot);
        }
    }

    /// A manual Refresh / Clear from Settings → Assets. Both bypass the
    /// auto-download setting and the release-bundle gate, and both mark the
    /// tier stale so the refetch is real rather than a cache hit. Clear wipes
    /// the per-user cache first.
    ///
    /// Clear does not wait for in-flight downloads to finish. A racing write
    /// lands a registry-hashed file through the same atomic replace it always
    /// used, and Clear's own semantics are "wipe, then fetch again" — so the
    /// worst case is the wipe missing a file that was about to be re-fetched
    /// anyway.
    fn on_asset_command(&mut self, cmd: AssetCommand, cx: &AsyncApp) {
        let (models, asset_source) = cx.update(|cx| {
            let state = cx.global::<AppState>();
            (state.asset_models(), state.app_settings().asset_source)
        });
        if cmd == AssetCommand::ClearCache {
            if let Err(e) = assets::clear_cache() {
                warn!(error = %e, "could not clear asset cache");
            }
            // The on-disk cache is gone: rebuild the resolver and repaint so
            // cleared art falls back to the silhouette (or bundled art)
            // immediately.
            self.cache = assets::AssetResolver::new();
            self.refresh_devices(cx);
        }
        assets::queries::invalidate_all(&self.swr);
        let targets: Vec<_> = models
            .into_iter()
            .chain(camera_targets(&self.cams))
            .collect();
        self.ensure_assets(asset_source, targets.into_iter(), cx);
    }

    /// A download landed. Re-resolve against the enlarged cache and repaint;
    /// the whole-record comparison in `refresh_inventories` decides whether
    /// anything actually changed.
    fn on_sync_finished(&mut self, ok: bool, cx: &AsyncApp) {
        if ok {
            self.cache = assets::AssetResolver::new();
            self.refresh_devices(cx);
        }
    }

    /// Rebuild the UI's device records against the current resolver.
    fn refresh_devices(&self, cx: &AsyncApp) {
        cx.update(|cx| {
            let changed = cx.update_global::<AppState, _>(|state, _| {
                state.refresh_inventories(
                    &self.inventories,
                    &self.standalone,
                    &self.cache,
                    &self.cams,
                )
            });
            if changed {
                cx.refresh_windows();
            }
        });
    }

    /// Ask the cache for every model in `targets`, and for the registry alone
    /// when there are none.
    ///
    /// One task per model rather than one per batch: each key carries its own
    /// in-flight state, so a slow depot no longer holds up the rest, and a
    /// failure retries on its own schedule instead of stalling every model
    /// behind one shared backoff. The fetchers run on the background executor —
    /// the HTTP layer blocks, and must never do so on the UI thread.
    fn ensure_assets(
        &mut self,
        source: AssetSourcePreference,
        targets: impl Iterator<Item = AssetTarget>,
        cx: &AsyncApp,
    ) {
        let mut wanted = HashSet::new();
        for target in targets {
            let key = sync::model_key(&target);
            if !self.asset_subs.contains_key(&key) {
                let watcher = assets::queries::watch_model(
                    &self.swr,
                    source,
                    target,
                    self.sync_tx.clone(),
                    cx,
                );
                self.asset_subs.insert(key.clone(), watcher);
            }
            wanted.insert(key);
        }
        // Dropping the task cancels the watcher, which drops the handle and
        // unsubscribes; the entry then ages out on its own.
        self.asset_subs.retain(|key, _| wanted.contains(key));
        // Nothing above keeps the registry warm before the first device
        // appears, and resolution needs it the moment one does.
        if self.index_sub.is_none() {
            self.index_sub = Some(assets::queries::watch_index(
                &self.swr,
                source,
                self.sync_tx.clone(),
                cx,
            ));
        }
    }
}

/// Cameras are enumerated on the UI side (UVC, not HID++), so
/// `AppState::asset_models` — built from the HID++ device list — can't see
/// them. Synthesize their targets so a webcam's product art downloads like any
/// other device's.
fn camera_targets(cams: &[Camera]) -> impl Iterator<Item = AssetTarget> + '_ {
    cams.iter().map(|c| AssetTarget::Hidpp {
        model: state::camera_model_info(c),
        codename: Some(c.name.clone()),
    })
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
