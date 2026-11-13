//! The menu-bar menu as data: which rows it has and what each one says,
//! decided from the agent's snapshot and config alone.
//!
//! Nothing here touches a toolkit, so every decision is tested once and each
//! platform's tray only draws the result (`tray` on macOS). Building the menu
//! reads no device and writes nothing: it shows what the agent already holds
//! in memory.

use std::borrow::Cow;
use std::collections::BTreeMap;

use openlogi_core::app::ForegroundApp;
use openlogi_core::brand::{DeeplinkCommand, DeviceLink};
use openlogi_core::config::{Config, canonical_device_key};
use openlogi_core::device::{
    BatteryAttention, BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceKind,
    LightCapabilities, StandaloneDevice, prettify_codename,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
use openlogi_core::diagnostics::ConnectionKind;
use openlogi_core::hid::{DIRECT_DEVICE_INDEX, DeviceRoute, find_receiver};
use openlogi_ipc::{AgentSnapshot, AgentStatus, FlowLinkState, ObservedLight};
use rust_i18n::t;

mod controls;

pub use controls::{
    Change, Controls, DeviceMemory, DpiControl, Entry, Failure, Read, Reading, Reads,
};
pub use openlogi_agent_core::orchestrator::RoutedDevice;

/// How many recently used applications the current-app submenu lists besides
/// the current one.
const RECENT_APPS_SHOWN: usize = 5;

/// Everything the tray menu shows above its fixed window and quit items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenu {
    /// What needs the user's attention, most urgent first. Empty in normal
    /// operation.
    pub issues: Vec<Issue>,
    /// Every device the agent sees, in the canonical device order the
    /// settings app's gallery uses.
    pub devices: Vec<DeviceRow>,
    /// The application per-app profiles resolve against, when there is one.
    pub app: Option<AppRow>,
    /// Flow's paired computers, while Flow is enabled.
    pub flow: Option<FlowRow>,
    /// The running agent's version, for the ⌥ diagnostics row.
    pub version: String,
    /// The IPC protocol the agent speaks, for the ⌥ diagnostics row.
    pub protocol: u32,
}

impl TrayMenu {
    /// Build the menu from the agent's current `snapshot` and `config`.
    #[must_use]
    pub fn new(snapshot: &AgentSnapshot, config: &Config) -> Self {
        let mut issues = actionable_issues(&snapshot.status);
        let has_key_bindings = !config.keyboard.bindings.is_empty();
        if let Some(app) = snapshot.status.secure_input_pausing_keys(has_key_bindings) {
            issues.push(Issue::SecureInput(app.map(|app| app.display_name.clone())));
        }
        Self {
            issues,
            devices: devices(snapshot, config),
            app: AppRow::new(snapshot, config),
            flow: FlowRow::new(snapshot),
            version: snapshot.status.agent_version.clone(),
            protocol: snapshot.status.protocol_version,
        }
    }
}

/// Whether the menu-bar icon should carry its attention mark: something the
/// user has to fix — any [`Issue`] but Secure Event Input, which ends on its
/// own — or an online device whose battery is critically low.
///
/// Reads the snapshot alone, so the agent can decide it on every state change
/// without the config the full menu needs.
#[must_use]
pub fn needs_attention(snapshot: &AgentSnapshot) -> bool {
    let critical_battery = snapshot
        .inventory
        .iter()
        .flat_map(|inventory| &inventory.paired)
        .filter(|device| device.online)
        .filter_map(|device| device.battery.as_ref())
        .any(|battery| battery.attention() == Some(BatteryAttention::Critical));
    critical_battery || !actionable_issues(&snapshot.status).is_empty()
}

/// The issues the user has to act on, in the order the menu lists them.
fn actionable_issues(status: &AgentStatus) -> Vec<Issue> {
    let mut issues = Vec::new();
    if !status.accessibility_granted {
        issues.push(Issue::MissingPermission(Permission::Accessibility));
    }
    if !status.input_monitoring_granted {
        issues.push(Issue::MissingPermission(Permission::InputMonitoring));
    } else if status.hid_open_failures {
        // Without the grant every open fails, and the permission row above
        // already says why.
        issues.push(Issue::DevicesUnavailable);
    }
    issues
}

/// A privacy permission the agent needs and lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// The input hook's event tap.
    Accessibility,
    /// Opening HID devices.
    InputMonitoring,
}

/// One row at the top of the menu, about something that needs attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// The agent lacks a permission. Clicking opens its System Settings pane.
    MissingPermission(Permission),
    /// Input Monitoring is granted, yet HID++ devices fail to open: another
    /// app holds them exclusively, or the grant needs a fresh login. Clicking
    /// opens Settings, which explains both.
    DevicesUnavailable,
    /// Secure Event Input is pausing key bindings, attributed to the named
    /// app when macOS names one. Explains only; nothing to click.
    SecureInput(Option<String>),
}

impl Issue {
    /// The row's title.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::MissingPermission(Permission::Accessibility) => {
                t!("tray.accessibility_needed").into_owned()
            }
            Self::MissingPermission(Permission::InputMonitoring) => {
                t!("tray.input_monitoring_needed").into_owned()
            }
            Self::DevicesUnavailable => t!("tray.devices_unavailable").into_owned(),
            Self::SecureInput(Some(app)) => {
                t!("keyboard.secure_input_pauses_remapping_for_app", app => app).into_owned()
            }
            Self::SecureInput(None) => t!("keyboard.secure_input_pauses_remapping").into_owned(),
        }
    }

    /// What the row's right side says clicking it does, or `None` when it
    /// does nothing.
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::MissingPermission(_) | Self::DevicesUnavailable => {
                Some(t!("tray.fix").into_owned())
            }
            Self::SecureInput(_) => None,
        }
    }
}

/// One device in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// The user's name for it, or the name it reports.
    pub name: String,
    /// What it is, for its icon.
    pub kind: DeviceKind,
    /// What its row reports.
    pub state: DeviceState,
    /// How it reaches this computer — what the ⌥ row shows.
    pub link: Link,
    /// What its "Open Device Settings…" item opens: the device's own page,
    /// or the main window for a device no link can name (one known only by
    /// its receiver slot).
    pub settings: DeeplinkCommand,
    /// Where the agent reads and writes it, when it knows.
    pub route: Option<DeviceRoute>,
    /// What its submenu can change.
    pub controls: Controls,
    /// The HID++ `0x0003` unit ID it reported, which tells it from another
    /// device later on its route.
    unit: [u8; 4],
}

/// What a device row reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    /// Online, with a battery reading.
    Battery(BatteryInfo),
    /// An online light, as it last read itself back.
    Light(LightReading),
    /// Online with nothing more to report: a wired device, or a light the
    /// agent cannot read back.
    Online,
    /// Not reachable now, with the last battery reading it gave.
    Offline(Option<BatteryInfo>),
}

/// A light's state, as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightReading {
    /// Whether it is on.
    pub on: bool,
    /// Its brightness, in percent of its range.
    pub brightness_percent: Option<u8>,
    /// Its colour temperature in Kelvin.
    pub temperature_kelvin: Option<u16>,
}

impl LightReading {
    fn of(light: &ObservedLight) -> Self {
        let state = &light.state;
        Self {
            on: state.on,
            brightness_percent: state
                .capabilities
                .brightness
                .zip(state.brightness)
                .and_then(|(range, value)| range.percent_for_native(value)),
            temperature_kelvin: state.temperature_kelvin,
        }
    }
}

/// How a device reaches this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The kind of connection.
    pub kind: ConnectionKind,
    /// The receiver pairing slot, for a device behind a receiver.
    pub slot: Option<u8>,
}

impl Link {
    /// The link in words: "Bolt receiver · Slot 1", "Bluetooth".
    #[must_use]
    pub fn text(self) -> String {
        let kind = match self.kind {
            ConnectionKind::BoltReceiver => t!("device.bolt_receiver"),
            ConnectionKind::UnifyingReceiver => t!("device.unifying_receiver"),
            ConnectionKind::NanoReceiver => t!("device.nano_receiver"),
            ConnectionKind::LightspeedReceiver => t!("device.lightspeed_receiver"),
            ConnectionKind::BluetoothDirect => t!("device.bluetooth"),
            ConnectionKind::Wired => t!("device.usb"),
            ConnectionKind::Unknown => t!("device.direct_connection"),
        };
        match self.slot {
            Some(slot) => {
                let slot = t!("device.receiver_slot_number", number => slot);
                format!("{kind} · {slot}")
            }
            None => kind.into_owned(),
        }
    }
}

impl DeviceRow {
    /// The device at its route, when the agent knows the route: what the
    /// menu remembers its readings under and makes its changes to.
    #[must_use]
    pub fn routed(&self) -> Option<RoutedDevice> {
        self.route.clone().map(|route| RoutedDevice {
            route,
            unit: self.unit,
        })
    }

    /// What the row's right side shows, or `None`.
    #[must_use]
    pub fn detail(&self) -> Option<Detail> {
        match &self.state {
            DeviceState::Battery(battery) => Some(Detail::Battery(battery.clone())),
            DeviceState::Light(light) => Some(Detail::Text(light_text(*light))),
            DeviceState::Online => None,
            DeviceState::Offline(_) => Some(Detail::Text(t!("device.offline").into_owned())),
        }
    }

    /// The read-only lines its submenu lists.
    #[must_use]
    pub fn info(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match &self.state {
            DeviceState::Battery(battery) => {
                lines.push(t!("tray.battery", level => battery_line(battery)).into_owned());
            }
            DeviceState::Light(light) => {
                lines.push(light_text(*light));
                if let Some(percent) = light.brightness_percent {
                    lines.push(t!("tray.brightness", percent => percent).into_owned());
                }
                if let Some(kelvin) = light.temperature_kelvin {
                    lines.push(t!("tray.color_temperature", kelvin => kelvin).into_owned());
                }
            }
            DeviceState::Online => {}
            DeviceState::Offline(battery) => {
                lines.push(t!("device.offline").into_owned());
                if let Some(battery) = battery {
                    lines.push(
                        t!("tray.last_known_battery", level => battery_text(battery)).into_owned(),
                    );
                }
            }
        }
        lines.push(self.link.text());
        lines
    }
}

/// What a device row's right side shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detail {
    /// A battery glyph and its reading ([`battery_text`]).
    Battery(BatteryInfo),
    /// Plain text.
    Text(String),
}

/// How full a battery glyph is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryFill {
    /// Empty.
    Empty,
    /// A quarter.
    Quarter,
    /// Half.
    Half,
    /// Three quarters.
    ThreeQuarters,
    /// Full.
    Full,
}

impl BatteryFill {
    /// The glyph for `battery`: its percentage to the nearest quarter, or its
    /// coarse level when it reports none.
    #[must_use]
    pub const fn of(battery: &BatteryInfo) -> Self {
        match battery.percentage {
            Some(0..=12) => Self::Empty,
            Some(13..=37) => Self::Quarter,
            Some(38..=62) => Self::Half,
            Some(63..=87) => Self::ThreeQuarters,
            Some(_) => Self::Full,
            None => match battery.level {
                BatteryLevel::Critical => Self::Empty,
                BatteryLevel::Low => Self::Quarter,
                BatteryLevel::Good | BatteryLevel::Unknown => Self::Half,
                BatteryLevel::Full => Self::Full,
            },
        }
    }
}

/// Whether `battery` is taking a charge, which its glyph shows with a bolt.
#[must_use]
pub const fn is_charging(battery: &BatteryInfo) -> bool {
    matches!(
        battery.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    )
}

/// A battery reading in a few characters: "62%", or the coarse level when the
/// device reports no percentage.
#[must_use]
pub fn battery_text(battery: &BatteryInfo) -> String {
    match (battery.percentage, battery.level) {
        (Some(percent), _) => t!("tray.percent", percent => percent).into_owned(),
        (None, BatteryLevel::Full) => t!("device.full").into_owned(),
        (None, BatteryLevel::Good) => t!("device.battery_good").into_owned(),
        (None, BatteryLevel::Low | BatteryLevel::Critical) => t!("device.low_battery").into_owned(),
        (None, BatteryLevel::Unknown) if is_charging(battery) => t!("device.charging").into_owned(),
        (None, BatteryLevel::Unknown) => "—".to_owned(),
    }
}

/// [`battery_text`], plus the charging state when there is one to name.
fn battery_line(battery: &BatteryInfo) -> String {
    let level = battery_text(battery);
    let status = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => t!("device.charging"),
        BatteryStatus::Full => t!("device.full"),
        BatteryStatus::Error => t!("device.battery_error"),
        BatteryStatus::Discharging | BatteryStatus::Unknown => return level,
    };
    if status == level {
        level
    } else {
        format!("{level} · {status}")
    }
}

fn light_text(light: LightReading) -> String {
    match (light.on, light.brightness_percent) {
        (true, Some(percent)) => t!("tray.light_on_percent", percent => percent).into_owned(),
        (true, None) => t!("tray.light_on").into_owned(),
        (false, _) => t!("tray.light_off").into_owned(),
    }
}

/// Every device in the snapshot, receiver-paired and standalone, in the
/// canonical [`DeviceStableId`] order.
fn devices(snapshot: &AgentSnapshot, config: &Config) -> Vec<DeviceRow> {
    let mut rows: Vec<(DeviceStableId, DeviceRow)> = Vec::new();
    for inventory in &snapshot.inventory {
        let brand = find_receiver(inventory.receiver.vendor_id, inventory.receiver.product_id)
            .map(|receiver| receiver.brand);
        for device in &inventory.paired {
            let route = DeviceRoute::for_slot(inventory, device.slot);
            let model = device.model_info.as_ref();
            let serial = model.and_then(|model| model.serial_number.as_deref());
            let unit_id = model.map_or([0; 4], |model| model.unit_id);
            let stable = DeviceStableId::from_parts(route.as_ref(), device.slot, serial, unit_id);
            let identity = DeviceIdentity::from_parts(serial, unit_id);
            let known = Known::of(config, &stable, device.online.then_some(&identity));
            let capabilities = device.capabilities.unwrap_or_else(|| {
                if device.hidpp10 {
                    Capabilities::presumed_for_hidpp10(device.kind)
                } else {
                    Capabilities::presumed_from_kind(device.kind)
                }
            });
            let controls = known.controls(config, device.kind, capabilities, &stable);
            let name = known
                .name
                .or_else(|| device.codename.as_deref().map(prettify_codename))
                .unwrap_or_else(|| {
                    t!("device.receiver_slot_number", number => device.slot).into_owned()
                });
            let state = match (device.online, &device.battery) {
                (false, battery) => DeviceState::Offline(battery.clone()),
                (true, Some(battery)) => DeviceState::Battery(battery.clone()),
                (true, None) => DeviceState::Online,
            };
            let slot = matches!(
                route,
                Some(DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })
            )
            .then_some(device.slot);
            let link = Link {
                kind: ConnectionKind::of(route.as_ref(), brand, model),
                slot,
            };
            rows.push((
                stable,
                DeviceRow {
                    name,
                    kind: device.kind,
                    state,
                    link,
                    settings: known.settings,
                    route,
                    controls,
                    unit: unit_id,
                },
            ));
        }
    }
    for device in &snapshot.standalone {
        rows.push(standalone_row(device, &snapshot.lights, config));
    }
    rows.sort_by(|(a, _), (b, _)| a.cmp(b));
    rows.into_iter().map(|(_, row)| row).collect()
}

fn standalone_row(
    device: &StandaloneDevice,
    lights: &[ObservedLight],
    config: &Config,
) -> (DeviceStableId, DeviceRow) {
    let route = device.route();
    let serial = device.serial_number.as_deref();
    let stable =
        DeviceStableId::from_parts(Some(&route), DIRECT_DEVICE_INDEX, serial, device.unit_id);
    let identity = DeviceIdentity::from_parts(serial, device.unit_id);
    let known = Known::of(config, &stable, device.online.then_some(&identity));
    let observed = lights
        .iter()
        .find(|light| light.route == route)
        .filter(|_| device.online);
    let capabilities = device
        .capabilities
        .unwrap_or_else(|| Capabilities::presumed_from_kind(device.kind));
    let mut controls = known.controls(config, device.kind, capabilities, &stable);
    controls.light = observed
        .map(|light| light.state.capabilities)
        .or(device.light_capabilities)
        .filter(|light| *light != LightCapabilities::default());
    let name = known.name.unwrap_or_else(|| device.display_name.clone());
    let state = match (device.online, observed) {
        (true, Some(light)) => DeviceState::Light(LightReading::of(light)),
        (true, None) => DeviceState::Online,
        (false, _) => DeviceState::Offline(None),
    };
    let link = Link {
        kind: ConnectionKind::of(Some(&route), None, None),
        slot: None,
    };
    let row = DeviceRow {
        name,
        kind: device.kind,
        state,
        link,
        settings: known.settings,
        route: Some(route),
        controls,
        unit: device.unit_id,
    };
    (stable, row)
}

/// What the config knows about one device.
struct Known {
    /// The key its settings live under, or would be saved under.
    key: Option<PhysicalDeviceKey>,
    /// The name the user gave it in the settings app.
    name: Option<String>,
    /// The link that opens its page in the settings app.
    settings: DeeplinkCommand,
}

impl Known {
    /// Look the device up by the key its settings live under, as the settings
    /// app does. A device the config has never seen links by the key its
    /// settings would be saved under.
    fn of(config: &Config, stable: &DeviceStableId, identity: Option<&DeviceIdentity>) -> Self {
        let key = config.resolve_device_key(stable, identity);
        let name = key
            .as_ref()
            .and_then(|key| config.device_custom_name(key.as_str()))
            .map(str::to_owned);
        let key = key.or_else(|| canonical_device_key(stable, identity));
        let settings = key
            .clone()
            .and_then(DeviceLink::new)
            .map_or(DeeplinkCommand::Show, DeeplinkCommand::OpenDevice);
        Self {
            key,
            name,
            settings,
        }
    }

    /// What a device of `kind` with `capabilities` can change from the menu,
    /// with the values the config saved for it on the link `stable` names —
    /// the values the agent writes when it reconnects.
    fn controls(
        &self,
        config: &Config,
        kind: DeviceKind,
        capabilities: Capabilities,
        stable: &DeviceStableId,
    ) -> Controls {
        let key = self.key.as_ref().map(PhysicalDeviceKey::as_str);
        let device = key.and_then(|key| config.devices.get(key));
        let route_key = stable.route_key();
        Controls {
            dpi: capabilities.pointer.then(|| DpiControl {
                presets: key.map_or_else(Default::default, |key| config.dpi_presets(key)),
                saved: device.and_then(|device| device.effective_dpi(&route_key)),
            }),
            wheel: capabilities.pointer,
            saved_wheel: device
                .and_then(|device| device.effective_smartshift(&route_key))
                .map(Into::into),
            managed: (matches!(kind, DeviceKind::Mouse | DeviceKind::Trackball)
                || kind.is_keyboard_family())
            .then(|| key.is_none_or(|key| config.device_enabled(key))),
            fn_lock: capabilities.fn_lock,
            saved_fn_lock: key.and_then(|key| config.fn_lock(key)),
            backlight: capabilities.backlight,
            disable_keys: capabilities.disable_keys,
            sidetone: capabilities.sidetone,
            light: None,
        }
    }
}

/// A webcam the agent found, as the tray scanned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenCamera {
    /// The name it reports.
    pub name: String,
    /// The key its settings live under (`camera:…`).
    pub key: String,
    /// Whether an application is capturing from it.
    pub in_use: bool,
}

/// One webcam in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraRow {
    /// The user's name for it, or the name it reports.
    pub name: String,
    /// Whether an application is capturing from it.
    pub in_use: bool,
    /// What its "Open Preview…" item opens.
    pub preview: DeeplinkCommand,
    /// What its "Open Device Settings…" item opens: its page on the tab that
    /// leaves the camera off.
    pub settings: DeeplinkCommand,
}

impl CameraRow {
    /// A row for each of `cameras`, by name.
    #[must_use]
    pub fn all(cameras: &[SeenCamera], config: &Config) -> Vec<Self> {
        let mut rows: Vec<Self> = cameras
            .iter()
            .map(|camera| Self {
                name: config
                    .device_custom_name(&camera.key)
                    .map_or_else(|| camera.name.clone(), str::to_owned),
                in_use: camera.in_use,
                preview: DeeplinkCommand::PreviewCamera(camera.key.clone()),
                settings: DeeplinkCommand::OpenCamera(camera.key.clone()),
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Whether it is in use, in words.
    #[must_use]
    pub fn state_text(&self) -> String {
        if self.in_use {
            t!("tray.camera_in_use")
        } else {
            t!("tray.camera_idle")
        }
        .into_owned()
    }
}

/// Whether an application runs with its own button bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppButtons {
    /// It has a per-app profile overriding this many buttons, over all
    /// devices.
    Own(usize),
    /// Every device uses its default bindings for it.
    Default,
}

impl AppButtons {
    /// Which buttons `app` (a [`ForegroundApp::id`]) runs with, matched the
    /// way the agent matches it: exact key, `exe:` key, or `app:` alias.
    ///
    /// [`ForegroundApp::id`]: openlogi_core::app::ForegroundApp::id
    fn of(config: &Config, app: &str) -> Self {
        let buttons: usize = config
            .devices
            .keys()
            .filter_map(|device| {
                let profile = config.app_profile_for(device, app)?;
                config.per_app_overrides(device, profile)
            })
            .map(BTreeMap::len)
            .sum();
        if buttons == 0 {
            Self::Default
        } else {
            Self::Own(buttons)
        }
    }

    /// What the app's row says on its right.
    #[must_use]
    pub fn text(self) -> Cow<'static, str> {
        match self {
            Self::Own(_) => t!("tray.own_buttons"),
            Self::Default => t!("tray.default_buttons"),
        }
    }
}

/// An application and the buttons it runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// The application's name.
    pub name: String,
    /// Which buttons it runs with.
    pub buttons: AppButtons,
    /// What opens its profile in the settings app.
    pub profile: DeeplinkCommand,
}

impl AppEntry {
    fn of(config: &Config, app: &ForegroundApp) -> Self {
        Self {
            name: app.display_name.clone(),
            buttons: AppButtons::of(config, &app.id),
            profile: DeeplinkCommand::OpenAppProfile(app.id.clone()),
        }
    }
}

/// The current-application row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRow {
    /// The application per-app profiles resolve against.
    pub current: AppEntry,
    /// The applications in front before it, newest first.
    pub recent: Vec<AppEntry>,
}

impl AppRow {
    /// The row for the application in front — or, while one of OpenLogi's own
    /// windows is, the one the user came from, as the settings app picks it.
    /// `None` when no application is in front or the platform cannot say.
    fn new(snapshot: &AgentSnapshot, config: &Config) -> Option<Self> {
        let foreground = &snapshot.foreground;
        foreground.current.as_ref()?;
        // `recent` leaves OpenLogi's own processes out, so its head is the
        // current application whenever that is not one of them.
        let (current, earlier) = foreground.recent.split_first()?;
        Some(Self {
            current: AppEntry::of(config, current),
            recent: earlier
                .iter()
                .take(RECENT_APPS_SHOWN)
                .map(|app| AppEntry::of(config, app))
                .collect(),
        })
    }

    /// The first line of its submenu: how many buttons it overrides.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.current.buttons {
            AppButtons::Own(count) => t!("tray.own_button_count", count => count).into_owned(),
            AppButtons::Default => t!("tray.uses_default_buttons").into_owned(),
        }
    }
}

/// One paired Flow computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowPeer {
    /// Its name in the Flow settings.
    pub name: String,
    /// The state of the session with it.
    pub state: FlowLinkState,
}

impl FlowPeer {
    /// Its link state in words.
    #[must_use]
    pub fn state_text(&self) -> Cow<'static, str> {
        match self.state {
            FlowLinkState::Connected => t!("flow.link_connected"),
            FlowLinkState::Degraded => t!("flow.link_unstable"),
            FlowLinkState::Lost => t!("flow.link_offline"),
        }
    }
}

/// The Flow row, shown while Flow is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRow {
    /// The paired computers, in config order.
    pub peers: Vec<FlowPeer>,
}

impl FlowRow {
    fn new(snapshot: &AgentSnapshot) -> Option<Self> {
        let flow = &snapshot.flow;
        flow.enabled.then(|| Self {
            peers: flow
                .peers
                .iter()
                .map(|peer| FlowPeer {
                    name: peer.name.as_str().to_owned(),
                    state: peer.state,
                })
                .collect(),
        })
    }

    /// What the row says on its right: the computer Flow is connected to, or
    /// that it is connected to none.
    #[must_use]
    pub fn detail(&self) -> String {
        let reachable = |state| self.peers.iter().find(|peer| peer.state == state);
        reachable(FlowLinkState::Connected)
            .or_else(|| reachable(FlowLinkState::Degraded))
            .map_or_else(
                || {
                    if self.peers.is_empty() {
                        t!("flow.status_no_peers").into_owned()
                    } else {
                        t!("tray.flow_not_connected").into_owned()
                    }
                },
                |peer| peer.name.clone(),
            )
    }
}

#[cfg(test)]
mod tests;
