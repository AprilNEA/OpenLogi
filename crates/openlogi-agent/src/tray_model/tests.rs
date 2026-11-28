use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::{
    Config, ConfigRevision, FlowPeerName, FlowPublicKey, KeyModifiers, KeyTrigger,
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, ChangedOnLight, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, LightCapabilities, LightState, LightValueRange,
    LightValueUnit, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::diagnostics::ConnectionKind;
use openlogi_core::hid::DIRECT_DEVICE_INDEX;
use openlogi_ipc::{
    AgentSnapshot, AgentStatus, FlowLinkState, FlowPeerStatus, FlowStatus, ForegroundApps,
    InventoryHealth, ObservedLight, SecureInput, VirtualKeyboardStatus,
};

use super::{
    AppButtons, BatteryFill, DeviceState, Issue, LightReading, Permission, TrayMenu,
    needs_attention,
};

/// A healthy agent with no devices, no app in front, and Flow off.
fn snapshot() -> AgentSnapshot {
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: true,
            launch_at_login: true,
            inventory: InventoryHealth::Ready,
            protocol_version: 46,
            agent_version: "0.9.0".to_owned(),
            input_monitoring_granted: true,
            hid_open_failures: false,
            secure_input: SecureInput::Off,
            virtual_keyboard: VirtualKeyboardStatus::Active,
        },
        inventory: Vec::new(),
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: ForegroundApps::default(),
        lights: Vec::new(),
        flow: FlowStatus::default(),
        config_revision: ConfigRevision::default(),
    }
}

fn discharging(percentage: u8) -> BatteryInfo {
    BatteryInfo {
        percentage: Some(percentage),
        level: BatteryLevel::Good,
        status: BatteryStatus::Discharging,
    }
}

/// A device in `slot` of a Bolt receiver, identified by `unit`.
fn paired(slot: u8, codename: &str, unit: [u8; 4], online: bool) -> PairedDevice {
    PairedDevice {
        slot,
        codename: Some(codename.to_owned()),
        wpid: None,
        kind: DeviceKind::Mouse,
        online,
        battery: None,
        model_info: Some(DeviceModelInfo {
            entity_count: 1,
            serial_number: None,
            unit_id: unit,
            transports: DeviceTransports::default(),
            model_ids: [0xb034, 0, 0],
            extended_model_id: 0,
        }),
        capabilities: None,
        settings_revision: 0,
        hidpp10: false,
    }
}

/// A receiver of product `product_id` holding `paired`.
fn receiver(product_id: u16, uid: &str, paired: Vec<PairedDevice>) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Receiver".to_owned(),
            vendor_id: 0x046d,
            product_id,
            unique_id: Some(uid.to_owned()),
        },
        paired,
    }
}

const BOLT: u16 = 0xc548;
const LIGHTSPEED: u16 = 0xc539;

fn app(id: &str, name: &str) -> ForegroundApp {
    ForegroundApp {
        id: id.to_owned(),
        display_name: name.to_owned(),
    }
}

#[test]
fn the_icon_is_marked_only_for_what_the_user_must_fix_or_a_critical_battery() {
    let mark = |change: &dyn Fn(&mut AgentSnapshot)| {
        let mut snapshot = snapshot();
        change(&mut snapshot);
        needs_attention(&snapshot)
    };
    let with_battery = |battery: BatteryInfo, online: bool| {
        move |snapshot: &mut AgentSnapshot| {
            let mut device = paired(1, "MX Master 3S", [1, 2, 3, 4], online);
            device.battery = Some(battery.clone());
            snapshot.inventory = vec![receiver(BOLT, "AA", vec![device])];
        }
    };

    assert!(!mark(&|_| {}), "a healthy agent shows the plain glyph");
    assert!(mark(&|s| s.status.accessibility_granted = false));
    assert!(mark(&|s| s.status.input_monitoring_granted = false));
    assert!(mark(&|s| s.status.hid_open_failures = true));
    assert!(mark(&with_battery(discharging(10), true)));
    assert!(
        !mark(&with_battery(discharging(11), true)),
        "a low battery only tints its glyph in the menu"
    );
    assert!(
        !mark(&with_battery(discharging(5), false)),
        "an offline device's battery is only its last reading"
    );
    let charging = BatteryInfo {
        status: BatteryStatus::Charging,
        ..discharging(5)
    };
    assert!(!mark(&with_battery(charging, true)));
    assert!(
        !mark(&|s| s.status.secure_input = SecureInput::On(None)),
        "Secure Event Input ends on its own"
    );
}

#[test]
fn issues_name_the_missing_permission_before_a_failed_open_it_explains() {
    let mut snapshot = snapshot();
    snapshot.status.accessibility_granted = false;
    snapshot.status.input_monitoring_granted = false;
    snapshot.status.hid_open_failures = true;
    let menu = TrayMenu::new(&snapshot, &Config::default());
    assert_eq!(
        menu.issues,
        [
            Issue::MissingPermission(Permission::Accessibility),
            Issue::MissingPermission(Permission::InputMonitoring),
        ],
        "without Input Monitoring every open fails, and its row says why"
    );

    snapshot.status.accessibility_granted = true;
    snapshot.status.input_monitoring_granted = true;
    let menu = TrayMenu::new(&snapshot, &Config::default());
    assert_eq!(menu.issues, [Issue::DevicesUnavailable]);
}

#[test]
fn secure_input_is_an_issue_only_while_it_pauses_a_key_binding() {
    let mut snapshot = snapshot();
    snapshot.status.secure_input =
        SecureInput::On(Some(app("com.agilebits.onepassword7", "1Password")));
    let mut config = Config::default();
    assert!(
        TrayMenu::new(&snapshot, &config).issues.is_empty(),
        "nothing to pause without a key binding"
    );

    config.set_keyboard_binding(
        KeyTrigger {
            keycode: 122,
            modifiers: KeyModifiers::default(),
        },
        Some(Action::Copy),
    );
    let menu = TrayMenu::new(&snapshot, &config);
    assert_eq!(
        menu.issues,
        [Issue::SecureInput(Some("1Password".to_owned()))]
    );
    assert!(
        !needs_attention(&snapshot),
        "Secure Input ends on its own; nothing to fix"
    );
    assert!(menu.issues[0].title().contains("1Password"));
    assert_eq!(menu.issues[0].detail(), None);

    snapshot.status.hook_installed = false;
    assert!(
        TrayMenu::new(&snapshot, &config).issues.is_empty(),
        "without the hook no key binding runs to be paused"
    );
}

#[test]
fn a_device_row_takes_the_users_name_and_links_the_device_itself() {
    let mut mouse = paired(1, "MX MASTER 3S", [0x4f, 0x4c, 0x44, 0x01], true);
    mouse.battery = Some(discharging(62));
    let mut snapshot = snapshot();
    snapshot.inventory = vec![receiver(BOLT, "AA", vec![mouse])];

    let menu = TrayMenu::new(&snapshot, &Config::default());
    let row = &menu.devices[0];
    assert_eq!(row.name, "MX Master 3S", "a shouted codename is tidied");
    assert_eq!(row.state, DeviceState::Battery(discharging(62)));
    assert_eq!(
        row.settings.to_url(),
        "openlogi://open-device/unit:4f4c4401",
        "the link names the unit, not the receiver slot it sits in"
    );

    let mut config = Config::default();
    config.set_device_custom_name("unit:4f4c4401", Some("Work mouse".to_owned()));
    let menu = TrayMenu::new(&snapshot, &config);
    assert_eq!(menu.devices[0].name, "Work mouse");
}

#[test]
fn a_device_known_only_by_its_receiver_slot_opens_the_main_window() {
    // Asleep since the agent started: no identity, and no config entry
    // linking the route to one.
    let mut asleep = paired(2, "MX Keys S", [0; 4], false);
    asleep.model_info = None;
    let mut snapshot = snapshot();
    snapshot.inventory = vec![receiver(BOLT, "AA", vec![asleep])];

    let menu = TrayMenu::new(&snapshot, &Config::default());
    assert_eq!(menu.devices[0].settings, DeeplinkCommand::Show);
    assert_eq!(menu.devices[0].state, DeviceState::Offline(None));
}

/// The menu remembers readings by device, not by receiver slot alone: a
/// keyboard paired into a mouse's slot must not start from the mouse's
/// readings, while the mouse asleep is still the mouse.
#[test]
fn a_device_paired_into_another_s_slot_is_remembered_apart_from_it() {
    let routed = |device: PairedDevice| {
        let mut snapshot = snapshot();
        snapshot.inventory = vec![receiver(BOLT, "AA", vec![device])];
        TrayMenu::new(&snapshot, &Config::default()).devices[0]
            .routed()
            .expect("a receiver slot is a route")
    };
    let mouse = routed(paired(1, "MX Master 3S", [1, 2, 3, 4], true));
    let keyboard = routed(paired(1, "MX Keys S", [5, 6, 7, 8], true));

    assert_eq!(mouse.route, keyboard.route);
    assert_ne!(mouse, keyboard);
    assert_eq!(
        routed(paired(1, "MX Master 3S", [1, 2, 3, 4], false)),
        mouse
    );
}

#[test]
fn an_offline_device_keeps_its_last_battery_for_the_submenu_only() {
    let mut mouse = paired(1, "MX Master 3S", [1, 2, 3, 4], false);
    mouse.battery = Some(discharging(40));
    let mut snapshot = snapshot();
    snapshot.inventory = vec![receiver(BOLT, "AA", vec![mouse])];

    let row = &TrayMenu::new(&snapshot, &Config::default()).devices[0];
    assert_eq!(row.state, DeviceState::Offline(Some(discharging(40))));
    assert_eq!(
        row.detail(),
        Some(super::Detail::Text("Offline".to_owned())),
        "the row says offline, not a charge it may no longer have"
    );
    assert_eq!(
        row.info(),
        [
            "Offline",
            "Last known battery: 40%",
            "Bolt receiver · Slot 1",
        ]
    );
}

#[test]
fn the_link_names_the_receiver_family_and_slot() {
    let mut snapshot = snapshot();
    snapshot.inventory = vec![
        receiver(
            BOLT,
            "AA",
            vec![paired(3, "MX Master 3S", [1, 0, 0, 0], true)],
        ),
        receiver(
            LIGHTSPEED,
            "BB",
            vec![paired(1, "G502 X", [2, 0, 0, 0], true)],
        ),
    ];
    let menu = TrayMenu::new(&snapshot, &Config::default());
    let links: Vec<_> = menu
        .devices
        .iter()
        .map(|row| (row.name.as_str(), row.link.kind, row.link.text()))
        .collect();
    assert_eq!(
        links,
        [
            (
                "MX Master 3S",
                ConnectionKind::BoltReceiver,
                "Bolt receiver · Slot 3".to_owned()
            ),
            // A Lightspeed receiver speaks the Unifying protocol; its row
            // still names what it is.
            (
                "G502 X",
                ConnectionKind::LightspeedReceiver,
                "Lightspeed receiver · Slot 1".to_owned()
            ),
        ]
    );
}

#[test]
fn devices_follow_the_canonical_order_whatever_order_they_were_enumerated_in() {
    let direct = DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Anywhere 3S".to_owned(),
            vendor_id: 0x046d,
            product_id: 0xb037,
            unique_id: None,
        },
        paired: vec![paired(
            DIRECT_DEVICE_INDEX,
            "MX Anywhere 3S",
            [9, 9, 9, 9],
            true,
        )],
    };
    let bolt = receiver(
        BOLT,
        "AA",
        vec![
            paired(2, "MX Keys S", [2, 0, 0, 0], true),
            paired(1, "MX Master 3S", [1, 0, 0, 0], true),
        ],
    );
    for inventory in [vec![direct.clone(), bolt.clone()], vec![bolt, direct]] {
        let mut snapshot = snapshot();
        snapshot.inventory = inventory;
        let names: Vec<_> = TrayMenu::new(&snapshot, &Config::default())
            .devices
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(names, ["MX Master 3S", "MX Keys S", "MX Anywhere 3S"]);
    }
}

#[test]
fn a_light_row_reports_what_the_light_read_back() {
    let address = RawDeviceAddress {
        vendor_id: 0x046d,
        product_id: 0xc900,
        usage_page: 0xff43,
        usage_id: 0x0202,
        identity: "serial:litra01".to_owned(),
    };
    let light = StandaloneDevice {
        address: address.clone(),
        display_name: "Litra Glow".to_owned(),
        manufacturer: None,
        serial_number: Some("LITRA01".to_owned()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: None,
        driver_id: "litra".to_owned(),
        registry_model_id: None,
    };
    let brightness = LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("range");
    let state = |on| LightState {
        capabilities: LightCapabilities {
            power: true,
            brightness: Some(brightness),
            temperature: None,
            color: false,
            zones: false,
        },
        on,
        brightness: Some(193),
        temperature_kelvin: Some(4800),
        brightness_limit: None,
    };
    let observed = |on| ObservedLight {
        route: light.route(),
        state: state(on),
        changed_on_light: ChangedOnLight::default(),
    };
    let mut snapshot = snapshot();
    snapshot.standalone = vec![light.clone()];

    let row =
        |snapshot: &AgentSnapshot| TrayMenu::new(snapshot, &Config::default()).devices[0].clone();
    assert_eq!(
        row(&snapshot).state,
        DeviceState::Online,
        "a light that reads nothing back has no state to report"
    );

    snapshot.lights = vec![observed(true)];
    let lit = row(&snapshot);
    assert_eq!(
        lit.state,
        DeviceState::Light(LightReading {
            on: true,
            // 193 lm on a 20–250 lm range, not 193 %.
            brightness_percent: Some(75),
            temperature_kelvin: Some(4800),
        })
    );
    assert_eq!(
        lit.detail(),
        Some(super::Detail::Text("On · 75%".to_owned()))
    );
    assert_eq!(lit.link.kind, ConnectionKind::Wired);

    snapshot.lights = vec![observed(false)];
    assert_eq!(
        row(&snapshot).detail(),
        Some(super::Detail::Text("Off".to_owned()))
    );
}

#[test]
fn the_app_row_counts_the_buttons_its_profile_overrides() {
    let mut snapshot = snapshot();
    snapshot.foreground = ForegroundApps {
        current: Some(app("com.apple.Safari", "Safari")),
        recent: vec![
            app("com.apple.Safari", "Safari"),
            app("com.apple.dt.Xcode", "Xcode"),
        ],
    };
    let mut config = Config::default();
    let row = TrayMenu::new(&snapshot, &config)
        .app
        .expect("an app is in front");
    assert_eq!(row.current.buttons, AppButtons::Default);
    assert_eq!(row.current.buttons.text(), "Default Buttons");

    for (device, button) in [
        ("unit:01020304", ButtonId::Back),
        ("unit:01020304", ButtonId::Forward),
        ("unit:05060708", ButtonId::Back),
    ] {
        config.set_per_app_binding(device, "com.apple.Safari", button, Some(Action::Undo));
    }
    let row = TrayMenu::new(&snapshot, &config)
        .app
        .expect("an app is in front");
    assert_eq!(row.current.name, "Safari");
    assert_eq!(
        row.current.buttons,
        AppButtons::Own(3),
        "counted over every device"
    );
    assert_eq!(row.current.buttons.text(), "Own Buttons");
    assert_eq!(row.summary(), "Buttons of its own: 3");
    assert_eq!(
        row.current.profile,
        DeeplinkCommand::OpenAppProfile("com.apple.Safari".to_owned())
    );
    assert_eq!(
        row.recent
            .iter()
            .map(|app| (app.name.as_str(), app.buttons))
            .collect::<Vec<_>>(),
        [("Xcode", AppButtons::Default)],
        "the current app is not repeated among the recent ones"
    );
}

#[test]
fn while_openlogi_is_in_front_the_app_row_is_the_one_the_user_came_from() {
    let mut snapshot = snapshot();
    snapshot.foreground = ForegroundApps {
        current: Some(app(openlogi_core::brand::APP_ID, "OpenLogi")),
        recent: vec![app("com.apple.Safari", "Safari")],
    };
    let row = TrayMenu::new(&snapshot, &Config::default())
        .app
        .expect("an app to show");
    assert_eq!(row.current.name, "Safari");

    snapshot.foreground.current = None;
    assert_eq!(
        TrayMenu::new(&snapshot, &Config::default()).app,
        None,
        "nothing is in front, or the platform cannot say"
    );
}

#[test]
fn the_flow_row_shows_only_while_flow_is_on_and_names_the_connected_computer() {
    let peer = |name: &str, state| FlowPeerStatus {
        name: FlowPeerName::new(name).expect("a valid name"),
        public_key: FlowPublicKey::from_bytes([0; 32]),
        state,
    };
    let mut snapshot = snapshot();
    snapshot.flow.peers = vec![
        peer("Mac mini", FlowLinkState::Lost),
        peer("MacBook Air", FlowLinkState::Connected),
    ];
    assert_eq!(TrayMenu::new(&snapshot, &Config::default()).flow, None);

    snapshot.flow.enabled = true;
    let flow = TrayMenu::new(&snapshot, &Config::default())
        .flow
        .expect("Flow is on");
    assert_eq!(flow.detail(), "MacBook Air");
    assert_eq!(flow.peers[0].state_text(), "Offline");

    snapshot.flow.peers = vec![peer("Mac mini", FlowLinkState::Lost)];
    let flow = TrayMenu::new(&snapshot, &Config::default())
        .flow
        .expect("Flow is on");
    assert_eq!(flow.detail(), "Not Connected");

    snapshot.flow.peers.clear();
    let flow = TrayMenu::new(&snapshot, &Config::default())
        .flow
        .expect("Flow is on");
    assert_eq!(flow.detail(), "No other computers");
}

#[test]
fn the_battery_glyph_fills_to_the_nearest_quarter() {
    let fill = |percentage| BatteryFill::of(&discharging(percentage));
    assert_eq!(fill(0), BatteryFill::Empty);
    assert_eq!(fill(12), BatteryFill::Empty);
    assert_eq!(fill(13), BatteryFill::Quarter);
    assert_eq!(fill(62), BatteryFill::Half);
    assert_eq!(fill(63), BatteryFill::ThreeQuarters);
    assert_eq!(fill(88), BatteryFill::Full);
    let level = |level| {
        BatteryFill::of(&BatteryInfo {
            percentage: None,
            level,
            status: BatteryStatus::Discharging,
        })
    };
    assert_eq!(level(BatteryLevel::Critical), BatteryFill::Empty);
    assert_eq!(level(BatteryLevel::Full), BatteryFill::Full);
}

mod controls {
    //! What a device's submenu offers to change, and when it greys out.

    use openlogi_agent_core::orchestrator::SettingError;
    use openlogi_core::config::{
        Config, ConfigError, DeviceEditError, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SmartShift,
        WheelMode,
    };
    use openlogi_core::device::{
        Capabilities, ChangedOnLight, DeviceKind, LightCapabilities, LightState, LightValueRange,
        LightValueUnit, PairedDevice, RawDeviceAddress, StandaloneDevice,
    };
    use openlogi_core::device_order::{DeviceStableId, PhysicalDeviceKey};
    use openlogi_core::hid::{
        BacklightChange, BacklightControls, BacklightEffect, BacklightEffects, BacklightMode,
        BacklightState, BacklightStatus, DeviceRoute, DisableKeysMask, DisableKeysState, Dpi,
        LightCommand, SidetoneLevel, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus,
        SmartShiftThreshold, TunableTorque, WriteError,
    };
    use openlogi_ipc::{AgentSnapshot, ObservedLight};

    use super::super::{
        CameraRow, Change, DeviceMemory, DeviceRow, Entry, Failure, Read, Reading, Reads,
        SeenCamera, TrayMenu,
    };
    use super::{BOLT, paired, receiver, snapshot};

    const MOUSE_KEY: &str = "unit:4f4c4401";
    const UNIT: [u8; 4] = [0x4f, 0x4c, 0x44, 0x01];

    /// A device of `kind` with `capabilities` in slot 1 of a Bolt receiver.
    fn bolt_device(kind: DeviceKind, capabilities: Capabilities, online: bool) -> PairedDevice {
        let mut device = paired(1, "MX Master 3S", UNIT, online);
        device.kind = kind;
        device.capabilities = Some(capabilities);
        device
    }

    fn mouse(online: bool) -> PairedDevice {
        let capabilities = Capabilities {
            buttons: true,
            pointer: true,
            ..Capabilities::default()
        };
        bolt_device(DeviceKind::Mouse, capabilities, online)
    }

    fn row(device: PairedDevice, config: &Config) -> DeviceRow {
        let mut snapshot = snapshot();
        snapshot.inventory = vec![receiver(BOLT, "AA", vec![device])];
        TrayMenu::new(&snapshot, config).devices[0].clone()
    }

    /// The key the agent writes a device's settings under on the link of
    /// [`row`] — the route an offline device resolves through.
    fn route_key(device: &PairedDevice) -> String {
        let inventory = receiver(BOLT, "AA", vec![device.clone()]);
        let route = DeviceRoute::for_slot(&inventory, device.slot);
        DeviceStableId::from_parts(route.as_ref(), device.slot, None, UNIT).route_key()
    }

    /// Each choice as (title, checked, what clicking it changes).
    fn choices(entries: &[Entry]) -> Vec<(String, bool, Option<Change>)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Choice {
                    title,
                    checked,
                    change,
                } => Some((title.clone(), *checked, *change)),
                _ => None,
            })
            .collect()
    }

    fn choice(entries: &[Entry], title: &str) -> (bool, Option<Change>) {
        choices(entries)
            .into_iter()
            .find(|(t, _, _)| t == title)
            .map_or_else(
                || panic!("no {title:?} in {entries:#?}"),
                |(_, checked, change)| (checked, change),
            )
    }

    fn notes(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Note(note) => Some(note.as_str()),
                _ => None,
            })
            .collect()
    }

    fn headers(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Header(header) => Some(header.as_str()),
                _ => None,
            })
            .collect()
    }

    fn dpi(value: u16) -> Dpi {
        Dpi::new(value)
    }

    fn threshold(value: u8) -> SmartShiftThreshold {
        SmartShiftThreshold::try_new(value).expect("a threshold")
    }

    #[test]
    fn an_online_mouse_checks_the_dpi_it_reads_back_and_the_saved_one_until_then() {
        let mut config = Config::default();
        config.set_dpi_presets(MOUSE_KEY, vec![dpi(800), dpi(1600), dpi(3200)]);
        config.set_dpi(MOUSE_KEY, dpi(1600));
        let row = row(mouse(true), &config);
        assert!(row.reads().dpi && row.reads().wheel);

        let mut memory = DeviceMemory::default();
        memory.start_reading();
        let entries = row.entries(&memory);
        assert_eq!(notes(&entries), ["Reading…"]);
        assert_eq!(
            choices(&entries)[..3],
            [
                ("800 DPI".to_owned(), false, Some(Change::Dpi(dpi(800)))),
                ("1600 DPI".to_owned(), true, Some(Change::Dpi(dpi(1600)))),
                ("3200 DPI".to_owned(), false, Some(Change::Dpi(dpi(3200)))),
            ]
        );
        assert_eq!(
            choice(&entries, "Managed by OpenLogi"),
            (true, Some(Change::Managed(false)))
        );
        assert!(
            !headers(&entries).contains(&"Wheel mode"),
            "a wheel neither saved nor read may not exist"
        );

        // The mouse's own DPI button moved it on from what the config holds.
        memory.finish_reading(Reads::default(), super::super::Reading::default());
        memory.reading.dpi = Read::Value(dpi(3200));
        let entries = row.entries(&memory);
        assert!(choice(&entries, "3200 DPI").0);
        assert!(!choice(&entries, "1600 DPI").0);
        assert!(notes(&entries).is_empty());

        memory.reading.dpi = Read::Value(dpi(1000));
        let entries = row.entries(&memory);
        assert!(
            choices(&entries)
                .iter()
                .take(3)
                .all(|(_, checked, _)| !checked)
        );
        assert_eq!(notes(&entries), ["Current: 1000 DPI"]);
    }

    #[test]
    fn a_mouse_without_presets_shows_its_dpi_and_the_way_to_set_presets() {
        let row = row(mouse(true), &Config::default());
        let mut memory = DeviceMemory::default();
        memory.reading.dpi = Read::Value(dpi(1200));
        let entries = row.entries(&memory);
        assert_eq!(headers(&entries)[0], "DPI presets");
        assert!(entries.contains(&Entry::Note("Current: 1200 DPI".to_owned())));
        assert!(entries.contains(&Entry::Open {
            title: "Set Presets…".to_owned(),
            link: row.settings.clone(),
        }));
        assert!(
            !choices(&entries)
                .iter()
                .any(|(title, ..)| title.ends_with("DPI")),
            "no DPI to pick from"
        );
    }

    #[test]
    fn an_offline_mouse_changes_the_dpi_and_wheel_it_gets_at_reconnect() {
        let device = mouse(false);
        let mut config = Config::default();
        let key = PhysicalDeviceKey::parse(MOUSE_KEY).expect("a key");
        // Asleep, the mouse reads no identity: its route finds its settings.
        config.adopt_route(&key, &route_key(&device), None);
        config.set_dpi_presets(MOUSE_KEY, vec![dpi(800), dpi(1600)]);
        config.set_dpi(MOUSE_KEY, dpi(1600));
        config.set_smartshift(
            MOUSE_KEY,
            SmartShift {
                mode: WheelMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Threshold(threshold(20)),
                tunable_torque: None,
            },
        );
        let row = row(device, &config);
        assert!(!row.reads().any(), "an offline device answers no read");

        // What it read before it went to sleep is not what it gets back.
        let mut memory = DeviceMemory::default();
        memory.reading.dpi = Read::Value(dpi(800));
        let entries = row.entries(&memory);
        assert_eq!(
            notes(&entries),
            ["Changes are saved now and apply when it reconnects."]
        );
        assert_eq!(
            choice(&entries, "1600 DPI"),
            (true, Some(Change::Dpi(dpi(1600))))
        );
        assert!(!choice(&entries, "800 DPI").0);
        assert!(choice(&entries, "SmartShift").0);
        assert_eq!(
            choice(&entries, "Free spin").1,
            Some(Change::Wheel(SmartShiftStatus {
                mode: SmartShiftMode::Free,
                auto_disengage: SmartShiftAutoDisengage::Threshold(threshold(20)),
                tunable_torque: None,
            }))
        );
        assert!(choice(&entries, "Managed by OpenLogi").1.is_some());
    }

    #[test]
    fn the_wheel_choices_keep_the_torque_and_a_speed_smartshift_can_save() {
        let row = row(mouse(true), &Config::default());
        let torque = TunableTorque::try_new(70).expect("a torque");
        let wheel = |mode, auto_disengage| SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque: Some(torque),
        };
        let entries_for = |status| {
            let mut memory = DeviceMemory::default();
            memory.reading.wheel = status;
            row.entries(&memory)
        };

        let permanent = wheel(SmartShiftMode::Ratchet, SmartShiftAutoDisengage::Permanent);
        let entries = entries_for(Read::Value(permanent));
        assert!(choice(&entries, "Ratchet").0);
        assert!(!choice(&entries, "SmartShift").0);
        assert_eq!(
            choice(&entries, "SmartShift").1,
            Some(Change::Wheel(wheel(
                SmartShiftMode::Ratchet,
                SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
            ))),
            "a permanent ratchet has no speed to release at"
        );

        let free = wheel(
            SmartShiftMode::Free,
            SmartShiftAutoDisengage::Threshold(threshold(30)),
        );
        let entries = entries_for(Read::Value(free));
        assert!(choice(&entries, "Free spin").0);
        assert_eq!(
            choice(&entries, "SmartShift").1,
            Some(Change::Wheel(wheel(
                SmartShiftMode::Ratchet,
                SmartShiftAutoDisengage::Threshold(threshold(30)),
            )))
        );
        assert_eq!(
            choice(&entries, "Ratchet").1,
            Some(Change::Wheel(permanent))
        );

        // Below the floor a config can hold, which would refuse the save.
        let twitchy = wheel(
            SmartShiftMode::Ratchet,
            SmartShiftAutoDisengage::Threshold(threshold(4)),
        );
        let entries = entries_for(Read::Value(twitchy));
        assert!(choice(&entries, "SmartShift").0);
        assert_eq!(
            choice(&entries, "Free spin").1,
            Some(Change::Wheel(wheel(
                SmartShiftMode::Free,
                SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
            )))
        );

        let unsupported = Read::of(Err::<SmartShiftStatus, _>(WriteError::FeatureUnsupported {
            feature_hex: 0x2111,
        }));
        assert!(!headers(&entries_for(unsupported)).contains(&"Wheel mode"));
    }

    fn keyboard(online: bool) -> PairedDevice {
        let capabilities = Capabilities {
            fn_lock: true,
            backlight: true,
            disable_keys: true,
            ..Capabilities::default()
        };
        bolt_device(DeviceKind::Keyboard, capabilities, online)
    }

    fn backlight(enabled: bool, mode: BacklightMode) -> BacklightState {
        BacklightState {
            enabled,
            mode,
            status: BacklightStatus::PermanentManual,
            current_level: 3,
            nb_levels: 8,
            power_save: false,
            controls: BacklightControls::AUTOMATIC | BacklightControls::LEVEL,
            duration: None,
            effect: BacklightEffect::Static,
            effects: BacklightEffects::default(),
        }
    }

    #[test]
    fn a_keyboard_greys_out_what_it_has_not_read_and_toggles_what_it_has() {
        let mut config = Config::default();
        config.set_fn_lock(MOUSE_KEY, true);
        let row = row(keyboard(true), &config);
        let reads = row.reads();
        assert_eq!(
            reads,
            Reads {
                fn_lock: true,
                backlight: true,
                disabled_keys: true,
                ..Reads::default()
            },
            "a keyboard reads what it has, and nothing of a mouse's"
        );

        let mut memory = DeviceMemory::default();
        memory.start_reading();
        let entries = row.entries(&memory);
        assert_eq!(notes(&entries), ["Reading…"]);
        assert_eq!(
            choice(&entries, "Fn Lock"),
            (true, None),
            "the saved Fn lock"
        );
        assert_eq!(choice(&entries, "Backlight").1, None);
        assert_eq!(choice(&entries, "Automatic brightness").1, None);
        assert!(
            !headers(&entries).contains(&"Disabled keys"),
            "which keys it can disable is the keyboard's to say"
        );

        // Caps Lock and an unknown key are disabled; the keyboard also
        // supports Insert, and nothing else this version knows.
        let unknown = DisableKeysMask::from_bits_retain(0x80);
        let fresh = super::super::Reading {
            fn_lock: Read::Value(false),
            backlight: Read::Value(backlight(true, BacklightMode::PermanentManual)),
            disabled_keys: Read::Value(DisableKeysState {
                supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::INSERT | unknown,
                disabled: DisableKeysMask::CAPS_LOCK | unknown,
            }),
            // Not asked for: a stale value the merge must keep.
            dpi: Read::Failed,
            ..Default::default()
        };
        memory.reading.dpi = Read::Value(dpi(800));
        memory.finish_reading(reads, fresh);
        assert_eq!(memory.reading.dpi, Read::Value(dpi(800)));
        let entries = row.entries(&memory);
        assert!(notes(&entries).is_empty());
        assert_eq!(
            choice(&entries, "Fn Lock"),
            (false, Some(Change::FnLock(true)))
        );
        assert_eq!(
            choice(&entries, "Backlight"),
            (
                true,
                Some(Change::Backlight(BacklightChange::Enabled(false)))
            )
        );
        assert_eq!(
            choice(&entries, "Automatic brightness"),
            (
                false,
                Some(Change::Backlight(BacklightChange::Automatic(true)))
            )
        );
        let keys: Vec<_> = choices(&entries)
            .into_iter()
            .filter(|(title, ..)| title == "Caps Lock" || title == "Insert" || title == "Num Lock")
            .collect();
        assert_eq!(
            keys,
            [
                (
                    "Caps Lock".to_owned(),
                    true,
                    Some(Change::DisabledKeys(unknown))
                ),
                (
                    "Insert".to_owned(),
                    false,
                    Some(Change::DisabledKeys(
                        DisableKeysMask::CAPS_LOCK | DisableKeysMask::INSERT | unknown
                    ))
                ),
            ],
            "only the keys it supports, each toggled alone"
        );
    }

    /// A keyboard that answered: Fn lock off, its backlight on and held, and
    /// Caps Lock and an unknown key disabled, with Insert disableable too.
    fn read_keyboard() -> (DeviceRow, DeviceMemory, DisableKeysMask) {
        let row = row(keyboard(true), &Config::default());
        let unknown = DisableKeysMask::from_bits_retain(0x80);
        let mut memory = DeviceMemory::default();
        memory.reading.fn_lock = Read::Value(false);
        memory.reading.backlight = Read::Value(backlight(true, BacklightMode::PermanentManual));
        memory.reading.disabled_keys = Read::Value(DisableKeysState {
            supported: DisableKeysMask::CAPS_LOCK | DisableKeysMask::INSERT | unknown,
            disabled: DisableKeysMask::CAPS_LOCK | unknown,
        });
        (row, memory, unknown)
    }

    #[test]
    fn a_keyboard_change_shows_at_once_and_greys_out_offline() {
        let (row, mut memory, unknown) = read_keyboard();
        // A change made from the menu shows at the next opening, before the
        // keyboard is read again.
        let mut changed = memory;
        changed.reading.record(Change::FnLock(true));
        changed
            .reading
            .record(Change::DisabledKeys(DisableKeysMask::INSERT | unknown));
        changed
            .reading
            .record(Change::Backlight(BacklightChange::Enabled(false)));
        let entries = row.entries(&changed);
        assert!(choice(&entries, "Fn Lock").0);
        assert!(!choice(&entries, "Caps Lock").0);
        assert!(choice(&entries, "Insert").0);
        assert!(!choice(&entries, "Backlight").0);

        // A dark backlight has no brightness to automate.
        memory.reading.backlight = Read::Value(backlight(false, BacklightMode::Automatic));
        let entries = row.entries(&memory);
        assert_eq!(choice(&entries, "Automatic brightness"), (false, None));

        // Offline, what it last read still shows, greyed out.
        let offline = super::super::DeviceRow {
            state: super::super::DeviceState::Offline(None),
            ..row.clone()
        };
        let entries = offline.entries(&memory);
        assert!(
            choices(&entries)
                .iter()
                .filter(|(title, ..)| title != "Managed by OpenLogi")
                .all(|(.., change)| change.is_none())
        );
        assert_eq!(
            choice(&entries, "Managed by OpenLogi").1,
            Some(Change::Managed(false)),
            "saved, so it changes offline"
        );
        assert_eq!(
            notes(&entries),
            ["Changes are saved now and apply when it reconnects."]
        );
    }

    #[test]
    fn a_read_under_way_does_not_undo_a_change_made_meanwhile() {
        let dpi_and_wheel = Reads {
            dpi: true,
            wheel: true,
            ..Reads::default()
        };
        let ratchet = SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Permanent,
            tunable_torque: None,
        };
        let answered = |dpi_value| Reading {
            dpi: Read::Value(dpi(dpi_value)),
            wheel: Read::Value(ratchet),
            ..Reading::default()
        };
        let mut memory = DeviceMemory::default();

        // An answer with no read under way is not taken.
        memory.finish_reading(dpi_and_wheel, answered(2400));
        assert_eq!(memory.reading, Reading::default());

        // The read reached the mouse before the change did.
        assert!(memory.start_reading());
        assert!(!memory.start_reading(), "one read at a time");
        memory.changed(Ok(Some(Change::Dpi(dpi(1600)))));
        memory.finish_reading(dpi_and_wheel, answered(800));
        assert_eq!(memory.reading.dpi, Read::Value(dpi(1600)));
        assert_eq!(memory.reading.wheel, Read::Value(ratchet));

        // The next read, begun after the change landed, is the device's word.
        memory.start_reading();
        memory.finish_reading(dpi_and_wheel, answered(2400));
        assert_eq!(memory.reading.dpi, Read::Value(dpi(2400)));

        // So is a read that begins after a change made while none ran.
        memory.changed(Ok(Some(Change::Dpi(dpi(1600)))));
        memory.start_reading();
        memory.finish_reading(dpi_and_wheel, answered(800));
        assert_eq!(memory.reading.dpi, Read::Value(dpi(800)));

        // A change a newer write superseded records nothing and is no failure.
        memory.failure = Some(Failure::Device);
        memory.changed(Ok(None));
        assert_eq!(memory.reading.dpi, Read::Value(dpi(800)));
        assert_eq!(memory.failure, None);
    }

    #[test]
    fn a_headset_offers_five_sidetone_levels_while_it_answers() {
        let capabilities = Capabilities {
            sidetone: true,
            ..Capabilities::default()
        };
        let headset = bolt_device(DeviceKind::Headset, capabilities, true);
        let row = row(headset, &Config::default());
        let mut memory = DeviceMemory::default();
        memory.reading.sidetone = Read::Value(SidetoneLevel::clamped(50));
        let entries = row.entries(&memory);
        let level = |percent| Some(Change::Sidetone(SidetoneLevel::clamped(percent)));
        assert_eq!(
            choices(&entries),
            [
                ("Off".to_owned(), false, level(0)),
                ("25%".to_owned(), false, level(25)),
                ("50%".to_owned(), true, level(50)),
                ("75%".to_owned(), false, level(75)),
                ("100%".to_owned(), false, level(100)),
            ],
            "a headset is not managed, so sidetone is all it offers"
        );

        memory.reading.sidetone = Read::Failed;
        let entries = row.entries(&memory);
        assert_eq!(notes(&entries), ["Couldn't read the device."]);
        assert!(
            choices(&entries)
                .iter()
                .all(|(.., change)| change.is_none())
        );
    }

    /// A Litra Glow, plugged in.
    fn litra() -> StandaloneDevice {
        StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:litra01".to_owned(),
            },
            display_name: "Litra Glow".to_owned(),
            manufacturer: None,
            serial_number: Some("LITRA01".to_owned()),
            unit_id: [0; 4],
            kind: DeviceKind::Light,
            online: true,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".to_owned(),
            registry_model_id: None,
        }
    }

    #[test]
    fn a_light_offers_power_and_the_presets_its_range_can_show() {
        let light = litra();
        let capabilities = LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("a range"),
            ),
            // A light no warmer than 5000 K: warm and neutral land on one stop.
            temperature: Some(
                LightValueRange::new(5000, 6500, 100, LightValueUnit::Kelvin).expect("a range"),
            ),
            color: false,
            zones: false,
        };
        let observed = |on| ObservedLight {
            route: light.route(),
            state: LightState {
                capabilities,
                on,
                brightness: Some(193),
                temperature_kelvin: Some(5000),
                brightness_limit: None,
            },
            changed_on_light: ChangedOnLight::default(),
        };
        let row_with = |light: &StandaloneDevice, on| {
            let mut snapshot: AgentSnapshot = snapshot();
            snapshot.standalone = vec![light.clone()];
            snapshot.lights = vec![observed(on)];
            TrayMenu::new(&snapshot, &Config::default()).devices[0].clone()
        };
        let command = |command| Some(Change::Light(command));

        let lit = row_with(&light, true).entries(&DeviceMemory::default());
        let item = |title: &str, checked, command| {
            (title.to_owned(), checked, Some(Change::Light(command)))
        };
        assert_eq!(
            choices(&lit),
            [
                item("Light On", true, LightCommand::Power(false)),
                item("25%", false, LightCommand::BrightnessPercent(25)),
                item("50%", false, LightCommand::BrightnessPercent(50)),
                item("75%", true, LightCommand::BrightnessPercent(75)),
                item("100%", false, LightCommand::BrightnessPercent(100)),
                item("Cool 6500 K", false, LightCommand::TemperatureKelvin(6500)),
                item(
                    "Neutral 5000 K",
                    true,
                    LightCommand::TemperatureKelvin(5000)
                ),
            ]
        );

        let dark = row_with(&light, false).entries(&DeviceMemory::default());
        assert_eq!(
            choice(&dark, "Light On"),
            (false, command(LightCommand::Power(true)))
        );
        assert!(
            choices(&dark)
                .iter()
                .skip(1)
                .all(|(.., change)| change.is_none()),
            "a dark light has no brightness or colour to change"
        );
        assert_eq!(
            choice(&dark, "75%"),
            (true, None),
            "what it comes back on at"
        );

        let unplugged = StandaloneDevice {
            online: false,
            light_capabilities: Some(capabilities),
            ..light.clone()
        };
        let entries = row_with(&unplugged, true).entries(&DeviceMemory::default());
        assert_eq!(
            choices(&entries).len(),
            7,
            "the light's controls, greyed out"
        );
        assert!(
            choices(&entries)
                .iter()
                .all(|(.., change)| change.is_none())
        );
        assert!(!row_with(&unplugged, true).reads().any());
    }

    #[test]
    fn a_change_that_did_not_take_says_why_once() {
        let conflict = SettingError::NotSaved(ConfigError::Conflict {
            path: openlogi_core::paths::CONFIG_FILE.into(),
        });
        assert_eq!(Failure::of(&conflict), Failure::Conflict);
        assert_eq!(
            Failure::of(&SettingError::Refused(DeviceEditError::LinkOverride)),
            Failure::LinkOverride
        );
        assert_eq!(
            Failure::of(&SettingError::Write(WriteError::DeviceNotFound)),
            Failure::Device
        );
        assert_eq!(
            Failure::of(&SettingError::Refused(DeviceEditError::NoConfigKey)),
            Failure::NotSaved
        );

        let row = row(mouse(true), &Config::default());
        let mut memory = DeviceMemory::default();
        memory.failure = Some(Failure::LinkOverride);
        assert_eq!(
            notes(&row.entries(&memory))[0],
            "Not saved: this connection has its own value. Change it in OpenLogi."
        );
    }

    #[test]
    fn cameras_list_by_name_and_open_their_preview_or_their_settings() {
        let mut config = Config::default();
        config.set_device_custom_name("camera:046d:0893", Some("Desk cam".to_owned()));
        let cameras = [
            SeenCamera {
                name: "StreamCam".to_owned(),
                key: "camera:046d:0893".to_owned(),
                in_use: true,
            },
            SeenCamera {
                name: "C920".to_owned(),
                key: "camera:046d:082d:serial:ab12".to_owned(),
                in_use: false,
            },
        ];
        let rows = CameraRow::all(&cameras, &config);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.state_text()))
                .collect::<Vec<_>>(),
            [
                ("C920", "Not in Use".to_owned()),
                ("Desk cam", "In Use".to_owned())
            ]
        );
        assert_eq!(
            rows[0].preview.to_url(),
            "openlogi://preview-camera/camera:046d:082d:serial:ab12"
        );
        assert_eq!(
            rows[1].settings.to_url(),
            "openlogi://open-camera/camera:046d:0893"
        );
    }
}
