//! Builds platform-free diagnostics reports from live GUI state.

use std::path::Path;

use gpui::App;
use openlogi_core::device::{DeviceModelInfo, ModelTransport};
use openlogi_core::diagnostics::{
    AppInfo, AssetInfo, AssetSource, ConnectionKind, DeviceDiag, DiagnosticsReport, InventoryState,
    ReceiverDiag, RenderState,
};
use openlogi_core::hid::{DeviceRoute, ReceiverBrand};
use openlogi_core::update_feed::UpdateChannel;
use openlogi_ipc::{InventoryHealth, PROTOCOL_VERSION, VirtualKeyboardStatus};

use crate::services::assets::AssetResolver;
use crate::state::{AppState, DpiLoad};

/// Build the report from the current app state, defaulting to an empty report before the entity is installed.
#[must_use]
pub fn collect(cx: &App) -> DiagnosticsReport {
    let resolver = AssetResolver::new();
    let assets = asset_info(&resolver);
    let state = AppState::try_global(cx);
    let state = state.as_ref().map(|state| state.read(cx));
    let app = app_info(
        state,
        crate::platform::updater::channel_in_use(cx),
        resolver.has_bundle_root(),
    );
    let (receivers, devices) = match state {
        Some(state) => (collect_receivers(state), collect_devices(state)),
        None => (Vec::new(), Vec::new()),
    };
    DiagnosticsReport {
        app,
        assets,
        receivers,
        devices,
    }
}

/// `update_channel` is the channel this launch's updater follows. A channel
/// picked since then applies at the next launch, so the report does not name it.
fn app_info(
    state: Option<&AppState>,
    update_channel: UpdateChannel,
    running_from_bundle: bool,
) -> AppInfo {
    // The live agent link, not a retained copy: a report copied while the
    // agent is down must say "not connected", not replay the last status.
    let status = state.and_then(AppState::agent_status);
    let settings = state.map(AppState::app_settings);
    let config = state.map(AppState::config_summary);
    let build_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    AppInfo {
        gui_version: env!("CARGO_PKG_VERSION").to_string(),
        build_profile: build_profile.to_string(),
        agent_version: status.map(|s| s.agent_version.clone()),
        protocol_gui: PROTOCOL_VERSION,
        protocol_agent: status.map(|s| s.protocol_version),
        inventory: status.map(|s| inventory_state(s.inventory)),
        os: std::env::consts::OS.to_string(),
        os_version: crate::platform::os::os_version(),
        arch: arch_label().to_string(),
        system_locale: sys_locale::get_locale(),
        ui_language: state.and_then(AppState::language).map(str::to_string),
        accessibility_granted: status.map(|s| s.accessibility_granted),
        hook_installed: status.map(|s| s.hook_installed),
        virtual_keyboard: status.and_then(|s| virtual_keyboard_state(s.virtual_keyboard)),
        launch_at_login: status.map(|s| s.launch_at_login),
        show_in_menu_bar: settings.map(|s| s.show_in_menu_bar),
        check_for_updates: settings.map(|s| s.check_for_updates),
        update_channel: Some(update_channel),
        thumbwheel_sensitivity: settings.map(|s| s.thumbwheel_sensitivity.into()),
        config_schema_version: config.map(|(version, _)| version),
        configured_device_count: config.map(|(_, count)| count),
        running_from_bundle,
    }
}

fn inventory_state(health: InventoryHealth) -> InventoryState {
    match health {
        InventoryHealth::Scanning => InventoryState::Scanning,
        InventoryHealth::Ready => InventoryState::Ready,
        InventoryHealth::Unavailable => InventoryState::Unavailable,
    }
}

/// `None` where the platform has no virtual keyboard, so the report leaves
/// the line out instead of calling it missing.
const fn virtual_keyboard_state(status: VirtualKeyboardStatus) -> Option<bool> {
    match status {
        VirtualKeyboardStatus::Active => Some(true),
        VirtualKeyboardStatus::Unavailable => Some(false),
        VirtualKeyboardStatus::Unsupported => None,
    }
}

fn asset_info(resolver: &AssetResolver) -> AssetInfo {
    let index_loaded = resolver.index_loaded();
    let source = if resolver.has_bundle_root() {
        AssetSource::Bundle
    } else if index_loaded {
        AssetSource::UserCache
    } else {
        AssetSource::None
    };
    AssetInfo {
        source,
        index_loaded,
        index_entries: resolver.index_entry_count(),
        user_cache_present: resolver.cache_root().exists(),
        cache_path: redact_home(resolver.cache_root()),
        bundle_present: resolver.has_bundle_root(),
    }
}

fn collect_receivers(state: &AppState) -> Vec<ReceiverDiag> {
    state
        .last_inventory()
        .iter()
        .filter(|inv| inv.receiver.unique_id.is_some())
        .map(|inv| ReceiverDiag {
            name: inv.receiver.name.clone(),
            vendor_id: inv.receiver.vendor_id,
            product_id: inv.receiver.product_id,
        })
        .collect()
}

fn collect_devices(state: &AppState) -> Vec<DeviceDiag> {
    state
        .devices()
        .iter()
        .map(|record| {
            // Each row reads its own record. A model key is not unique: every
            // device reporting all-zero model ids shares `00000` (#2312).
            let model = record.model_info.as_ref();
            DeviceDiag {
                display_name: record.display_name.clone(),
                kind: record.kind,
                codename: record.codename.clone(),
                connection: connection_for(record.route.as_ref(), record.receiver_brand, model),
                online: record.is_online(),
                battery: record.battery.clone(),
                capabilities: record.capabilities,
                dpi: dpi_summary(state.dpi_load_for(&record.device_key())),
                // Diagnostics are model-level by contract. The runtime config
                // key may contain a receiver UID or raw-device serial.
                config_key: record.model_key.clone(),
                wpid: record.wpid,
                model_ids: model.map(|m| m.model_ids),
                extended_model_id: model.map(|m| m.extended_model_id),
                transports: model.map(|m| m.transports),
                render: match &record.asset {
                    Some(asset) => RenderState::Resolved(asset.depot.clone()),
                    None => RenderState::Silhouette,
                },
                slot: record.slot,
            }
        })
        .collect()
}

/// Refine the HID++ route into a user-facing connection type.
///
/// A `Direct` route carries the vendor/product id of the HID node this
/// session actually enumerated, which — for a multi-transport device —
/// pinpoints the transport that's live *right now* via
/// [`DeviceModelInfo::transport_for_product_id`]. Falling back to the
/// static, unordered transport flags (as this used to do unconditionally)
/// mislabels a USB-connected multi-transport device as Bluetooth, since a
/// device that supports both always has both flags set: see issue #1218.
fn connection_for(
    route: Option<&DeviceRoute>,
    receiver_brand: Option<ReceiverBrand>,
    model: Option<&DeviceModelInfo>,
) -> ConnectionKind {
    match route {
        Some(DeviceRoute::Bolt { .. }) => ConnectionKind::BoltReceiver,
        Some(DeviceRoute::Unifying { .. } | DeviceRoute::Hidpp20Receiver { .. }) => {
            match receiver_brand {
                Some(ReceiverBrand::Nano) => ConnectionKind::NanoReceiver,
                Some(ReceiverBrand::Lightspeed) => ConnectionKind::LightspeedReceiver,
                Some(ReceiverBrand::Bolt | ReceiverBrand::Unifying) | None => {
                    ConnectionKind::UnifyingReceiver
                }
            }
        }
        Some(DeviceRoute::Direct { product_id, .. }) => {
            match model.and_then(|m| m.transport_for_product_id(*product_id)) {
                Some(ModelTransport::Bluetooth | ModelTransport::Btle) => {
                    ConnectionKind::BluetoothDirect
                }
                Some(ModelTransport::Equad | ModelTransport::Usb) => ConnectionKind::Wired,
                // Product id didn't match a known slot (no model info yet, or
                // an unrecognized PID) — fall back to the old best-effort
                // guess from the static capability flags.
                None => match model.map(|m| m.transports) {
                    Some(t) if t.bluetooth || t.btle => ConnectionKind::BluetoothDirect,
                    Some(t) if t.usb => ConnectionKind::Wired,
                    _ => ConnectionKind::Unknown,
                },
            }
        }
        Some(DeviceRoute::RawHid { .. }) => ConnectionKind::Wired,
        None => ConnectionKind::Unknown,
    }
}

fn dpi_summary(load: DpiLoad) -> Option<String> {
    match load {
        DpiLoad::Unknown => None,
        DpiLoad::Loading => Some("querying…".to_string()),
        DpiLoad::Ready(info) => Some(format!(
            "{} dpi (range {}–{}, {} steps)",
            info.current,
            info.capabilities.min(),
            info.capabilities.max(),
            info.capabilities.values().len(),
        )),
        DpiLoad::Unsupported(_) => Some("unsupported".to_string()),
        DpiLoad::Failed(_) => Some("read failed".to_string()),
    }
}

/// Replace the home-directory prefix of `path` with `~` so the report doesn't leak the username.
fn redact_home(path: &Path) -> String {
    let shown = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = shown.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    shown
}

fn arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

#[cfg(test)]
mod connection_for_tests {
    use openlogi_core::device::DeviceTransports;

    use super::{ConnectionKind, DeviceModelInfo, DeviceRoute, ReceiverBrand, connection_for};

    /// G915 X LS from issue #1218: reports all three transports (USB, eQuad,
    /// BTLE), model_ids packed BTLE/eQuad/USB per the ascending-bit-order
    /// contract in [`DeviceModelInfo::model_ids`].
    fn g915_x_ls_model() -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 3,
            serial_number: None,
            unit_id: [0, 0, 0, 0],
            transports: DeviceTransports {
                usb: true,
                equad: true,
                btle: true,
                bluetooth: false,
            },
            model_ids: [0xb38a, 0x40b5, 0xc356],
            extended_model_id: 0x01,
        }
    }

    #[test]
    fn multi_transport_device_over_usb_direct_is_wired() {
        let model = g915_x_ls_model();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc356, // the USB slot in model_ids
        };
        assert_eq!(
            connection_for(Some(&route), None, Some(&model)),
            ConnectionKind::Wired,
            "a device plugged in over USB must be labeled wired even though \
             it also supports Bluetooth/BTLE"
        );
    }

    #[test]
    fn multi_transport_device_over_btle_direct_is_bluetooth() {
        let model = g915_x_ls_model();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb38a, // the BTLE slot in model_ids
        };
        assert_eq!(
            connection_for(Some(&route), None, Some(&model)),
            ConnectionKind::BluetoothDirect,
            "the same multi-transport device connected over BTLE must be \
             labeled Bluetooth, proving the label follows the live route \
             rather than a fixed preference order"
        );
    }

    #[test]
    fn unrecognized_product_id_falls_back_to_capability_guess() {
        let model = g915_x_ls_model();
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xdead, // matches none of model_ids
        };
        // No live match: falls back to the old best-effort guess from the
        // static transport flags (bluetooth/btle preferred there too).
        assert_eq!(
            connection_for(Some(&route), None, Some(&model)),
            ConnectionKind::BluetoothDirect
        );
    }

    #[test]
    fn missing_model_info_falls_back_to_unknown() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc356,
        };
        assert_eq!(
            connection_for(Some(&route), None, None),
            ConnectionKind::Unknown
        );
    }

    #[test]
    fn bolt_and_unifying_routes_are_unaffected_by_transports() {
        let model = g915_x_ls_model();
        assert_eq!(
            connection_for(
                Some(&DeviceRoute::Bolt {
                    receiver_uid: "r1".to_string(),
                    slot: 1,
                }),
                None,
                Some(&model)
            ),
            ConnectionKind::BoltReceiver
        );
        assert_eq!(
            connection_for(
                Some(&DeviceRoute::Unifying {
                    receiver_uid: "r1".to_string(),
                    slot: 1,
                }),
                None,
                Some(&model)
            ),
            ConnectionKind::UnifyingReceiver
        );
    }

    #[test]
    fn receiver_product_families_use_the_registered_brand() {
        let route = DeviceRoute::Unifying {
            receiver_uid: "r1".to_string(),
            slot: 1,
        };
        for (brand, expected) in [
            (None, ConnectionKind::UnifyingReceiver),
            (
                Some(ReceiverBrand::Unifying),
                ConnectionKind::UnifyingReceiver,
            ),
            (Some(ReceiverBrand::Nano), ConnectionKind::NanoReceiver),
            (
                Some(ReceiverBrand::Lightspeed),
                ConnectionKind::LightspeedReceiver,
            ),
        ] {
            assert_eq!(connection_for(Some(&route), brand, None), expected);
        }
    }

    #[test]
    fn a_hidpp20_receiver_uses_the_registered_brand() {
        let route = DeviceRoute::Hidpp20Receiver {
            receiver_uid: "r1".to_string(),
            slot: 1,
        };
        assert_eq!(
            connection_for(Some(&route), Some(ReceiverBrand::Lightspeed), None),
            ConnectionKind::LightspeedReceiver
        );
    }
}

#[cfg(test)]
mod collect_devices_tests {
    use openlogi_core::config::Config;
    use openlogi_core::device::{
        DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
    };

    use super::{ConnectionKind, collect_devices};
    use crate::services::assets::AssetResolver;
    use crate::state::{AppState, Sources};

    /// A receiver with one online device whose model ids are all zero, so its
    /// model key is `00000`, the same as every other such device.
    fn zero_model_id_inventory(
        receiver_product_id: u16,
        receiver_uid: &str,
        kind: DeviceKind,
        codename: &str,
        wpid: u16,
    ) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Receiver".into(),
                vendor_id: 0x046d,
                product_id: receiver_product_id,
                unique_id: Some(receiver_uid.into()),
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some(codename.into()),
                wpid: Some(wpid),
                kind,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 0,
                    serial_number: None,
                    unit_id: [0; 4],
                    transports: DeviceTransports::default(),
                    model_ids: [0; 3],
                    extended_model_id: 0,
                }),
                capabilities: None,
                settings_revision: 0,
                hidpp10: false,
            }],
        }
    }

    /// #2312: a K750 on a Unifying receiver and a G602 on a Nano receiver
    /// both report all-zero model ids. Each row names its own device, in
    /// either inventory order.
    #[test]
    fn each_row_names_its_own_device_when_model_ids_collide() {
        let k750 = zero_model_id_inventory(
            0xc52b,
            "BE2E95ED",
            DeviceKind::Keyboard,
            "Wireless Solar Keyboard K750",
            0x400f,
        );
        let g602 = zero_model_id_inventory(
            0xc537,
            "F762998E",
            DeviceKind::Mouse,
            "Wireless Gaming Mouse",
            0x402c,
        );
        for inventories in [[k750.clone(), g602.clone()], [g602.clone(), k750.clone()]] {
            let resolver = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut state =
                AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
            let _ = state.refresh_inventories(&inventories, &[], &resolver, &[]);
            state.store_inventory_snapshot(&inventories);

            let rows: Vec<_> = collect_devices(&state)
                .into_iter()
                .map(|row| (row.connection, row.codename, row.wpid))
                .collect();
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert!(
                rows.contains(&(
                    ConnectionKind::UnifyingReceiver,
                    Some("Wireless Solar Keyboard K750".to_string()),
                    Some(0x400f),
                )),
                "{rows:?}"
            );
            assert!(
                rows.contains(&(
                    ConnectionKind::NanoReceiver,
                    Some("Wireless Gaming Mouse".to_string()),
                    Some(0x402c),
                )),
                "{rows:?}"
            );
        }
    }
}
