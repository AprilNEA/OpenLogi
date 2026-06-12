//! Maps live GUI state onto the platform-free [`openlogi_core::diagnostics`] report.

use std::path::Path;

use gpui::App;
use openlogi_agent_core::ipc::{InventoryHealth, PROTOCOL_VERSION};
use openlogi_core::device::{DeviceInventory, PairedDevice};
use openlogi_core::diagnostics::{
    AppInfo, AssetInfo, AssetSource, ConnectionKind, DeviceDiag, DiagnosticsReport, InventoryState,
    ReceiverDiag, RenderState,
};
use openlogi_hid::DeviceRoute;

use crate::asset::AssetResolver;
use crate::state::{AppState, DpiStatus};

/// Build the report from the current app state, defaulting to an empty report before the global is installed.
#[must_use]
pub fn collect(cx: &App) -> DiagnosticsReport {
    let resolver = AssetResolver::new();
    let assets = asset_info(&resolver);
    let state = cx.try_global::<AppState>();
    let app = app_info(state, resolver.has_bundle_root());
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

fn app_info(state: Option<&AppState>, running_from_bundle: bool) -> AppInfo {
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
        accessibility_granted: status.is_some_and(|s| s.accessibility_granted),
        hook_installed: status.map(|s| s.hook_installed),
        launch_at_login: status.map(|s| s.launch_at_login),
        show_in_menu_bar: settings.map(|s| s.show_in_menu_bar),
        check_for_updates: settings.map(|s| s.check_for_updates),
        thumbwheel_sensitivity: settings.map(|s| s.thumbwheel_sensitivity),
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
    let inventories = state.last_inventory();
    state
        .device_list
        .iter()
        .map(|record| {
            let paired = find_paired(&record.config_key, inventories);
            let model = paired.and_then(|p| p.model_info.as_ref());
            DeviceDiag {
                display_name: record.display_name.clone(),
                kind: record.kind,
                codename: paired.and_then(|p| p.codename.clone()),
                connection: connection_for(record.route.as_ref(), model.map(|m| m.transports)),
                online: record.online,
                battery: record.battery.clone(),
                capabilities: record.capabilities,
                dpi: dpi_summary(state.dpi_status_for(&record.config_key)),
                config_key: record.config_key.clone(),
                wpid: paired.and_then(|p| p.wpid),
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

fn find_paired<'a>(
    config_key: &str,
    inventories: &'a [DeviceInventory],
) -> Option<&'a PairedDevice> {
    inventories.iter().flat_map(|inv| &inv.paired).find(|p| {
        p.model_info
            .as_ref()
            .is_some_and(|m| m.config_key() == config_key)
    })
}

/// Refine the HID++ route into a user-facing connection type via the device's announced transports.
fn connection_for(
    route: Option<&DeviceRoute>,
    transports: Option<openlogi_core::device::DeviceTransports>,
) -> ConnectionKind {
    match route {
        Some(DeviceRoute::Bolt { .. }) => ConnectionKind::BoltReceiver,
        Some(DeviceRoute::Unifying { .. }) => ConnectionKind::UnifyingReceiver,
        Some(DeviceRoute::Direct { .. }) => match transports {
            Some(t) if t.bluetooth || t.btle => ConnectionKind::BluetoothDirect,
            Some(t) if t.usb => ConnectionKind::Wired,
            _ => ConnectionKind::Unknown,
        },
        None => ConnectionKind::Unknown,
    }
}

fn dpi_summary(status: Option<DpiStatus>) -> Option<String> {
    match status? {
        DpiStatus::Unknown => None,
        DpiStatus::Loading => Some("querying…".to_string()),
        DpiStatus::Ready(info) => Some(format!(
            "{} dpi (range {}–{}, {} steps)",
            info.current,
            info.capabilities.min(),
            info.capabilities.max(),
            info.capabilities.values().len(),
        )),
        DpiStatus::Unsupported(_) => Some("unsupported".to_string()),
        DpiStatus::Failed(_) => Some("read failed".to_string()),
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
