use super::home::{connection_icon_path, ordered_device_indices};
use super::{Capabilities, DetailTab, DeviceKind, DeviceRecord};
use crate::services::assets::ResolvedAsset;
use crate::ui::battery::battery_charging_no_reading;
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

/// The device rename dialog must paint its own Save and Cancel buttons: a
/// plain gpui-component dialog draws only the footer it is given.
#[gpui::test]
fn the_device_rename_dialog_offers_save_and_cancel(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, Modifiers};
    use gpui_component::Root;

    use super::{AppView, home::open_rename_dialog};
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::{AppState, Sources};

    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    cx.update(gpui_component::init);
    // The dialog slides in on the wall clock, so on a slow runner a redraw
    // between measuring Cancel and clicking it moves the button away from
    // the click. Reduced motion draws the dialog where it comes to rest.
    cx.update(|cx| cx.set_reduce_motion(true));
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
        open_rename_dialog(
            window,
            cx,
            "direct:046d:b35b:unit:a393cae0".to_owned(),
            String::new(),
            "MX Keys".to_owned(),
        );
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("dialog-save").is_some(),
        "the dialog can be saved"
    );
    let cancel = cx
        .debug_bounds("dialog-cancel")
        .expect("the dialog can be cancelled");

    cx.simulate_click(cancel.center(), Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("dialog-save").is_none(),
        "Cancel closes the dialog"
    );
}

/// The Pointer tab's Scrolling card carries the smooth-wheel switch, shows the
/// saved setting (a smooth wheel turned on earlier switches off) and knows this
/// link has `0x2121` (so it switches back on).
#[gpui::test]
fn the_pointer_tab_offers_the_smooth_wheel(cx: &mut gpui::TestAppContext) {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{AppContext as _, Modifiers, px, size};
    use gpui_component::Root;

    use super::AppView;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::tests::{KNOWN_MOUSE_KEY, direct_inventory};
    use crate::state::{AgentLink, AppState, Sources};
    use openlogi_ipc::{AgentStatus, InventoryHealth, PROTOCOL_VERSION};

    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    cx.update(gpui_component::init);
    cx.update(|cx| {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
        inventory.paired[0]
            .capabilities
            .as_mut()
            .expect("the fixture mouse has capabilities")
            .hires_wheel = true;
        let mut config = openlogi_core::config::Config::ephemeral();
        config.set_smooth_wheel(KNOWN_MOUSE_KEY, true);
        let state = cx.new(|_| {
            let mut state = AppState::new(Sources {
                inventories: &[inventory],
                ..Sources::in_memory(config, &resolver, commands)
            });
            // Past the connecting frame and the accessibility gate.
            let _ = state.set_agent_link(AgentLink::Ready(AgentStatus {
                accessibility_granted: true,
                hook_installed: true,
                launch_at_login: false,
                inventory: InventoryHealth::Ready,
                protocol_version: PROTOCOL_VERSION,
                agent_version: "smooth-wheel-test".to_string(),
                input_monitoring_granted: true,
                hid_open_failures: false,
            }));
            state
        });
        AppState::set_global(state, cx);
    });
    let hosted = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&hosted);
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| AppView::new(window, cx));
        *slot.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let app_view = hosted
        .borrow_mut()
        .take()
        .expect("the window hosts the app view");
    cx.simulate_resize(size(px(1280.), px(900.)));
    cx.update(|window, cx| {
        let record_key = AppState::try_read(cx)
            .expect("state is installed")
            .devices()[0]
            .record_key()
            .clone();
        app_view.update(cx, |view, cx| {
            view.open_device(record_key, cx);
            view.active_tab = DetailTab::Pointer;
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });

    let switch = cx
        .debug_bounds("smooth-wheel-row-control")
        .expect("the Scrolling card offers the smooth wheel");
    let mut click_and_read = || {
        cx.simulate_click(switch.center(), Modifiers::none());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            AppState::try_read(cx)
                .expect("state is installed")
                .current_smooth_wheel()
        })
    };
    assert!(
        !click_and_read(),
        "the switch shows the saved setting, so a click turns it off"
    );
    assert!(
        click_and_read(),
        "this link has 0x2121, so it turns back on"
    );
}

/// "Charging" replaces the bogus percentage only when charging *and* the
/// reading is still 0% or absent with no level (cold start, no cached
/// pre-charge value). A non-zero charge, a real 0% while discharging, or a
/// level-only reading keeps something to show.
#[test]
fn charging_without_reading_suppresses_percentage() {
    let b = |percentage, status| BatteryInfo {
        percentage,
        level: BatteryLevel::Good,
        status,
    };
    assert!(battery_charging_no_reading(&b(
        Some(0),
        BatteryStatus::Charging
    )));
    assert!(battery_charging_no_reading(&b(
        Some(0),
        BatteryStatus::ChargingSlow
    )));
    assert!(!battery_charging_no_reading(&b(
        Some(40),
        BatteryStatus::Charging
    )));
    assert!(!battery_charging_no_reading(&b(
        Some(0),
        BatteryStatus::Discharging
    )));
    // A level-only device that is charging still has its level to show.
    assert!(!battery_charging_no_reading(&b(
        None,
        BatteryStatus::Charging
    )));
    assert!(battery_charging_no_reading(&BatteryInfo {
        percentage: None,
        level: BatteryLevel::Unknown,
        status: BatteryStatus::Charging,
    }));
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
        connection_icon_path(Some(&bolt), None, None),
        "action-icons/bolt.svg"
    );
    assert_eq!(
        connection_icon_path(Some(&uni), None, None),
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
            Some(&model_with_pids(wired, &[0xb019])),
            None
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
            Some(&model_with_pids(equad_only, &[0xb01a])),
            None
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
            Some(&model_with_pids(DeviceTransports::default(), &[])),
            None
        ),
        "action-icons/bluetooth.svg"
    );
    // Product id doesn't match a live slot (stale/unknown info): falls back
    // to guessing from the static flags, same as before this fix.
    assert_eq!(
        connection_icon_path(
            Some(&direct_unknown_pid),
            Some(&model_with_pids(bt, &[0xb019])),
            None
        ),
        "action-icons/bluetooth.svg"
    );
    assert_eq!(
        connection_icon_path(
            Some(&direct_unknown_pid),
            Some(&model_with_pids(btle, &[0xb01b])),
            None
        ),
        "action-icons/bluetooth.svg"
    );
    // Unknown transports (no 0x0003 snapshot) keep the old default.
    assert_eq!(
        connection_icon_path(Some(&direct_unknown_pid), None, None),
        "action-icons/bluetooth.svg"
    );
    // No route and no model information means the default icon.
    assert_eq!(
        connection_icon_path(None, None, None),
        "action-icons/circle-dot.svg"
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
        connection_icon_path(
            record.route.as_ref(),
            record.model_info.as_ref(),
            state.offline_connection(record)
        ),
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
        connection_icon_path(Some(&usb_direct), Some(&model), None),
        "action-icons/usb.svg",
        "plugged in over USB must show the USB glyph even though the same \
         device also supports Bluetooth/BTLE"
    );
    assert_eq!(
        connection_icon_path(Some(&btle_direct), Some(&model), None),
        "action-icons/bluetooth.svg",
        "the same device connected over BTLE must show the Bluetooth glyph"
    );
}

#[test]
fn disconnected_gaming_keyboards_keep_the_observed_usb_icon() {
    use openlogi_core::config::Config;
    use openlogi_core::device::{PairedDevice, ReceiverInfo};

    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    for (name, pids) in [
        ("G915 X LS", [0xb38a, 0x40b5, 0xc356]),
        ("G515 LS TKL", [0xb389, 0x40b4, 0xc355]),
    ] {
        let mut model = model_with_pids(
            DeviceTransports {
                usb: true,
                equad: true,
                btle: true,
                bluetooth: false,
            },
            &pids,
        );
        model.unit_id = [1, 2, 3, 4];
        let inventory = openlogi_core::device::DeviceInventory {
            receiver: ReceiverInfo {
                name: name.into(),
                vendor_id: 0x046d,
                product_id: pids[2],
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
                codename: Some(name.into()),
                wpid: None,
                kind: DeviceKind::Keyboard,
                online: true,
                battery: None,
                model_info: Some(model),
                capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Keyboard)),
                settings_revision: 0,
            }],
        };
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: std::slice::from_ref(&inventory),
            ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
        });
        let record = state.current_record().expect("connected keyboard");
        assert!(record.online);
        assert_eq!(
            connection_icon_path(
                record.route.as_ref(),
                record.model_info.as_ref(),
                state.offline_connection(record)
            ),
            "action-icons/usb.svg"
        );

        let _ = state.refresh_inventories(&[], &[], &resolver, &[]);
        let record = state.current_record().expect("saved disconnected keyboard");
        assert!(!record.online);
        assert!(
            record.route.is_none(),
            "offline writes must remain disabled"
        );
        assert_eq!(
            connection_icon_path(
                record.route.as_ref(),
                record.model_info.as_ref(),
                state.offline_connection(record)
            ),
            "action-icons/usb.svg",
            "{name} must not acquire a Bluetooth icon when its USB node disappears"
        );
    }
}

#[test]
fn disconnected_multi_transport_keyboard_without_history_is_unknown() {
    let model = model_with_pids(
        DeviceTransports {
            usb: true,
            equad: true,
            btle: true,
            bluetooth: false,
        },
        &[0xb38a, 0x40b5, 0xc356],
    );
    assert_eq!(
        connection_icon_path(None, Some(&model), None),
        "action-icons/circle-dot.svg"
    );
}

#[test]
fn disconnected_keyboard_icon_uses_only_unambiguous_saved_links() {
    use openlogi_core::config::{Config, DeviceIdentity};
    use openlogi_core::device_order::PhysicalDeviceKey;

    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    let cases: &[(&[&str], &str)] = &[
        (&["direct:046d:c355"], "action-icons/usb.svg"),
        (&["direct:046d:b389"], "action-icons/bluetooth.svg"),
        (
            &["direct:046d:c355", "direct:046d:b389"],
            "action-icons/circle-dot.svg",
        ),
        (&["direct:046d:ffff"], "action-icons/circle-dot.svg"),
        (&["direct:invalid:c355"], "action-icons/circle-dot.svg"),
        (&["direct:046d:c355:extra"], "action-icons/circle-dot.svg"),
    ];
    for (links, expected) in cases {
        let mut config = Config::ephemeral();
        let key = PhysicalDeviceKey::parse("unit:01020304").expect("synthetic identity");
        config.set_device_identity(
            key.as_str(),
            DeviceIdentity {
                display_name: "G515 LS TKL".into(),
                model_info: Some(model_with_pids(
                    DeviceTransports {
                        usb: true,
                        equad: true,
                        btle: true,
                        bluetooth: false,
                    },
                    &[0xb389, 0x40b4, 0xc355],
                )),
                codename: None,
                kind: DeviceKind::Keyboard,
                capabilities: Capabilities::presumed_from_kind(DeviceKind::Keyboard),
                light_capabilities: None,
                driver_id: None,
                registry_model_id: None,
            },
        );
        for link in *links {
            config.adopt_route(&key, link, None);
        }
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::new(Sources::in_memory(config, &resolver, commands));
        let record = state
            .current_record()
            .expect("saved keyboard after restart");
        assert!(!record.online);
        assert!(record.route.is_none());
        assert_eq!(
            connection_icon_path(
                record.route.as_ref(),
                record.model_info.as_ref(),
                state.offline_connection(record),
            ),
            *expected,
            "saved links: {links:?}"
        );
    }
}

#[test]
fn live_bluetooth_icon_does_not_use_a_saved_usb_transport() {
    use openlogi_core::device::ModelTransport;

    let model = model_with_pids(
        DeviceTransports {
            usb: true,
            equad: true,
            btle: true,
            bluetooth: false,
        },
        &[0xb389, 0x40b4, 0xc355],
    );
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb389,
    };
    assert_eq!(
        connection_icon_path(
            Some(&route),
            Some(&model),
            Some(crate::state::ObservedConnection::Direct(
                ModelTransport::Usb
            ))
        ),
        "action-icons/bluetooth.svg"
    );
}

fn keyboard_inventory_for_mode(
    model: &DeviceModelInfo,
    transport: openlogi_core::device::ModelTransport,
    product_id: u16,
) -> openlogi_core::device::DeviceInventory {
    use openlogi_core::device::{DeviceInventory, ModelTransport, PairedDevice, ReceiverInfo};

    let receiver = transport == ModelTransport::Equad;
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Synthetic mode-switch keyboard".into(),
            vendor_id: 0x046d,
            product_id: if receiver { 0xc52b } else { product_id },
            unique_id: receiver.then(|| "synthetic-receiver".into()),
        },
        paired: vec![PairedDevice {
            slot: if receiver {
                1
            } else {
                openlogi_core::hid::DIRECT_DEVICE_INDEX
            },
            codename: Some("Synthetic mode-switch keyboard".into()),
            wpid: receiver.then_some(product_id),
            kind: DeviceKind::Keyboard,
            online: true,
            battery: None,
            model_info: Some(model.clone()),
            capabilities: Some(Capabilities {
                buttons: true,
                ..Capabilities::default()
            }),
            settings_revision: 0,
        }],
    }
}

fn assert_keyboard_profile(state: &crate::state::AppState, key: &str, icon: &str) {
    use openlogi_core::binding::{Action, ButtonId};

    assert_eq!(state.devices().len(), 1, "one gallery card");
    assert_eq!(state.config_summary().1, 1, "one saved device profile");
    let record = state.current_record().expect("selected keyboard");
    assert_eq!(record.record_key(), key);
    assert_eq!(record.display_name, "My keyboard");
    assert_eq!(
        state.button_bindings().get(&ButtonId::control(0x00c0)),
        Some(&Action::Screenshot)
    );
    assert_eq!(
        connection_icon_path(
            record.route.as_ref(),
            record.model_info.as_ref(),
            state.offline_connection(record)
        ),
        icon,
    );
}

fn assert_keyboard_modes(
    observations: &[(Vec<openlogi_core::device::DeviceInventory>, &'static str)],
    offline_icon: Option<&str>,
) {
    use openlogi_core::binding::{Action, ButtonId};
    use openlogi_core::config::Config;

    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources, StateEvent};

    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
    let mut original_key = None;
    for (inventory, icon) in observations.iter().chain(observations.iter()) {
        let before = state
            .current_record()
            .and_then(|record| record.route.clone());
        let events = state.refresh_inventories(inventory, &[], &resolver, &[]);
        let record = state.current_record().expect("mode-switch keyboard");
        let key = record.record_key();
        if before.as_ref() != record.route.as_ref() {
            assert!(
                events.contains(&StateEvent::InventoryChanged),
                "a route change must repaint"
            );
        }
        if original_key.is_none() {
            original_key = Some(key.clone());
            let _ = state.commit_device_custom_name(&key, "My keyboard");
            let _ = state.commit_binding(ButtonId::control(0x00c0), Action::Screenshot);
        }
        assert_keyboard_profile(&state, original_key.as_deref().unwrap(), icon);
    }
    if let Some(icon) = offline_icon {
        let _ = state.refresh_inventories(&[], &[], &resolver, &[]);
        let record = state.current_record().expect("saved keyboard");
        assert!(!record.online);
        assert!(
            record.route.is_none(),
            "history must not restore a writable route"
        );
        assert_keyboard_profile(&state, original_key.as_deref().unwrap(), icon);
    }
}

#[test]
fn keyboard_mode_switches_keep_one_profile_and_update_the_icon() {
    use openlogi_core::device::ModelTransport;

    let slots = [
        (
            1,
            ModelTransport::Bluetooth,
            0xb701,
            "action-icons/bluetooth.svg",
        ),
        (
            2,
            ModelTransport::Btle,
            0xb702,
            "action-icons/bluetooth.svg",
        ),
        (
            4,
            ModelTransport::Equad,
            0x4070,
            "action-icons/unifying.svg",
        ),
        (8, ModelTransport::Usb, 0xc370, "action-icons/usb.svg"),
    ];
    // HID++ packs at most three enabled transport slots. Exercise every valid
    // combination, with both serial-backed and unit-only physical identities.
    for mask in (1u8..16).filter(|mask| mask.count_ones() <= 3) {
        let active_slots: Vec<_> = slots.iter().filter(|(bit, ..)| mask & bit != 0).collect();
        let pids: Vec<_> = active_slots.iter().map(|(_, _, pid, _)| *pid).collect();
        for serial in [None, Some("synthetic-keyboard-serial")] {
            let mut model = model_with_pids(
                DeviceTransports {
                    bluetooth: mask & 1 != 0,
                    btle: mask & 2 != 0,
                    equad: mask & 4 != 0,
                    usb: mask & 8 != 0,
                },
                &pids,
            );
            model.unit_id = [1, 2, 3, 4];
            model.serial_number = serial.map(str::to_string);
            let observations: Vec<_> = active_slots
                .iter()
                .map(|(_, transport, pid, icon)| {
                    (
                        vec![keyboard_inventory_for_mode(&model, *transport, *pid)],
                        *icon,
                    )
                })
                .collect();
            let (_, _, _, last_icon) = active_slots.last().unwrap();
            let has_direct_link = active_slots
                .iter()
                .any(|(_, transport, ..)| *transport != ModelTransport::Equad);
            assert_keyboard_modes(&observations, has_direct_link.then_some(*last_icon));
        }
    }
}

#[test]
fn incomplete_keyboard_probes_preserve_the_last_observed_connection() {
    use openlogi_core::device::ModelTransport;

    let mut model = model_with_pids(
        DeviceTransports {
            btle: true,
            usb: true,
            ..DeviceTransports::default()
        },
        &[0xb702, 0xc370],
    );
    model.unit_id = [1, 2, 3, 4];
    let usb = keyboard_inventory_for_mode(&model, ModelTransport::Usb, 0xc370);
    let bluetooth = keyboard_inventory_for_mode(&model, ModelTransport::Btle, 0xb702);

    for (latest, expected) in [
        (&usb, "action-icons/usb.svg"),
        (&bluetooth, "action-icons/bluetooth.svg"),
    ] {
        for missing_model in [true, false] {
            let mut incomplete = latest.clone();
            incomplete.paired[0].capabilities = None;
            incomplete.paired[0].model_info = if missing_model {
                None
            } else {
                // The probe has an identity but cannot resolve the live PID.
                let mut unknown = model.clone();
                unknown.model_ids = [0; 3];
                Some(unknown)
            };
            assert_keyboard_modes(
                &[
                    (vec![usb.clone()], "action-icons/usb.svg"),
                    (vec![bluetooth.clone()], "action-icons/bluetooth.svg"),
                    (vec![latest.clone()], expected),
                    // Keep the existing best-effort live icon; only the
                    // offline observation must survive the incomplete probe.
                    (vec![incomplete], "action-icons/bluetooth.svg"),
                ],
                Some(expected),
            );
        }
    }
}

#[test]
fn keyboard_bolt_and_bluetooth_switch_does_not_duplicate_the_profile() {
    use openlogi_core::device::ModelTransport;

    let mut model = model_with_pids(
        DeviceTransports {
            btle: true,
            usb: true,
            ..DeviceTransports::default()
        },
        &[0xb702, 0xc370],
    );
    model.unit_id = [1, 2, 3, 4];
    let usb = keyboard_inventory_for_mode(&model, ModelTransport::Usb, 0xc370);
    let bluetooth = keyboard_inventory_for_mode(&model, ModelTransport::Btle, 0xb702);
    let mut bolt = bluetooth.clone();
    bolt.receiver.product_id = 0xc548;
    bolt.receiver.unique_id = Some("synthetic-bolt-receiver".into());
    bolt.paired[0].slot = 1;
    bolt.paired[0].wpid = Some(0xb702);
    let mut sleeping_bolt = bolt.clone();
    sleeping_bolt.paired[0].online = false;
    sleeping_bolt.paired[0].model_info = None;
    sleeping_bolt.paired[0].capabilities = None;
    assert_keyboard_modes(
        &[
            (vec![usb.clone()], "action-icons/usb.svg"),
            (vec![bolt.clone()], "action-icons/bolt.svg"),
            (
                vec![sleeping_bolt, bluetooth.clone()],
                "action-icons/bluetooth.svg",
            ),
            (vec![usb.clone()], "action-icons/usb.svg"),
        ],
        Some("action-icons/usb.svg"),
    );
    assert_keyboard_modes(
        &[
            (vec![usb], "action-icons/usb.svg"),
            (vec![bluetooth], "action-icons/bluetooth.svg"),
            (vec![bolt], "action-icons/bolt.svg"),
        ],
        Some("action-icons/bolt.svg"),
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
        settings_revision: 0,
        stale_pairings: Vec::new(),
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
        os_layout: false,
        report_rate: false,
        backlight: false,
        easy_switch: false,
        virtual_thumbwheel: false,
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
        os_layout: false,
        report_rate: false,
        backlight: false,
        easy_switch: false,
        virtual_thumbwheel: false,
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
        os_layout: false,
        report_rate: false,
        backlight: false,
        easy_switch: false,
        virtual_thumbwheel: false,
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
        os_layout: false,
        report_rate: false,
        backlight: false,
        easy_switch: false,
        virtual_thumbwheel: false,
    });
    let keyboard = record(DeviceKind::Keyboard, caps);
    let tabs = DetailTab::tabs_for(&keyboard);
    assert!(tabs.contains(&DetailTab::Keys));
    assert!(!tabs.contains(&DetailTab::Buttons));
    // A key can be bound to the Actions Ring action, so the ring needs its
    // own editor tab even on a keyboard that has no mouse model (#1833).
    assert!(tabs.contains(&DetailTab::ActionsRing));
    // Keys stays the keyboard's opening tab: adding the ring editor must not
    // reassign `default_for` away from the main keyboard screen.
    assert_eq!(DetailTab::default_for(&keyboard), DetailTab::Keys);
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

/// A receiver-paired card names its slot, as a slot rather than the
/// device's Easy-Switch channel; a direct device has no slot to name.
#[test]
fn receiver_slot_is_named_only_for_receiver_routes() {
    use super::widgets::{connection_summary, receiver_slot_label};
    use crate::services::i18n::LOCALE_LOCK;

    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let mut bolt = record(DeviceKind::Keyboard, None);
    bolt.route = Some(DeviceRoute::Bolt {
        receiver_uid: "OL-BOLT-UID-0001".to_string(),
        slot: 4,
    });
    bolt.slot = 4;
    assert_eq!(receiver_slot_label(&bolt).as_deref(), Some("Slot 4"));
    assert_eq!(connection_summary(&bolt), "Bolt receiver · Slot 4");

    let mut direct = record(DeviceKind::Mouse, None);
    direct.route = Some(DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc083,
    });
    direct.slot = 0xff;
    assert_eq!(receiver_slot_label(&direct), None);
    assert_eq!(connection_summary(&direct), "Direct connection");
}
