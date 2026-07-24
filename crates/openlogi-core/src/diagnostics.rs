//! Privacy-filtered diagnostics report for support tickets — model-level only, no unique identifiers by construction.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::device::{BatteryInfo, BatteryStatus, Capabilities, DeviceKind, DeviceTransports};

/// Where the resolver found the bundled device renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSource {
    /// Read-only assets shipped inside the macOS `.app` bundle (release builds).
    Bundle,
    /// The per-user cache populated by the background asset sync.
    UserCache,
    /// Neither tier was found — devices fall back to the synthetic silhouette.
    None,
}

/// How a device reaches the host, refined past the raw HID++ route via announced transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    /// Paired through a Logi Bolt receiver.
    BoltReceiver,
    /// Paired through a legacy Unifying receiver.
    UnifyingReceiver,
    /// Connected directly over Bluetooth — no receiver involved.
    BluetoothDirect,
    /// Connected over a USB cable.
    Wired,
    /// The route could not be classified from the announced transports.
    Unknown,
}

/// Whether a curated render resolved, or the device fell back to the silhouette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "depot")]
pub enum RenderState {
    /// A curated render resolved; carries the depot name (e.g.
    /// `"mx_master_3s"`).
    Resolved(String),
    /// No depot matched — the UI draws the synthetic silhouette instead.
    Silhouette,
}

/// Agent-side device-enumeration health — explains an empty or stale device
/// section (e.g. a report copied while the agent is still scanning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryState {
    /// The first enumeration hasn't completed yet — the device set is unknown.
    Scanning,
    /// Enumeration completed; the device section is authoritative.
    Ready,
    /// Enumeration failed and is no longer retried; details in the agent log.
    Unavailable,
}

/// A receiver, by model only — never its `unique_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverDiag {
    /// Receiver product string — model-level, carries no per-unit identity.
    pub name: String,
    /// USB vendor ID (`0x046d` for Logitech).
    pub vendor_id: u16,
    /// USB product ID distinguishing the receiver model.
    pub product_id: u16,
}

/// One paired device, model-level only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceDiag {
    /// The name the GUI shows for this device.
    pub display_name: String,
    /// Classified device kind — an identity guess, not a capability claim.
    pub kind: DeviceKind,
    /// Firmware codename (e.g. `"MX Master 3S"`), when known.
    pub codename: Option<String>,
    /// How the device reaches the host.
    pub connection: ConnectionKind,
    /// Whether the device was reachable when the report was generated.
    pub online: bool,
    /// Battery snapshot, `None` when offline or unreported.
    pub battery: Option<BatteryInfo>,
    /// Measured HID++ capabilities, or `None` if never probed since the agent started.
    pub capabilities: Option<Capabilities>,
    /// Human DPI summary (current + supported range), or `None` when not queried.
    pub dpi: Option<String>,
    /// Model identifier (e.g. `"2b35a"`) — a per-model key, not user-identifying.
    pub config_key: String,
    /// Wireless PID from the receiver's pairing table, when paired via a
    /// receiver.
    pub wpid: Option<u16>,
    /// Per-transport PID array from HID++ DeviceInformation (0x0003).
    pub model_ids: Option<[u16; 3]>,
    /// Extended-model byte pairing with [`Self::model_ids`] to form the
    /// registry `modelId`.
    pub extended_model_id: Option<u8>,
    /// Transports announced by the firmware, when the device was probed.
    pub transports: Option<DeviceTransports>,
    /// Whether a curated render resolved, or the silhouette fallback drew.
    pub render: RenderState,
    /// Receiver slot, or `0xFF` for direct connections (rendered as
    /// "direct").
    pub slot: u8,
}

/// App, agent, and host environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Version of the GUI process writing the report.
    pub gui_version: String,
    /// `"debug"` or `"release"`.
    pub build_profile: String,
    /// `None` when the agent is unreachable (not yet connected / restarting).
    pub agent_version: Option<String>,
    /// IPC protocol version compiled into the GUI.
    pub protocol_gui: u32,
    /// IPC protocol version the agent reported, `None` when unreachable.
    /// A mismatch with [`Self::protocol_gui`] is flagged in the rendered
    /// report.
    pub protocol_agent: Option<u32>,
    /// Enumeration health behind the device section, `None` when the agent
    /// status is unavailable.
    pub inventory: Option<InventoryState>,
    /// Raw `std::env::consts::OS` (`"macos"` / `"linux"` / `"windows"`).
    pub os: String,
    /// OS version string, when the platform exposes one.
    pub os_version: Option<String>,
    /// Host CPU architecture (e.g. `"arm64"`).
    pub arch: String,
    /// OS-reported locale, `None` when detection failed.
    pub system_locale: Option<String>,
    /// Explicit UI-language override, or `None` for "follow system".
    pub ui_language: Option<String>,
    /// Input-monitoring/Accessibility permission state — macOS gates the
    /// input hook on it.
    pub accessibility_granted: bool,
    /// `None` when the agent status is unavailable.
    pub hook_installed: Option<bool>,
    /// Launch-at-login setting, `None` when unknown.
    pub launch_at_login: Option<bool>,
    /// Menu-bar/tray icon setting, `None` when unknown.
    pub show_in_menu_bar: Option<bool>,
    /// Automatic update-check setting, `None` when unknown.
    pub check_for_updates: Option<bool>,
    /// Thumbwheel sensitivity setting, `None` when unknown.
    pub thumbwheel_sensitivity: Option<i32>,
    /// `schema_version` of the loaded `config.toml`, when one loaded.
    pub config_schema_version: Option<u32>,
    /// Number of device entries in the config, when known.
    pub configured_device_count: Option<usize>,
    /// `true` for an installed app bundle, `false` for a source/dev build.
    pub running_from_bundle: bool,
}

/// Asset-cache state behind device renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInfo {
    /// Which tier the render resolver is serving from.
    pub source: AssetSource,
    /// Whether a registry `index.json` parsed successfully.
    pub index_loaded: bool,
    /// Number of device models in the loaded index, when known.
    pub index_entries: Option<usize>,
    /// Whether the per-user asset cache directory exists.
    pub user_cache_present: bool,
    /// Cache directory with the home prefix redacted to `~`.
    pub cache_path: String,
    /// Whether the assets shipped inside the app bundle were found.
    pub bundle_present: bool,
}

/// The whole report. Render with [`Self::to_markdown`] for the clipboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// App, agent, and host environment section.
    pub app: AppInfo,
    /// Asset-cache state behind device renders.
    pub assets: AssetInfo,
    /// Model-level receiver list (may be empty for direct-only setups).
    pub receivers: Vec<ReceiverDiag>,
    /// Per-device detail, one entry per enumerated device.
    pub devices: Vec<DeviceDiag>,
}

impl DiagnosticsReport {
    /// Render the report as the Markdown blob copied to the clipboard.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "### OpenLogi Diagnostics\n");
        self.write_app(&mut out);
        self.write_assets(&mut out);
        self.write_devices(&mut out);
        out.truncate(out.trim_end().len());
        out
    }

    fn write_app(&self, out: &mut String) {
        let a = &self.app;
        let _ = writeln!(out, "**App**");
        let _ = writeln!(
            out,
            "- OpenLogi (GUI): v{} ({})",
            a.gui_version, a.build_profile
        );
        let agent = match &a.agent_version {
            Some(v) if *v == a.gui_version => format!("v{v} (connected)"),
            Some(v) => format!("v{v} (connected) ⚠️ version mismatch with GUI"),
            None => "not connected".to_string(),
        };
        let _ = writeln!(out, "- Agent: {agent}");
        let proto = match a.protocol_agent {
            Some(p) if p == a.protocol_gui => format!("GUI {} / agent {p}", a.protocol_gui),
            Some(p) => format!("GUI {} / agent {p} ⚠️ mismatch", a.protocol_gui),
            None => format!("GUI {} / agent —", a.protocol_gui),
        };
        let _ = writeln!(out, "- IPC protocol: {proto}");
        let inventory = match a.inventory {
            Some(InventoryState::Ready) => "ready",
            Some(InventoryState::Scanning) => "scanning (first enumeration in progress)",
            Some(InventoryState::Unavailable) => {
                "⚠️ unavailable (enumeration failed — see agent log)"
            }
            None => "—",
        };
        let _ = writeln!(out, "- Inventory: {inventory}");
        let os = match &a.os_version {
            Some(v) => format!("{} {} ({})", os_label(&a.os), v, a.arch),
            None => format!("{} ({})", os_label(&a.os), a.arch),
        };
        let _ = writeln!(out, "- OS: {os}");
        let locale = a.system_locale.as_deref().unwrap_or("unknown");
        let ui = a.ui_language.as_deref().unwrap_or("follow system");
        let _ = writeln!(out, "- Locale: {locale} (UI: {ui})");
        let _ = writeln!(
            out,
            "- Accessibility: {} · Input hook: {}",
            granted(a.accessibility_granted),
            opt_state(a.hook_installed, "installed", "not installed"),
        );
        let _ = writeln!(
            out,
            "- Launch at login: {} · Menu bar: {} · Update check: {}",
            opt_state(a.launch_at_login, "yes", "no"),
            opt_state(a.show_in_menu_bar, "yes", "no"),
            opt_state(a.check_for_updates, "on", "off"),
        );
        let source = if a.running_from_bundle {
            "app bundle (release)"
        } else {
            "source build (dev)"
        };
        let _ = writeln!(out, "- Running from: {source}");
        let _ = writeln!(
            out,
            "- Config: schema {} · {} configured device(s) · thumbwheel {}\n",
            opt_num(a.config_schema_version),
            opt_num(a.configured_device_count),
            opt_num(a.thumbwheel_sensitivity),
        );
    }

    fn write_assets(&self, out: &mut String) {
        let s = &self.assets;
        let _ = writeln!(out, "**Assets**");
        let index = match (s.index_loaded, s.index_entries) {
            (true, Some(n)) => format!("loaded ({n} models)"),
            (true, None) => "loaded".to_string(),
            (false, _) => "not loaded".to_string(),
        };
        let _ = writeln!(
            out,
            "- Source: {} · Index: {index} · User cache: {}",
            asset_source_label(s.source),
            if s.user_cache_present {
                "present"
            } else {
                "absent"
            },
        );
        let _ = writeln!(
            out,
            "- Cache path: {} · Bundle assets: {}\n",
            s.cache_path,
            if s.bundle_present {
                "present"
            } else {
                "absent"
            },
        );
    }

    fn write_devices(&self, out: &mut String) {
        let _ = writeln!(out, "**Devices ({})**", self.devices.len());
        if self.devices.is_empty() {
            let _ = writeln!(out, "- No devices detected.");
        }
        for d in &self.devices {
            let codename = d
                .codename
                .as_deref()
                .map(|c| format!(" (codename: {c})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {} — {}{codename}",
                d.display_name,
                kind_label(d.kind)
            );
            let _ = writeln!(
                out,
                "  - Connection: {} · Online: {} · Battery: {}",
                connection_label(d.connection),
                yes_no(d.online),
                battery_label(d.battery.as_ref()),
            );
            let caps = match d.capabilities {
                Some(c) => format!(
                    "buttons={}, pointer={}, lighting={}",
                    yes_no(c.buttons),
                    yes_no(c.pointer),
                    yes_no(c.lighting),
                ),
                None => "not probed".to_string(),
            };
            let _ = writeln!(out, "  - Capabilities: {caps}");
            if let Some(dpi) = &d.dpi {
                let _ = writeln!(out, "  - DPI: {dpi}");
            }
            let _ = writeln!(out, "  - Model: {}{}", d.config_key, model_detail(d));
            if let Some(t) = d.transports {
                let _ = writeln!(out, "  - Transports: {}", transports_label(t));
            }
            let render = match &d.render {
                RenderState::Resolved(depot) => depot.clone(),
                RenderState::Silhouette => "⚠️ none (silhouette)".to_string(),
            };
            let _ = writeln!(out, "  - Render: {render} · {}", slot_label(d.slot));
        }
        if !self.receivers.is_empty() {
            let _ = writeln!(out, "\n**Receivers ({})**", self.receivers.len());
            for r in &self.receivers {
                let _ = writeln!(
                    out,
                    "- {} (VID {:04x} / PID {:04x})",
                    r.name, r.vendor_id, r.product_id
                );
            }
        }
    }
}

fn model_detail(d: &DeviceDiag) -> String {
    let mut parts = Vec::new();
    if let Some(wpid) = d.wpid {
        parts.push(format!("wpid: {wpid:04x}"));
    }
    if let Some([a, b, c]) = d.model_ids {
        parts.push(format!("model-ids: {a:04x}/{b:04x}/{c:04x}"));
    }
    if let Some(ext) = d.extended_model_id {
        parts.push(format!("ext-model: {ext:02x}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn slot_label(slot: u8) -> String {
    // 0xFF is the HID++ direct-device index (USB cable / Bluetooth, no receiver).
    if slot == 0xFF {
        "direct".to_string()
    } else {
        format!("Slot {slot}")
    }
}

fn os_label(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    }
}

fn asset_source_label(source: AssetSource) -> &'static str {
    match source {
        AssetSource::Bundle => "app bundle",
        AssetSource::UserCache => "user cache",
        AssetSource::None => "none",
    }
}

fn kind_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Mouse => "mouse",
        DeviceKind::Keyboard => "keyboard",
        DeviceKind::Numpad => "numpad",
        DeviceKind::Presenter => "presenter",
        DeviceKind::Remote => "remote",
        DeviceKind::Trackball => "trackball",
        DeviceKind::Touchpad => "touchpad",
        DeviceKind::Tablet => "tablet",
        DeviceKind::Gamepad => "gamepad",
        DeviceKind::Joystick => "joystick",
        DeviceKind::Headset => "headset",
        DeviceKind::Unknown => "unknown",
    }
}

fn connection_label(connection: ConnectionKind) -> &'static str {
    match connection {
        ConnectionKind::BoltReceiver => "Logi Bolt receiver",
        ConnectionKind::UnifyingReceiver => "Logi Unifying receiver",
        ConnectionKind::BluetoothDirect => "Bluetooth (direct)",
        ConnectionKind::Wired => "Wired (USB)",
        ConnectionKind::Unknown => "unknown",
    }
}

fn battery_label(battery: Option<&BatteryInfo>) -> String {
    match battery {
        Some(b) => format!(
            "{}% ({}, {})",
            b.percentage,
            battery_status_label(b.status),
            battery_level_label(b.level),
        ),
        None => "n/a".to_string(),
    }
}

fn battery_status_label(status: BatteryStatus) -> &'static str {
    match status {
        BatteryStatus::Discharging => "discharging",
        BatteryStatus::Charging => "charging",
        BatteryStatus::ChargingSlow => "charging (slow)",
        BatteryStatus::Full => "full",
        BatteryStatus::Error => "error",
        BatteryStatus::Unknown => "unknown",
    }
}

fn battery_level_label(level: crate::device::BatteryLevel) -> &'static str {
    use crate::device::BatteryLevel;
    match level {
        BatteryLevel::Critical => "critical",
        BatteryLevel::Low => "low",
        BatteryLevel::Good => "good",
        BatteryLevel::Full => "full",
        BatteryLevel::Unknown => "unknown",
    }
}

fn transports_label(t: DeviceTransports) -> String {
    let mut parts = Vec::new();
    if t.usb {
        parts.push("USB");
    }
    if t.equad {
        parts.push("eQuad");
    }
    if t.btle {
        parts.push("BTLE");
    }
    if t.bluetooth {
        parts.push("Bluetooth");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn granted(value: bool) -> &'static str {
    if value { "granted" } else { "denied" }
}

fn opt_state(value: Option<bool>, yes: &'static str, no: &'static str) -> &'static str {
    match value {
        Some(true) => yes,
        Some(false) => no,
        None => "unknown",
    }
}

fn opt_num<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "—".to_string(), |v| v.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::{
        AppInfo, AssetInfo, AssetSource, ConnectionKind, DeviceDiag, DiagnosticsReport,
        InventoryState, ReceiverDiag, RenderState,
    };
    use crate::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceKind, DeviceTransports,
    };

    fn app() -> AppInfo {
        AppInfo {
            gui_version: "0.6.6".to_string(),
            build_profile: "release".to_string(),
            agent_version: Some("0.6.6".to_string()),
            protocol_gui: 1,
            protocol_agent: Some(1),
            inventory: Some(InventoryState::Ready),
            os: "macos".to_string(),
            os_version: Some("15.5".to_string()),
            arch: "arm64".to_string(),
            system_locale: Some("en-US".to_string()),
            ui_language: None,
            accessibility_granted: true,
            hook_installed: Some(true),
            launch_at_login: Some(true),
            show_in_menu_bar: Some(true),
            check_for_updates: Some(false),
            thumbwheel_sensitivity: Some(0),
            config_schema_version: Some(2),
            configured_device_count: Some(3),
            running_from_bundle: true,
        }
    }

    fn assets() -> AssetInfo {
        AssetInfo {
            source: AssetSource::Bundle,
            index_loaded: true,
            index_entries: Some(142),
            user_cache_present: true,
            cache_path: "~/.local/share/openlogi/assets".to_string(),
            bundle_present: true,
        }
    }

    fn sample() -> DiagnosticsReport {
        DiagnosticsReport {
            app: app(),
            assets: assets(),
            receivers: vec![ReceiverDiag {
                name: "Logi Bolt".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
            }],
            devices: vec![
                DeviceDiag {
                    display_name: "MX Keys".to_string(),
                    kind: DeviceKind::Keyboard,
                    codename: Some("MX Keys".to_string()),
                    connection: ConnectionKind::BoltReceiver,
                    online: true,
                    battery: Some(BatteryInfo {
                        percentage: 80,
                        level: BatteryLevel::Good,
                        status: BatteryStatus::Discharging,
                    }),
                    capabilities: Some(Capabilities::default()),
                    dpi: None,
                    config_key: "2b35a".to_string(),
                    wpid: Some(0x4093),
                    model_ids: Some([0xb35a, 0, 0]),
                    extended_model_id: Some(0x02),
                    transports: Some(DeviceTransports {
                        equad: true,
                        ..DeviceTransports::default()
                    }),
                    render: RenderState::Silhouette,
                    slot: 2,
                },
                DeviceDiag {
                    display_name: "MX Master 3S".to_string(),
                    kind: DeviceKind::Mouse,
                    codename: Some("MX Master 3S".to_string()),
                    connection: ConnectionKind::Wired,
                    online: false,
                    battery: None,
                    capabilities: Some(Capabilities {
                        buttons: true,
                        pointer: true,
                        lighting: false,
                        scroll_inversion: false,
                        hires_wheel: true,
                        onboard_profiles: false,
                    }),
                    dpi: Some("1600 dpi (range 200–8000, 5 steps)".to_string()),
                    config_key: "4082d".to_string(),
                    wpid: Some(0x4082),
                    model_ids: Some([0x082d, 0, 0]),
                    extended_model_id: Some(0x04),
                    transports: Some(DeviceTransports {
                        usb: true,
                        ..DeviceTransports::default()
                    }),
                    render: RenderState::Resolved("mx_master_3s".to_string()),
                    slot: 1,
                },
            ],
        }
    }

    #[test]
    fn renders_header_and_sections() {
        let md = sample().to_markdown();
        assert!(md.starts_with("### OpenLogi Diagnostics"));
        assert!(md.contains("**App**"));
        assert!(md.contains("**Assets**"));
        assert!(md.contains("**Devices (2)**"));
        assert!(md.contains("**Receivers (1)**"));
        assert!(md.contains("- Logi Bolt (VID 046d / PID c548)"));
        assert!(md.contains("- OpenLogi (GUI): v0.6.6 (release)"));
        assert!(md.contains("- Agent: v0.6.6 (connected)"));
        assert!(md.contains("- IPC protocol: GUI 1 / agent 1"));
        assert!(md.contains("- Inventory: ready"));
        assert!(md.contains("- OS: macOS 15.5 (arm64)"));
        assert!(
            md.contains("- Source: app bundle · Index: loaded (142 models) · User cache: present")
        );
        assert!(md.contains("- Config: schema 2 · 3 configured device(s) · thumbwheel 0"));
    }

    #[test]
    fn renders_device_detail() {
        let md = sample().to_markdown();
        assert!(md.contains("- MX Keys — keyboard (codename: MX Keys)"));
        assert!(md.contains(
            "Connection: Logi Bolt receiver · Online: yes · Battery: 80% (discharging, good)"
        ));
        assert!(md.contains("Capabilities: buttons=no, pointer=no, lighting=no"));
        assert!(md.contains("Model: 2b35a (wpid: 4093, model-ids: b35a/0000/0000, ext-model: 02)"));
        assert!(md.contains("Transports: eQuad"));
        assert!(md.contains("Render: ⚠️ none (silhouette) · Slot 2"));
        assert!(md.contains("- MX Master 3S — mouse"));
        assert!(md.contains("DPI: 1600 dpi (range 200–8000, 5 steps)"));
        assert!(md.contains("Transports: USB"));
        assert!(md.contains("Render: mx_master_3s · Slot 1"));
        assert!(md.contains("Battery: n/a"));
    }

    #[test]
    fn flags_version_and_protocol_mismatch() {
        let mut report = sample();
        report.app.agent_version = Some("0.6.5".to_string());
        report.app.protocol_agent = Some(2);
        let md = report.to_markdown();
        assert!(md.contains("v0.6.5 (connected) ⚠️ version mismatch with GUI"));
        assert!(md.contains("GUI 1 / agent 2 ⚠️ mismatch"));
    }

    #[test]
    fn omits_unique_identifiers_and_footer() {
        let md = sample().to_markdown();
        assert!(!md.contains("Serial"));
        assert!(!md.to_lowercase().contains("unit id"));
        assert!(!md.contains("omitted by design"));
    }

    #[test]
    fn direct_slot_renders_as_direct() {
        let mut report = sample();
        report.devices[0].slot = 0xFF;
        let md = report.to_markdown();
        assert!(md.contains("· direct"));
        assert!(!md.contains("Slot 255"));
    }

    #[test]
    fn unprobed_capabilities_render_not_probed() {
        let mut report = sample();
        report.devices[0].capabilities = None;
        let md = report.to_markdown();
        assert!(md.contains("  - Capabilities: not probed"));
    }

    #[test]
    fn empty_inventory_still_renders() {
        let report = DiagnosticsReport {
            app: app(),
            assets: assets(),
            receivers: Vec::new(),
            devices: Vec::new(),
        };
        let md = report.to_markdown();
        assert!(md.contains("**Devices (0)**"));
        assert!(md.contains("- No devices detected."));
    }

    #[test]
    fn unreachable_agent_renders_unknowns() {
        let mut report = sample();
        report.app.agent_version = None;
        report.app.protocol_agent = None;
        report.app.inventory = None;
        report.app.hook_installed = None;
        report.app.launch_at_login = None;
        let md = report.to_markdown();
        assert!(md.contains("- Agent: not connected"));
        assert!(md.contains("GUI 1 / agent —"));
        assert!(md.contains("- Inventory: —"));
        assert!(md.contains("Input hook: unknown"));
    }

    #[test]
    fn incomplete_enumeration_is_flagged() {
        let mut report = sample();
        report.app.inventory = Some(InventoryState::Scanning);
        assert!(
            report
                .to_markdown()
                .contains("- Inventory: scanning (first enumeration in progress)")
        );
        report.app.inventory = Some(InventoryState::Unavailable);
        assert!(
            report
                .to_markdown()
                .contains("- Inventory: ⚠️ unavailable (enumeration failed — see agent log)")
        );
    }
}
