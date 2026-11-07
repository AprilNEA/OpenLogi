//! An `openlogi://` link opens the page it names, on a cold launch as well as
//! in a running app, and never a device the link does not name.

use gpui::{App, AppContext as _, TestAppContext};
use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::{Config, ConfigFile};
use openlogi_core::hid::Dpi;
use openlogi_fixture::{CANONICAL_DEVICE_PROFILE_JSON, DeviceProfile};
use openlogi_ipc::{AgentSnapshot, InventoryHealth};

use super::Deeplinks;
use crate::app::{AppView, DetailTab, Route};
use crate::services::assets::AssetResolver;
use crate::state::tests::device_list::snapshot_candidate;
use crate::state::{AppState, ConfigPersistence, Sources};
use crate::windows::{WindowRegistry, main_window};

/// The canonical profile's Bolt mouse in slot 1, and its RGB keyboard, which
/// has no per-application profile editor.
const MOUSE: &str = "unit:4f4c4401";
const KEYBOARD: &str = "unit:4f4c4402";

fn link(url: &str) -> DeeplinkCommand {
    DeeplinkCommand::parse_url(url).expect("a valid openlogi:// URL")
}

fn device_link(key: &str) -> DeeplinkCommand {
    link(&format!("openlogi://open-device/{key}"))
}

fn profile() -> DeviceProfile {
    serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses")
}

/// The agent's snapshot while its first enumeration is still running.
fn scanning(profile: &DeviceProfile) -> AgentSnapshot {
    let mut snapshot = snapshot_candidate(profile);
    snapshot.status.inventory = InventoryHealth::Scanning;
    snapshot.inventory.clear();
    snapshot.standalone.clear();
    snapshot
}

/// Start the way the event loop does: no device list yet, the main window
/// open.
fn launch(config: Config, persistence: ConfigPersistence, cx: &mut App) {
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let resolver = AssetResolver::new();
    let state = cx.new(|_| {
        AppState::new(Sources {
            persistence,
            ..Sources::in_memory(config, &resolver, commands)
        })
    });
    AppState::set_global(state, cx);
    main_window::open(cx);
}

/// Merge one agent snapshot, then retry the waiting link, as the event loop
/// does for every snapshot.
fn deliver(snapshot: &AgentSnapshot, links: &mut Deeplinks, cx: &mut App) {
    AppState::update(cx, |state, _| {
        let _ = state.apply_agent_snapshot(snapshot, &AssetResolver::new(), &[]);
    });
    links.retry(cx);
}

/// What the main window shows: its route, and the tab a device page is on.
fn page(cx: &mut App) -> (Route, DetailTab) {
    let main = cx
        .default_global::<WindowRegistry>()
        .main
        .expect("the main window is open");
    main.update(cx, |root, _, cx| {
        let view = root
            .view()
            .clone()
            .downcast::<AppView>()
            .expect("the app view");
        let view = view.read(cx);
        (view.route.clone(), view.active_tab)
    })
    .expect("the main window is live")
}

fn device_page(key: &str) -> Route {
    Route::Device {
        record_key: key.to_owned(),
    }
}

fn selected(cx: &App) -> String {
    let state = AppState::try_read(cx).expect("state is installed");
    let record = state.current_record().expect("a device is selected");
    record.config_key.clone()
}

#[gpui::test]
fn a_device_link_opens_that_device_cold_and_warm_and_never_its_replacement(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let profile = profile();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
        // Saved under the mouse's pairing slot, before the GUI moved the
        // settings to the mouse's own identity.
        let slot = "receiver:ol-bolt-uid-0001:slot:1";
        config.devices.entry(slot.into()).or_default().dpi = Some(Dpi::new(1600));
        file.save(&config).unwrap();
        launch(config, ConfigPersistence::UserFile(file), cx);
        let mut links = Deeplinks::default();

        // Cold: macOS delivers the URL before the agent's first complete
        // inventory, so the link waits for it.
        links.dispatch(device_link(MOUSE), cx);
        assert_eq!(page(cx).0, Route::Home);
        deliver(&scanning(&profile), &mut links, cx);
        assert_eq!(
            page(cx).0,
            Route::Home,
            "a scanning agent lists nothing yet"
        );
        deliver(&snapshot_candidate(&profile), &mut links, cx);
        assert_eq!(page(cx), (device_page(MOUSE), DetailTab::Buttons));
        assert_eq!(selected(cx), MOUSE);

        // Warm: the running window moves to the next device.
        links.dispatch(device_link(KEYBOARD), cx);
        let keyboard_page = page(cx);
        assert_eq!(keyboard_page.0, device_page(KEYBOARD));
        assert_eq!(selected(cx), KEYBOARD);
        assert_eq!(cx.windows().len(), 1, "the link reuses the main window");

        // A device the complete inventory lacks is given up, so it cannot
        // pull the window away when it shows up later.
        links.dispatch(device_link("unit:0badf00d"), cx);
        let mut later = snapshot_candidate(&profile);
        let direct = &mut later.inventory[1].paired[0];
        direct.model_info.as_mut().unwrap().unit_id = [0x0b, 0xad, 0xf0, 0x0d];
        deliver(&later, &mut links, cx);
        assert_eq!(page(cx), keyboard_page);

        // Another mouse paired into the first mouse's slot does not answer to
        // the first mouse's link.
        let mut replaced = snapshot_candidate(&profile);
        let paired = &mut replaced.inventory[0].paired[0];
        paired.model_info.as_mut().unwrap().unit_id = [7; 4];
        deliver(&replaced, &mut links, cx);
        links.dispatch(device_link(MOUSE), cx);
        assert_eq!(page(cx).0, device_page(MOUSE));
        let state = AppState::try_read(cx).unwrap();
        let record = state.current_record().unwrap();
        assert_eq!(record.config_key, MOUSE);
        assert!(
            !record.is_online() && record.route.is_none(),
            "the mouse's offline page, which reaches no device, not the replacement's"
        );
    });
}

#[gpui::test]
fn an_app_profile_link_opens_the_profile_the_selected_device_runs_the_app_under(
    cx: &mut TestAppContext,
) {
    const PHOTOSHOP: &str = r"c:\program files\adobe\photoshop.exe";
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let mut config = Config::ephemeral();
        // The mouse's buttons have a profile for every install of the app; its
        // Actions Ring has none for it yet.
        config.set_per_app_binding(
            MOUSE,
            "exe:photoshop.exe",
            ButtonId::Back,
            Some(Action::Undo),
        );
        config.set_selected_device(Some(MOUSE.into()));
        launch(config, ConfigPersistence::MemoryOnly, cx);
        let mut links = Deeplinks::default();
        let profile_link = DeeplinkCommand::OpenAppProfile(PHOTOSHOP.into());

        // Cold: which device is selected is unknown until the inventory.
        links.dispatch(profile_link.clone(), cx);
        assert_eq!(page(cx).0, Route::Home);
        deliver(&snapshot_candidate(&profile()), &mut links, cx);
        assert_eq!(page(cx), (device_page(MOUSE), DetailTab::Buttons));
        let state = AppState::try_read(cx).unwrap();
        assert_eq!(state.editing_app(), Some("exe:photoshop.exe"));
        assert_eq!(state.button_bindings()[&ButtonId::Back], Action::Undo);
        assert_eq!(state.editing_action_ring_app(), Some(PHOTOSHOP));

        // A selected device with no profile editor keeps its page.
        links.dispatch(device_link(KEYBOARD), cx);
        let keyboard_page = page(cx);
        links.dispatch(profile_link, cx);
        assert_eq!(page(cx), keyboard_page);
        assert_eq!(AppState::try_read(cx).unwrap().editing_app(), None);
    });
}

#[gpui::test]
fn window_links_open_their_window_and_replace_a_waiting_device_link(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        launch(Config::ephemeral(), ConfigPersistence::MemoryOnly, cx);
        let mut links = Deeplinks::default();

        links.dispatch(device_link(MOUSE), cx);
        links.dispatch(link("openlogi://add-device"), cx);
        let add_device = cx.default_global::<WindowRegistry>().add_device;
        assert!(add_device.is_some());
        assert_eq!(cx.active_window(), add_device.map(Into::into));
        deliver(&snapshot_candidate(&profile()), &mut links, cx);
        assert_eq!(
            page(cx).0,
            Route::Home,
            "the window the user opened since replaced the waiting link"
        );

        links.dispatch(link("openlogi://open-flow"), cx);
        let flow = cx.default_global::<WindowRegistry>().flow;
        assert!(flow.is_some());
        assert_eq!(cx.active_window(), flow.map(Into::into));
        assert_eq!(cx.windows().len(), 3);
    });
}
