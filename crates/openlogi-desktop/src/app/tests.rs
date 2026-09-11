use super::home::{connection_icon_path, ordered_device_indices};
use super::{Capabilities, DetailTab, DeviceKind, DeviceRecord};
use crate::ui::battery::{battery_charging_no_reading, battery_needs_attention};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceModelInfo, DeviceTransports, LightCapabilities,
    LightValueRange, LightValueUnit,
};
use openlogi_core::hid::DeviceRoute;

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
    // Direct + radio-less firmware = the cable is the only possible link.
    // The product id doesn't match any model_ids slot (no model info given
    // beyond the flags), so this exercises the static-flags fallback.
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
    // No route (e.g. a synthetic/placeholder card) falls back to Bluetooth.
    assert_eq!(
        connection_icon_path(None, None),
        "action-icons/bluetooth.svg"
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
    });
    // After 0x0005 kind-correction the record has kind=Mouse, not Keyboard.
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Mouse, caps));
    assert!(tabs.contains(&DetailTab::Buttons));
    assert!(tabs.contains(&DetailTab::Pointer));
    assert!(!tabs.contains(&DetailTab::Lighting));
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
    });
    let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
    assert!(tabs.contains(&DetailTab::Keys));
    assert!(!tabs.contains(&DetailTab::Buttons));
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
