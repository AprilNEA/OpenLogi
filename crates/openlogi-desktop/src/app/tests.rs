use super::home::{connection_icon_path, ordered_device_indices};
use super::{Capabilities, DetailTab, DeviceKind, DeviceRecord};
use crate::services::assets::ResolvedAsset;
use crate::ui::battery::{battery_charging_no_reading, battery_needs_attention};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceModelInfo, DeviceTransports, LightCapabilities,
    LightValueRange, LightValueUnit,
};
use openlogi_core::hid::DeviceRoute;

#[gpui::test]
fn config_error_folder_button_opens_from_the_active_window(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, Modifiers};
    use gpui_component::Root;

    use super::{AppView, menu};
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::{AppState, ConfigPersistence, Sources};

    let _locale = LOCALE_LOCK.lock().unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources {
                persistence: ConfigPersistence::ReadOnly("invalid config".into()),
                ..Sources::in_memory(
                    openlogi_core::config::Config::ephemeral(),
                    &resolver,
                    commands,
                )
            })
        });
        AppState::set_global(state, cx);
        menu::install(cx);
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| AppView::new(window, cx));
        Root::new(view, window, cx)
    });
    visual.update(|window, cx| {
        window.activate_window();
        window.draw(cx).clear(cx);
        assert_eq!(cx.active_window(), Some(window.window_handle()));
    });
    let button = visual.debug_bounds("open-config-folder").unwrap();
    assert_eq!(visual.opened_url(), None);

    visual.simulate_click(button.center(), Modifiers::default());

    let path = openlogi_core::paths::config_dir().unwrap();
    let expected_url = menu::file_url(&path).unwrap();
    assert_eq!(
        visual.opened_url(),
        Some(expected_url),
        "the config-error button must open the folder while its window handles the click"
    );
}

#[gpui::test]
fn main_window_renders_profile_confirmation_dialogs(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, InteractiveElement as _, ParentElement as _, div};
    use gpui_component::{Root, WindowExt as _};

    use super::AppView;
    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    cx.update(gpui_component::init);
    cx.update(|cx| {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources::in_memory(
                openlogi_core::config::Config::ephemeral(),
                &resolver,
                commands,
            ))
        });
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| AppView::new(window, cx));
        Root::new(view, window, cx)
    });
    cx.update(|window, cx| {
        window.open_alert_dialog(cx, |alert, _, _| {
            alert.confirm().title(
                div()
                    .debug_selector(|| "profile-confirmation".into())
                    .child("Remove Safari profile?"),
            )
        });
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("profile-confirmation").is_some(),
        "the production AppView must paint the dialog, even before the agent connects"
    );
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("profile-confirmation").is_none());
}

/// "Charging" replaces the bogus percentage only when charging *and* the
/// reading is still 0% (cold start, no cached pre-charge value). A non-zero
/// charge or a real 0% while discharging keeps the number.
#[test]
fn charging_without_reading_suppresses_percentage() {
    let b = |percentage, status| BatteryInfo {
        percentage,
        level: BatteryLevel::Good,
        status,
    };
    assert!(battery_charging_no_reading(&b(0, BatteryStatus::Charging)));
    assert!(battery_charging_no_reading(&b(
        0,
        BatteryStatus::ChargingSlow
    )));
    assert!(!battery_charging_no_reading(&b(
        40,
        BatteryStatus::Charging
    )));
    assert!(!battery_charging_no_reading(&b(
        0,
        BatteryStatus::Discharging
    )));
}

#[test]
fn low_discharging_battery_needs_attention() {
    let battery = |percentage, status| BatteryInfo {
        percentage,
        level: BatteryLevel::Low,
        status,
    };

    assert!(battery_needs_attention(&battery(
        20,
        BatteryStatus::Discharging
    )));
    assert!(!battery_needs_attention(&battery(
        21,
        BatteryStatus::Discharging
    )));
    assert!(!battery_needs_attention(&battery(
        20,
        BatteryStatus::Charging
    )));
}

/// Builds a [`DeviceModelInfo`] with `transports` and the packed per-transport
/// PIDs a real device would report for them (ascending bit order: Bluetooth,
/// BTLE, eQuad, USB — see [`DeviceModelInfo::model_ids`]), so tests can probe
/// `connection_icon_path` with a `Direct` route's product id matching one of
/// the live slots.
fn model_with_pids(transports: DeviceTransports, pids: &[u16]) -> DeviceModelInfo {
    let mut model_ids = [0u16; 3];
    model_ids[..pids.len()].copy_from_slice(pids);
    DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [0, 0, 0, 0],
        transports,
        model_ids,
        extended_model_id: 0,
    }
}

#[test]
fn connection_icon_matches_route() {
    let bolt = DeviceRoute::Bolt {
        receiver_uid: "r".into(),
        slot: 1,
    };
    let uni = DeviceRoute::Unifying {
        receiver_uid: "r".into(),
        slot: 1,
    };
    // Firmware transport tables (HID++ 0x0003): a wired-only device (G513),
    // a Bluetooth-capable one (MX Master on a cable or BT), and BLE-direct.
    let wired = DeviceTransports {
        usb: true,
        ..DeviceTransports::default()
    };
    let bt = DeviceTransports {
        usb: true,
        bluetooth: true,
        ..DeviceTransports::default()
    };
    let btle = DeviceTransports {
        btle: true,
        ..DeviceTransports::default()
    };
    assert_eq!(
        connection_icon_path(Some(&bolt), None),
        "action-icons/bolt.svg"
    );
    assert_eq!(
        connection_icon_path(Some(&uni), None),
        "action-icons/unifying.svg"
    );
    // The matching USB PID selects the cable icon.
    let direct_wired_pid = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb019,
    };
    assert_eq!(
        connection_icon_path(
            Some(&direct_wired_pid),
            Some(&model_with_pids(wired, &[0xb019]))
        ),
        "action-icons/usb.svg"
    );
    // eQuad is receiver-only, so an equad-only table on a *direct* route
    // still means a cable — not Bluetooth.
    let equad_only = DeviceTransports {
        equad: true,
        ..DeviceTransports::default()
    };
    let direct_equad_pid = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb01a,
    };
    assert_eq!(
        connection_icon_path(
            Some(&direct_equad_pid),
            Some(&model_with_pids(equad_only, &[0xb01a]))
        ),
        "action-icons/usb.svg"
    );
    // An all-false table is "unknown", not "wired".
    let direct_unknown_pid = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xffff,
    };
    assert_eq!(
        connection_icon_path(
            Some(&direct_unknown_pid),
            Some(&model_with_pids(DeviceTransports::default(), &[]))
        ),
        "action-icons/bluetooth.svg"
    );
    // Product id doesn't match a live slot (stale/unknown info): falls back
    // to guessing from the static flags, same as before this fix.
    assert_eq!(
        connection_icon_path(
            Some(&direct_unknown_pid),
            Some(&model_with_pids(bt, &[0xb019]))
        ),
        "action-icons/bluetooth.svg"
    );
    assert_eq!(
        connection_icon_path(
            Some(&direct_unknown_pid),
            Some(&model_with_pids(btle, &[0xb01b]))
        ),
        "action-icons/bluetooth.svg"
    );
    // Unknown transports (no 0x0003 snapshot) keep the old default.
    assert_eq!(
        connection_icon_path(Some(&direct_unknown_pid), None),
        "action-icons/bluetooth.svg"
    );
    // No route and no model information means the default icon.
    assert_eq!(
        connection_icon_path(None, None),
        "action-icons/bluetooth.svg"
    );
}

#[test]
fn connection_icon_preserves_saved_wired_device_without_live_route() {
    use openlogi_core::config::{Config, DeviceIdentity};

    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    let mut config = Config::ephemeral();
    config.set_device_identity(
        "unit:01020304",
        DeviceIdentity {
            display_name: "Wired keyboard".into(),
            model_info: Some(model_with_pids(
                DeviceTransports {
                    usb: true,
                    ..DeviceTransports::default()
                },
                &[0xc33a],
            )),
            codename: None,
            kind: DeviceKind::Keyboard,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Keyboard),
            light_capabilities: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources::in_memory(config, &resolver, commands));
    let record = state
        .current_record()
        .expect("saved keyboard must remain visible");

    assert!(!record.online);
    assert!(record.route.is_none());
    assert_eq!(
        connection_icon_path(record.route.as_ref(), record.model_info.as_ref()),
        "action-icons/usb.svg",
        "a saved wired-only keyboard must not acquire a Bluetooth icon while offline"
    );
}

/// Regression for issue #1218: a G915 X LS reports all three transports
/// (USB, eQuad, BTLE) as supported, so the static flags alone can't tell
/// which one is live. The `Direct` route's product id — the HID node this
/// session actually enumerated — must pick the icon instead.
#[test]
fn connection_icon_follows_live_route_on_multi_transport_device() {
    let model = model_with_pids(
        DeviceTransports {
            usb: true,
            equad: true,
            btle: true,
            bluetooth: false,
        },
        &[0xb38a, 0x40b5, 0xc356], // BTLE, eQuad, USB — ascending bit order
    );
    let usb_direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc356,
    };
    let btle_direct = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb38a,
    };
    assert_eq!(
        connection_icon_path(Some(&usb_direct), Some(&model)),
        "action-icons/usb.svg",
        "plugged in over USB must show the USB glyph even though the same \
         device also supports Bluetooth/BTLE"
    );
    assert_eq!(
        connection_icon_path(Some(&btle_direct), Some(&model)),
        "action-icons/bluetooth.svg",
        "the same device connected over BTLE must show the Bluetooth glyph"
    );
}

fn record(kind: DeviceKind, capabilities: Option<Capabilities>) -> DeviceRecord {
    DeviceRecord {
        config_key: "test".to_string(),
        canonical_key: None,
        persistent: true,
        route_key: "test".to_string(),
        model_key: "test".to_string(),
        model_name: "Test".to_string(),
        display_name: "Test".to_string(),
        asset: None,
        model_info: None,
        codename: None,
        serial_number: None,
        unit_id: [0; 4],
        driver_id: None,
        registry_model_id: None,
        route: None,
        capture_id: None,
        kind,
        capabilities,
        light_capabilities: None,
        slot: 1,
        online: true,
        battery: None,
    }
}

#[test]
fn gallery_order_moves_connected_devices_first_stably() {
    let mut records = vec![
        record(DeviceKind::Mouse, None),
        record(DeviceKind::Keyboard, None),
        record(DeviceKind::Trackball, None),
        record(DeviceKind::Light, None),
    ];
    records[0].online = false;
    records[2].online = false;

    assert_eq!(ordered_device_indices(&records), vec![1, 3, 0, 2]);
}

/// #1699: unlike the #127 scenario below, kind resolution itself can stay
/// wrong (a Bluetooth-direct MX Master 3S kept kind=Keyboard despite
/// measuring real buttons+pointer capabilities) — the panel choice has to
/// tolerate that directly, not just rely on kind having been corrected
/// upstream. A measured pointer capability must win Buttons over Keys even
/// then, and Keys must not also show for the same record.
#[test]
fn tabs_prefer_buttons_when_a_keyboard_kind_measures_a_pointer() {
    let caps = Some(Capabilities {
        buttons: true,
        pointer: true,
        lighting: false,
        scroll_inversion: false,
        hires_wheel: false,
        thumbwheel: false,
        haptic_feedback: false,
        haptic_panel: false,
        dpi_gestures: false,
        fn_lock: false,
    });
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
    assert!(
        tabs.contains(&DetailTab::Buttons),
        "a measured pointer capability must win Buttons even with kind=Keyboard"
    );
    assert!(
        !tabs.contains(&DetailTab::Keys),
        "the same record must not also show Keys"
    );
}

/// Tabs follow measured capabilities, not kind — the core of the #127 fix.
/// A device the Bolt register mislabels as Keyboard but whose 0x0005 probe
/// returns Mouse ends up with kind=Mouse; measured caps drive the tabs.
#[test]
fn tabs_follow_capabilities_not_kind() {
    let caps = Some(Capabilities {
        buttons: true,
        pointer: true,
        lighting: false,
        scroll_inversion: false,
        hires_wheel: false,
        thumbwheel: false,
        haptic_feedback: false,
        haptic_panel: false,
        dpi_gestures: false,
        fn_lock: false,
    });
    // After 0x0005 kind-correction the record has kind=Mouse, not Keyboard.
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Mouse, caps));
    assert!(tabs.contains(&DetailTab::Buttons));
    assert!(tabs.contains(&DetailTab::Pointer));
    assert!(!tabs.contains(&DetailTab::Lighting));
}

#[test]
fn wheel_controls_show_pointer_tab_without_adjustable_dpi() {
    for caps in [
        Capabilities {
            scroll_inversion: true,
            ..Capabilities::default()
        },
        Capabilities {
            hires_wheel: true,
            ..Capabilities::default()
        },
    ] {
        assert!(
            DetailTab::tabs_for(&record(DeviceKind::Mouse, Some(caps)))
                .contains(&DetailTab::Pointer)
        );
    }
}

/// A keyboard that exposes ReprogControls (buttons=true) but has no resolved
/// asset should not get the mouse-model Buttons panel — the generic mouse
/// hotspot layout (Middle Click, DPI Toggle, …) is wrong for a keyboard.
#[test]
fn keyboard_without_asset_hides_buttons_tab() {
    let caps = Some(Capabilities {
        buttons: true,
        pointer: false,
        lighting: true,
        scroll_inversion: false,
        hires_wheel: false,
        thumbwheel: false,
        haptic_feedback: false,
        haptic_panel: false,
        dpi_gestures: false,
        fn_lock: false,
    });
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
    assert!(
        !tabs.contains(&DetailTab::Buttons),
        "mouse model shown for keyboard"
    );
    assert!(tabs.contains(&DetailTab::Lighting));
}

#[test]
fn keyboard_with_buttons_shows_keys_tab() {
    let caps = Some(Capabilities {
        buttons: true,
        pointer: false,
        lighting: true,
        scroll_inversion: false,
        hires_wheel: false,
        thumbwheel: false,
        haptic_feedback: false,
        haptic_panel: false,
        dpi_gestures: false,
        fn_lock: false,
    });
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
    assert!(tabs.contains(&DetailTab::Keys));
    assert!(!tabs.contains(&DetailTab::Buttons));
}

/// A sleeping keyboard whose slot was never probed has no capability data,
/// but a resolved depot already says which controls it has: the Keys tab
/// shows them so bindings can be set before the keyboard wakes.
#[test]
fn keyboard_with_a_depot_but_no_capabilities_shows_keys_tab() {
    let mut keyboard = record(DeviceKind::Keyboard, None);
    keyboard.asset = Some(ResolvedAsset {
        depot: "mx_keys_mini".to_string(),
        display_name: "MX Keys Mini".to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: std::path::PathBuf::from("/tmp/mx-keys-mini.png"),
        hero_image_path: None,
        glow: None,
        metadata: openlogi_assets::Metadata::default(),
        png_width: 1872,
        png_height: 728,
    });
    assert!(DetailTab::tabs_for(&keyboard).contains(&DetailTab::Keys));
    assert!(!DetailTab::tabs_for(&record(DeviceKind::Keyboard, None)).contains(&DetailTab::Keys));
}

/// Each panel is independent: a lighting-only device (e.g. a keyboard with
/// RGB but no remappable keys yet) shows only Lighting + Device.
#[test]
fn lighting_only_device_shows_only_lighting() {
    let caps = Some(Capabilities {
        lighting: true,
        ..Capabilities::default()
    });
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
    assert_eq!(tabs, vec![DetailTab::Lighting, DetailTab::Device]);
}

#[test]
fn light_tab_follows_light_capabilities() {
    let mut device = record(DeviceKind::Light, None);
    device.light_capabilities = Some(LightCapabilities {
        power: true,
        brightness: Some(
            LightValueRange::new(20, 250, 1, LightValueUnit::Lumens)
                .expect("demo light range is valid"),
        ),
        ..LightCapabilities::default()
    });
    assert_eq!(
        DetailTab::tabs_for(&device),
        vec![DetailTab::Light, DetailTab::Device]
    );
}

/// An unprobed (offline) device has no measured capabilities and falls back
/// to a kind presumption, so a sleeping mouse keeps its button/pointer tabs.
#[test]
fn unprobed_mouse_falls_back_to_presumed_capabilities() {
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Mouse, None));
    assert!(tabs.contains(&DetailTab::Buttons));
    assert!(tabs.contains(&DetailTab::Pointer));
    assert!(!tabs.contains(&DetailTab::Lighting));
}

/// An unprobed, unidentified device presumes nothing — only the info tab,
/// rather than guessing wrong panels (the old Unknown+Direct→lighting bug).
#[test]
fn unprobed_unknown_device_shows_only_device_tab() {
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Unknown, None));
    assert_eq!(tabs, vec![DetailTab::Device]);
}

mod corpus;
