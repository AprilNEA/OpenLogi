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

use gpui::{AppContext as _, AsyncApp, Task};
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
use crate::state::{self, AppState, ConfigPersistence, DeviceKey, StateEvent};
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

        // Install the shared AppState entity up front, then open the window at
        // launch; closing it leaves the app live in the menu bar. Start with no
        // devices and never block startup on HID enumeration — a sleeping or
        // unresponsive device must not be able to wedge the main thread before
        // the window opens. The agent's first snapshot wires up devices,
        // bindings and the hook live.
        let swr = cx.update(|cx| {
            let swr_runtime: Arc<dyn swr_core::Runtime> = Arc::new(GpuiRuntime::new(cx));
            let swr = SwrClient::builder()
                .default_options(assets::queries::default_options())
                .build(swr_runtime.clone());
            if AppState::try_global(cx).is_none() {
                let cache = assets::AssetResolver::new();
                let state = cx.new(|_| {
                    let mut state = AppState::with_runtime(
                        config,
                        &[],
                        &[],
                        &cache,
                        &cams,
                        persistence,
                        ipc_commands,
                    );
                    state.connect_device_reads(swr.clone(), swr_runtime.clone());
                    state
                });
                AppState::set_global(state, cx);
                AppState::load_current_device_reads(cx);
            } else {
                AppState::update(cx, |state, _| {
                    state.connect_device_reads(swr.clone(), swr_runtime);
                });
            }
            windows::main_window::open(&[], cx);
            swr
        });

        // First launch only: offer to opt in to the update check, since it
        // defaults to off. Marked seen either way so it shows just once.
        cx.update(|cx| {
            let show = AppState::try_global(cx)
                .is_some_and(|state| !state.read(cx).app_settings().update_prompt_seen);
            if show {
                windows::update_consent::open(cx);
            }
        });

        // One-time sweep of the legacy pre-rendered glow PNGs the old overlay
        // baked into the user cache; the glow is painted live now, so they're
        // dead bytes. Off-thread so it never delays the first paint.
        std::thread::spawn(assets::cleanup_legacy_glow_pngs);

        reconcile_login_item_at_startup(cx);

        let (sync_tx, mut sync_done) = tokio::sync::mpsc::unbounded_channel::<bool>();
        let mut rt = Runtime::new(cams, sync_tx, swr);
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

/// Reconcile the agent's login-item registration with the `launch_at_login`
/// setting at startup (macOS; a no-op elsewhere): a fresh install registers on
/// its first GUI launch, and a setting changed while the GUI wasn't running
/// catches up here. Only the preference's own gap drives it — a service the
/// user switched off in System Settings (`RequiresApproval`) is respected,
/// never re-registered; the Settings row surfaces that state instead. Skipped
/// for dev profiles: a login item pointing into a `target/` build goes stale
/// on the next `cargo clean`, so dev registration stays an explicit toggle in
/// the dev GUI.
///
/// A detached task, with the XPC round-trips on the background executor, so
/// nothing here delays the first paint — and the preference is read from the
/// live [`AppState`] only *after* the slow status read, so a toggle flipped in
/// a just-opened Settings window wins over whatever the preference was at
/// spawn (the stale-async-input rule; the toggle's own registration call still
/// runs last and agrees).
fn reconcile_login_item_at_startup(cx: &mut gpui::AsyncApp) {
    if openlogi_core::paths::is_dev_profile() {
        return;
    }
    cx.spawn(async move |cx| {
        use crate::platform::login_item::{self, ServiceStatus};
        let status = cx
            .background_executor()
            .spawn(async { login_item::status() })
            .await;
        let enabled = cx.update(|cx| {
            AppState::try_global(cx).map(|state| state.read(cx).app_settings().launch_at_login)
        });
        let Some(enabled) = enabled else { return };
        let register = match (enabled, status) {
            (true, ServiceStatus::NotRegistered) => true,
            (false, ServiceStatus::Enabled | ServiceStatus::RequiresApproval) => false,
            _ => return,
        };
        let result = cx
            .background_executor()
            .spawn(async move { login_item::sync_registration(register) })
            .await;
        if let Err(error) = result {
            tracing::warn!(error, register, "startup login-item reconcile failed");
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
    /// The process-wide swr cache, shared with `AppState`'s device reads. This
    /// runtime owns the asset mirror probe and depot-download subscriptions —
    /// see [`assets::queries`].
    swr: SwrClient,
    /// Whether the *automatic* download path runs in this build at all: a
    /// release bundle already ships the art. Manual actions ignore it.
    auto_sync: bool,
    /// One live cache subscription per [`AssetWatch`], keyed by [`watch_key`].
    /// Holding the task is what holds the subscription; dropping it
    /// unsubscribes.
    asset_subs: Subscriptions<Task<()>>,
    sync_tx: UnboundedSender<bool>,
    /// Most recent completed enumeration, kept so a manual Refresh / Clear can
    /// sync the current devices without waiting for the next snapshot.
    inventories: Vec<DeviceInventory>,
    standalone: Vec<StandaloneDevice>,
}

impl Runtime {
    fn new(cams: Vec<Camera>, sync_tx: UnboundedSender<bool>, swr: SwrClient) -> Self {
        let cache = assets::AssetResolver::new();
        let auto_sync = sync::should_run(cache.has_bundle_root());
        Self {
            cams,
            camera_misses: 0,
            snapshot: None,
            cache,
            swr,
            auto_sync,
            asset_subs: Subscriptions::new(),
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
                cx.update(|cx| {
                    let event_key = DeviceKey::from(key.as_str());
                    AppState::update(cx, |state, cx| {
                        if state.apply_light_command_result(key, request_id, command, result) {
                            cx.emit(StateEvent::LightingChanged(event_key));
                        }
                    });
                });
            }
            ipc::GuiUpdate::PairingUndeliverable(failure) => {
                cx.update(|cx| windows::add_device::apply_undeliverable(cx, failure));
            }
            ipc::GuiUpdate::ConfigReloadResult(result) => {
                cx.update(|cx| {
                    AppState::update(cx, |state, cx| {
                        if state.apply_config_reload_result(result) {
                            cx.emit(StateEvent::SettingsChanged);
                        }
                    });
                });
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
        cx.update(|cx| {
            windows::add_device::apply_state(cx, snapshot.pairing.clone());
        });
        let (auto_download, asset_source, models) = cx.update(|cx| {
            let (merged, auto_download, asset_source, models) =
                AppState::update(cx, |state, cx| {
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
                    let agent_changed =
                        state.set_agent_link(state::AgentLink::Ready(snapshot.status.clone()));
                    let camera_changed = state.set_camera_active(snapshot.camera_active);
                    let foreground_changed = state.set_foreground(snapshot.foreground.clone());
                    if merged {
                        cx.emit(StateEvent::InventoryChanged);
                    }
                    if agent_changed {
                        cx.emit(StateEvent::AgentChanged);
                    }
                    if camera_changed {
                        cx.emit(StateEvent::CameraChanged);
                    }
                    if foreground_changed {
                        cx.emit(StateEvent::ForegroundChanged);
                    }
                    let settings = state.app_settings();
                    (
                        merged,
                        settings.auto_download_assets,
                        settings.asset_source,
                        state.asset_models(),
                    )
                });
            // A reconnect can drop an in-flight reply without changing the
            // inventory. Retry any cache entry that the reply lifecycle reset
            // to Unknown on every completed snapshot; resolved entries no-op.
            if inventory_ready {
                AppState::load_current_device_reads(cx);
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
            let state = AppState::global(cx);
            let state = state.read(cx);
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
            let changed = AppState::update(cx, |state, cx| {
                let changed = state.refresh_inventories(
                    &self.inventories,
                    &self.standalone,
                    &self.cache,
                    &self.cams,
                );
                if changed {
                    cx.emit(StateEvent::InventoryChanged);
                }
                changed
            });
            if changed {
                AppState::load_current_device_reads(cx);
                app::menu::rebuild(cx);
            }
        });
    }

    /// Keep exactly one cache subscription per thing we want fetched: the
    /// shared registry, plus every device model we want art for.
    ///
    /// One subscription per model rather than one batch: each key carries its
    /// own in-flight state, so a slow depot no longer holds up the rest and a
    /// failure retries on its own schedule instead of stalling every model
    /// behind one shared backoff. The fetchers run on the background executor —
    /// the HTTP layer blocks, and must never do so on the UI thread.
    ///
    /// The registry rides the same reconciliation rather than being held apart:
    /// keeping it separate meant it also had to grow its own answer to the
    /// source changing, and it has none of its own.
    fn ensure_assets(
        &mut self,
        source: AssetSourcePreference,
        targets: impl Iterator<Item = AssetTarget>,
        cx: &AsyncApp,
    ) {
        let (swr, tx) = (&self.swr, &self.sync_tx);
        // Nothing has a device behind the registry, so it is named here.
        let wanted = std::iter::once(AssetWatch::Index)
            .chain(targets.map(AssetWatch::Model))
            .map(|watch| (watch_key(source, &watch), watch));
        self.asset_subs.reconcile(wanted, |watch| match watch {
            AssetWatch::Index => assets::queries::watch_index(swr, source, tx.clone(), cx),
            AssetWatch::Model(target) => {
                assets::queries::watch_model(swr, source, target, tx.clone(), cx)
            }
        });
    }
}

/// What one asset subscription covers.
enum AssetWatch {
    /// The shared registry every model fetch reads.
    Index,
    /// One device model's download.
    Model(AssetTarget),
}

/// Identity of one subscription, and the reason the source is part of it: a
/// fetcher captures the source it was built with, so a subscription kept across
/// a source change would keep fetching from the old mirror — the Settings
/// dropdown would appear to do nothing until the process restarted. Naming the
/// source here makes a change re-key every entry, which reconciliation already
/// knows how to act on.
fn watch_key(source: AssetSourcePreference, watch: &AssetWatch) -> String {
    let source = sync::source_segment(source);
    match watch {
        AssetWatch::Index => format!("{source}/index"),
        AssetWatch::Model(target) => format!("{source}/model/{}", sync::model_key(target)),
    }
}

/// Live cache subscriptions, one per key, opened on demand and dropped once
/// their key stops being wanted.
///
/// Generic over the handle so the reconciliation can be tested without a GPUI
/// context or a real fetch: what matters is that a repeated key keeps its one
/// subscription and a vanished key has its handle *dropped*, since dropping is
/// what unsubscribes.
struct Subscriptions<H> {
    live: HashMap<String, H>,
}

impl<H> Subscriptions<H> {
    fn new() -> Self {
        Self {
            live: HashMap::new(),
        }
    }

    /// Make the live set match `wanted`: open what is missing, keep what is
    /// already there, and drop the rest.
    fn reconcile<T>(
        &mut self,
        wanted: impl Iterator<Item = (String, T)>,
        mut open: impl FnMut(T) -> H,
    ) {
        let mut keep = HashSet::new();
        for (key, item) in wanted {
            if !self.live.contains_key(&key) {
                let handle = open(item);
                self.live.insert(key.clone(), handle);
            }
            keep.insert(key);
        }
        self.live.retain(|key, _| keep.contains(key));
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
    AppState::update(cx, |state, cx| {
        if state.set_agent_link(link) {
            cx.emit(StateEvent::AgentChanged);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use openlogi_camera::Camera;

    use openlogi_core::config::AssetSourcePreference;

    use super::{AssetWatch, Subscriptions, camera_targets, watch_key};
    use crate::services::assets::sync::model_key;

    /// Stands in for a live subscription. Dropping a real one unsubscribes, so
    /// the tests below assert on drops rather than on any handle contents.
    struct DropSpy {
        key: String,
        dropped: Rc<RefCell<Vec<String>>>,
    }

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.dropped.borrow_mut().push(self.key.clone());
        }
    }

    /// Reconcile `keys`, recording every key a subscription was opened for.
    fn reconcile(
        subs: &mut Subscriptions<DropSpy>,
        keys: &[&str],
        opened: &Rc<RefCell<Vec<String>>>,
        dropped: &Rc<RefCell<Vec<String>>>,
    ) {
        let wanted = keys.iter().map(|k| ((*k).to_string(), (*k).to_string()));
        subs.reconcile(wanted, |key: String| {
            opened.borrow_mut().push(key.clone());
            DropSpy {
                key,
                dropped: Rc::clone(dropped),
            }
        });
    }

    #[test]
    fn a_repeated_target_keeps_its_one_subscription() {
        // The event loop offers the whole known set on every snapshot, so
        // re-subscribing per snapshot would refetch every model forever.
        let (opened, dropped) = (Rc::default(), Rc::default());
        let mut subs = Subscriptions::new();

        reconcile(&mut subs, &["a", "b"], &opened, &dropped);
        reconcile(&mut subs, &["a", "b"], &opened, &dropped);

        assert_eq!(opened.borrow().len(), 2, "each model subscribes once");
        assert!(dropped.borrow().is_empty(), "nothing was given up");
    }

    #[test]
    fn a_target_that_disappears_drops_its_subscription() {
        // Dropping the handle is what unsubscribes and lets the entry age out;
        // leaking it would pin every device ever seen for the session.
        let (opened, dropped) = (Rc::default(), Rc::default());
        let mut subs = Subscriptions::new();

        reconcile(&mut subs, &["a", "b"], &opened, &dropped);
        reconcile(&mut subs, &["b"], &opened, &dropped);

        assert_eq!(*dropped.borrow(), ["a"]);
        assert_eq!(opened.borrow().len(), 2, "`b` was not reopened");
    }

    #[test]
    fn a_returning_target_subscribes_again() {
        let (opened, dropped) = (Rc::default(), Rc::default());
        let mut subs = Subscriptions::new();

        reconcile(&mut subs, &["a"], &opened, &dropped);
        reconcile(&mut subs, &[], &opened, &dropped);
        reconcile(&mut subs, &["a"], &opened, &dropped);

        assert_eq!(*opened.borrow(), ["a", "a"]);
        assert_eq!(*dropped.borrow(), ["a"]);
    }

    fn camera(product_id: u16, unique_id: &str) -> Camera {
        Camera {
            name: "MX Brio".into(),
            unique_id: unique_id.into(),
            serial_number: None,
            vendor_id: 0x046d,
            product_id,
            max_resolution: None,
            max_fps: None,
        }
    }

    #[test]
    fn a_source_change_rekeys_every_subscription() {
        // A fetcher captures the source it was built with, and reconciliation
        // leaves an already-live key alone — so a subscription kept across a
        // source change would go on fetching from the old mirror, and the
        // Settings dropdown would appear to do nothing until a restart.
        // Re-keying is what turns a source change into drop-and-reopen.
        let target = camera_targets(&[camera(0x0944, "0x2031")])
            .next()
            .expect("one camera yields one target");
        let watches = [AssetWatch::Index, AssetWatch::Model(target)];

        for watch in &watches {
            assert_ne!(
                watch_key(AssetSourcePreference::OpenLogi, watch),
                watch_key(AssetSourcePreference::Cloudflare, watch),
            );
        }
    }

    #[test]
    fn a_camera_keeps_one_subscription_across_snapshots() {
        // A camera's asset key must depend only on model-level identity. Fold
        // anything per-connection into it — the OS capture id changes across a
        // port change — and every snapshot would drop and reopen the
        // subscription, refetching art the cache already holds.
        let moved_ports = [camera(0x0944, "0x2031"), camera(0x0944, "0x2042")];
        let keys: Vec<_> = camera_targets(&moved_ports)
            .map(|t| model_key(&t))
            .collect();

        assert_eq!(keys[0], keys[1]);
    }
}
