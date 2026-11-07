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
