use std::collections::HashSet;

use openlogi_core::device::{Capabilities, DeviceInventory, LightCapabilities, StandaloneDevice};
use openlogi_core::hid::{DeviceRoute, DpiInfo, SmartShiftStatus};
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

use crate::{BacklightState, BacklightStatus, ScrollWheelMode};

/// Schema version supported by the initial profile and cassette formats.
pub const FIXTURE_SCHEMA_VERSION: u32 = 1;

/// A fixture schema, validation, or replay failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FixtureError {
    /// The asset uses a schema version this build does not understand.
    #[error("unsupported {asset} schema version {actual}; expected {supported}")]
    UnsupportedSchema {
        /// Human-readable asset kind.
        asset: &'static str,
        /// Version found in the asset.
        actual: u32,
        /// Version supported by this build.
        supported: u32,
    },
    /// The asset violates a schema invariant.
    #[error("invalid {asset}: {message}")]
    InvalidAsset {
        /// Human-readable asset kind.
        asset: &'static str,
        /// Specific failed invariant.
        message: String,
    },
    /// No pending cassette exchange matched an outgoing report.
    #[error("unmatched HID request: actual={actual}, hidpp20_normalized={normalized}")]
    UnmatchedRequest {
        /// Exact outgoing bytes as lowercase hex.
        actual: String,
        /// The same bytes with only the HID++ 2.0 software-ID nibble cleared.
        normalized: String,
    },
    /// One or more required cassette exchanges were not consumed.
    #[error("required cassette exchanges were not consumed: {requests:?}")]
    UnconsumedExchanges {
        /// Normalized request keys that remained pending.
        requests: Vec<String>,
    },
    /// A topology operation named a node that does not exist.
    #[error("unknown replay node {0}")]
    UnknownNode(String),
    /// A topology operation named a logical channel that does not exist.
    #[error("unknown replay channel {0}")]
    UnknownChannel(String),
}

impl FixtureError {
    pub(super) fn invalid(asset: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidAsset {
            asset,
            message: message.into(),
        }
    }
}

/// A semantic, host-independent snapshot of one synthetic specimen.
///
/// Raw reports and mutable topology do not belong here. The profile is the
/// independently reviewable expectation consumed by semantic mocks and tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceProfile {
    /// Profile schema version.
    pub schema_version: u32,
    /// Stable synthetic specimen identifier, never a hardware serial.
    pub id: String,
    /// Human-readable specimen name.
    pub name: String,
    /// Expected semantic device inventories.
    pub inventories: Vec<DeviceInventory>,
    /// Standalone raw-HID devices exposed beside the HID++ inventories.
    pub standalone: Vec<StandaloneDevice>,
    /// Initial setting reads and operation support, keyed by device route.
    pub settings: Vec<ProfileDeviceSettings>,
}

#[derive(Deserialize)]
struct DeviceProfileFields {
    schema_version: u32,
    id: String,
    name: String,
    inventories: Vec<DeviceInventory>,
    standalone: Vec<StandaloneDevice>,
    settings: Vec<ProfileDeviceSettings>,
}

impl<'de> Deserialize<'de> for DeviceProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut unknown = None;
        let fields: DeviceProfileFields = serde_ignored::deserialize(deserializer, |path| {
            if unknown.is_none() {
                unknown = Some(path.to_string());
            }
        })?;
        if let Some(path) = unknown {
            return Err(de::Error::custom(format!("unknown field at {path}")));
        }
        Ok(Self {
            schema_version: fields.schema_version,
            id: fields.id,
            name: fields.name,
            inventories: fields.inventories,
            standalone: fields.standalone,
            settings: fields.settings,
        })
    }
}

/// Whether a write-only profile operation is accepted by a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSupport {
    /// The route accepts this operation family.
    Supported,
    /// The route reports the operation family as unsupported.
    Unsupported,
}

impl ProfileSupport {
    /// Whether this behavior accepts the operation.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Initial behavior of one typed setting read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "support",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProfileSetting<T> {
    /// The feature is absent and reads/writes return the typed unsupported error.
    Unsupported,
    /// The feature is present with this initial value.
    Supported(
        /// Initial semantic read result.
        T,
    ),
    /// The feature is present, but no semantic value is currently available.
    Unavailable,
}

impl<T> ProfileSetting<T> {
    /// Whether the feature is present, independently of value availability.
    #[must_use]
    pub const fn supports_feature(&self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    /// Borrow the supported value, if present.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Unsupported | Self::Unavailable => None,
            Self::Supported(value) => Some(value),
        }
    }

    /// Mutably borrow the supported value, if present.
    #[must_use]
    pub const fn value_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Unsupported | Self::Unavailable => None,
            Self::Supported(value) => Some(value),
        }
    }
}

/// Route-keyed setting behavior used by semantic mocks and tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDeviceSettings {
    /// Device whose operations these behaviors describe.
    pub route: DeviceRoute,
    /// Adjustable-DPI read state.
    pub dpi: ProfileSetting<DpiInfo>,
    /// SmartShift read state.
    pub smartshift: ProfileSetting<SmartShiftStatus>,
    /// HiResWheel read state.
    pub wheel: ProfileSetting<ScrollWheelMode>,
    /// Keyboard-backlight read state.
    pub backlight: ProfileSetting<BacklightState>,
    /// RGB keyboard-lighting write support.
    pub lighting: ProfileSupport,
    /// Standalone-light command support.
    pub light: ProfileSupport,
}

struct ProfileRouteFacts {
    route: DeviceRoute,
    capabilities: Option<Capabilities>,
    standalone: bool,
    light_capabilities: Option<LightCapabilities>,
}

#[derive(Default)]
struct ProfileValidation {
    receiver_identities: HashSet<String>,
    direct_identities: HashSet<(u16, u16)>,
    standalone_identities: HashSet<String>,
    unit_ids: HashSet<[u8; 4]>,
    routes: Vec<ProfileRouteFacts>,
}

impl DeviceProfile {
    /// Validate the schema version, identities, routes, values, and capability consistency.
    pub fn validate(&self) -> Result<(), FixtureError> {
        validate_version("device profile", self.schema_version)?;
        validate_name("device profile", "id", &self.id)?;
        validate_name("device profile", "name", &self.name)?;
        if self.inventories.is_empty() && self.standalone.is_empty() {
            return Err(FixtureError::invalid(
                "device profile",
                "at least one inventory or standalone device is required",
            ));
        }

        let mut validation = ProfileValidation::default();
        for inventory in &self.inventories {
            validation.add_inventory(inventory)?;
        }
        for device in &self.standalone {
            validation.add_standalone(device)?;
        }
        validate_setting_routes(&self.settings, &validation.routes)
    }
}

impl ProfileValidation {
    fn add_inventory(&mut self, inventory: &DeviceInventory) -> Result<(), FixtureError> {
        validate_name(
            "device profile",
            "inventory receiver name",
            &inventory.receiver.name,
        )?;
        self.validate_paired_devices(inventory)?;
        self.validate_inventory_identity(inventory)?;
        for device in &inventory.paired {
            let Some(route) = DeviceRoute::device_route_for(inventory, device.slot) else {
                return Err(FixtureError::invalid(
                    "device profile",
                    format!(
                        "slot {} on {} has no addressable route",
                        device.slot, inventory.receiver.name
                    ),
                ));
            };
            self.push_route(ProfileRouteFacts {
                route,
                capabilities: device.capabilities,
                standalone: false,
                light_capabilities: None,
            })?;
        }
        Ok(())
    }

    fn validate_paired_devices(&mut self, inventory: &DeviceInventory) -> Result<(), FixtureError> {
        let mut slots = HashSet::new();
        for device in &inventory.paired {
            if !slots.insert(device.slot) {
                return Err(FixtureError::invalid(
                    "device profile",
                    format!(
                        "receiver {:04x}:{:04x} repeats slot {}",
                        inventory.receiver.vendor_id, inventory.receiver.product_id, device.slot
                    ),
                ));
            }
            validate_battery(
                device.battery.as_ref(),
                &inventory.receiver.name,
                device.slot,
            )?;
            if let Some(model) = &device.model_info {
                let owner = format!("slot {} on {}", device.slot, inventory.receiver.name);
                validate_unit_id(&mut self.unit_ids, model.unit_id, &owner)?;
            }
        }
        Ok(())
    }

    fn validate_inventory_identity(
        &mut self,
        inventory: &DeviceInventory,
    ) -> Result<(), FixtureError> {
        if let Some(receiver_uid) = inventory.receiver.unique_id.as_deref() {
            validate_name("device profile", "receiver unique_id", receiver_uid)?;
            if !self
                .receiver_identities
                .insert(receiver_uid.to_ascii_lowercase())
            {
                return Err(FixtureError::invalid(
                    "device profile",
                    format!("receiver identity {receiver_uid} is repeated"),
                ));
            }
            if let Some(device) = inventory
                .paired
                .iter()
                .find(|device| !(1..=6).contains(&device.slot))
            {
                return Err(FixtureError::invalid(
                    "device profile",
                    format!(
                        "receiver {receiver_uid} has invalid pairing slot {}",
                        device.slot
                    ),
                ));
            }
            return Ok(());
        }

        if inventory.paired.len() != 1 || inventory.paired[0].slot != crate::DIRECT_DEVICE_INDEX {
            return Err(FixtureError::invalid(
                "device profile",
                format!(
                    "direct inventory {:04x}:{:04x} must contain exactly one device at index 0xff",
                    inventory.receiver.vendor_id, inventory.receiver.product_id
                ),
            ));
        }
        let identity = (inventory.receiver.vendor_id, inventory.receiver.product_id);
        if !self.direct_identities.insert(identity) {
            return Err(FixtureError::invalid(
                "device profile",
                format!(
                    "direct identity {:04x}:{:04x} is repeated",
                    identity.0, identity.1
                ),
            ));
        }
        Ok(())
    }

    fn add_standalone(&mut self, device: &StandaloneDevice) -> Result<(), FixtureError> {
        validate_name(
            "device profile",
            "standalone display_name",
            &device.display_name,
        )?;
        validate_name("device profile", "standalone driver_id", &device.driver_id)?;
        validate_name(
            "device profile",
            "standalone address identity",
            &device.address.identity,
        )?;
        if !self
            .standalone_identities
            .insert(device.address.identity.clone())
        {
            return Err(FixtureError::invalid(
                "device profile",
                format!(
                    "standalone identity {} is repeated",
                    device.address.identity
                ),
            ));
        }
        let owner = format!("standalone device {}", device.display_name);
        validate_unit_id(&mut self.unit_ids, device.unit_id, &owner)?;
        self.push_route(ProfileRouteFacts {
            route: DeviceRoute::RawHid {
                vendor_id: device.address.vendor_id,
                product_id: device.address.product_id,
                usage_page: device.address.usage_page,
                usage_id: device.address.usage_id,
                identity: device.address.identity.clone(),
            },
            capabilities: device.capabilities,
            standalone: true,
            light_capabilities: device.light_capabilities,
        })
    }

    fn push_route(&mut self, facts: ProfileRouteFacts) -> Result<(), FixtureError> {
        if self.routes.iter().any(|known| known.route == facts.route) {
            Err(FixtureError::invalid(
                "device profile",
                format!("device route {} is repeated", facts.route),
            ))
        } else {
            self.routes.push(facts);
            Ok(())
        }
    }
}

fn validate_setting_routes(
    settings: &[ProfileDeviceSettings],
    routes: &[ProfileRouteFacts],
) -> Result<(), FixtureError> {
    let mut setting_routes = Vec::new();
    for settings in settings {
        if setting_routes.contains(&&settings.route) {
            return Err(FixtureError::invalid(
                "device profile",
                format!("repeats settings for route {}", settings.route),
            ));
        }
        setting_routes.push(&settings.route);
        let Some(facts) = routes.iter().find(|facts| facts.route == settings.route) else {
            return Err(FixtureError::invalid(
                "device profile",
                format!("settings route {} does not exist", settings.route),
            ));
        };
        validate_settings(settings, facts)?;
    }
    for facts in routes {
        if !setting_routes.contains(&&facts.route) {
            return Err(FixtureError::invalid(
                "device profile",
                format!("route {} has no settings behavior", facts.route),
            ));
        }
    }
    Ok(())
}

fn validate_unit_id(
    unit_ids: &mut HashSet<[u8; 4]>,
    unit_id: [u8; 4],
    owner: &str,
) -> Result<(), FixtureError> {
    if unit_id != [0; 4] && !unit_ids.insert(unit_id) {
        Err(FixtureError::invalid(
            "device profile",
            format!("{owner} repeats unit identity {unit_id:02x?}"),
        ))
    } else {
        Ok(())
    }
}

fn validate_battery(
    battery: Option<&openlogi_core::device::BatteryInfo>,
    receiver: &str,
    slot: u8,
) -> Result<(), FixtureError> {
    if battery.is_some_and(|battery| battery.percentage > 100) {
        Err(FixtureError::invalid(
            "device profile",
            format!("slot {slot} on {receiver} has battery percentage above 100"),
        ))
    } else {
        Ok(())
    }
}

fn validate_settings(
    settings: &ProfileDeviceSettings,
    facts: &ProfileRouteFacts,
) -> Result<(), FixtureError> {
    validate_setting_values(settings)?;
    validate_setting_support(settings, facts)
}

fn validate_setting_values(settings: &ProfileDeviceSettings) -> Result<(), FixtureError> {
    if let Some(dpi) = settings.dpi.value() {
        validate_dpi(&settings.route, dpi)?;
    }
    if let Some(backlight) = settings.backlight.value() {
        validate_backlight(&settings.route, *backlight)?;
    }
    if settings.backlight.supports_feature() && settings.lighting.is_supported() {
        return Err(FixtureError::invalid(
            "device profile",
            format!(
                "route {} cannot expose RGB lighting and backlight together",
                settings.route
            ),
        ));
    }
    Ok(())
}

fn validate_dpi(route: &DeviceRoute, dpi: &DpiInfo) -> Result<(), FixtureError> {
    let values = dpi.capabilities.values();
    if values.is_empty() {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} has an empty DPI capability list"),
        ));
    }
    if values.iter().any(|dpi| dpi.into_inner() == 0) {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} has a zero DPI capability"),
        ));
    }
    if values
        .array_windows::<2>()
        .any(|[left, right]| left >= right)
    {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} DPI capabilities must be sorted and unique"),
        ));
    }
    if !dpi.capabilities.contains(dpi.current) {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} current DPI {} is not supported", dpi.current),
        ));
    }
    Ok(())
}

fn validate_backlight(route: &DeviceRoute, backlight: BacklightState) -> Result<(), FixtureError> {
    if backlight.nb_levels == 0 {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} backlight has no levels"),
        ));
    }
    if backlight.current_level >= backlight.nb_levels {
        return Err(FixtureError::invalid(
            "device profile",
            format!(
                "route {route} backlight level {} exceeds its {} levels",
                backlight.current_level, backlight.nb_levels
            ),
        ));
    }
    let software_disabled = backlight.status == BacklightStatus::DisabledBySoftware;
    if backlight.enabled == software_disabled {
        return Err(FixtureError::invalid(
            "device profile",
            format!("route {route} backlight enabled state contradicts its status"),
        ));
    }
    Ok(())
}

fn validate_setting_support(
    settings: &ProfileDeviceSettings,
    facts: &ProfileRouteFacts,
) -> Result<(), FixtureError> {
    if facts.standalone {
        if settings.dpi.supports_feature()
            || settings.smartshift.supports_feature()
            || settings.wheel.supports_feature()
            || settings.backlight.supports_feature()
            || settings.lighting.is_supported()
        {
            return Err(FixtureError::invalid(
                "device profile",
                format!(
                    "standalone route {} declares a HID++ setting",
                    settings.route
                ),
            ));
        }
        let light_supported = facts.light_capabilities.is_some_and(|capabilities| {
            capabilities.power
                || capabilities.brightness.is_some()
                || capabilities.temperature.is_some()
        });
        if settings.light.is_supported() != light_supported {
            return Err(FixtureError::invalid(
                "device profile",
                format!(
                    "route {} light support does not match its capabilities",
                    settings.route
                ),
            ));
        }
    } else {
        if settings.light.is_supported() {
            return Err(FixtureError::invalid(
                "device profile",
                format!("HID++ route {} declares raw-light support", settings.route),
            ));
        }
        if let Some(capabilities) = facts.capabilities {
            validate_capability_support(
                settings,
                "DPI",
                settings.dpi.supports_feature(),
                capabilities.pointer,
            )?;
            validate_capability_support(
                settings,
                "wheel",
                settings.wheel.supports_feature(),
                capabilities.hires_wheel,
            )?;
            validate_capability_support(
                settings,
                "lighting",
                settings.lighting.is_supported(),
                capabilities.lighting,
            )?;
        }
    }
    Ok(())
}

fn validate_capability_support(
    settings: &ProfileDeviceSettings,
    family: &str,
    declared: bool,
    advertised: bool,
) -> Result<(), FixtureError> {
    if declared == advertised {
        Ok(())
    } else {
        Err(FixtureError::invalid(
            "device profile",
            format!(
                "route {} {family} support does not match inventory capabilities",
                settings.route
            ),
        ))
    }
}

/// HID report widths exposed by one logical replay channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportSupport {
    /// Both seven-byte (`0x10`) and twenty-byte (`0x11`) HID++ reports.
    ShortAndLong,
    /// Only twenty-byte (`0x11`) HID++ reports; short requests are widened by
    /// the production channel before reaching replay.
    LongOnly,
}

impl ReportSupport {
    pub(super) const fn flags(self) -> (bool, bool) {
        match self {
            Self::ShortAndLong => (true, true),
            Self::LongOnly => (false, true),
        }
    }
}

/// How one outgoing cassette request is keyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMatch {
    /// Match every request byte exactly. HID++ 1.0 exchanges use this mode so
    /// register address and correlation fields remain protocol-specific.
    Exact,
    /// Match a HID++ 2.0 report after clearing only byte 3's software-ID low
    /// nibble. No arbitrary masks are part of the schema.
    Hidpp20,
}

/// One required or optional request/response exchange in a raw HID cassette.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CassetteExchange {
    /// Matching rule for the outgoing request.
    pub request_match: RequestMatch,
    /// Exact outgoing report, including report ID; serde uses lowercase hex.
    #[serde(with = "hex_report")]
    pub request: Vec<u8>,
    /// Incoming report as lowercase hex, or `None` for a matched write.
    #[serde(default, with = "optional_hex_report")]
    pub response: Option<Vec<u8>>,
    /// Whether completion fails if this exchange remains unused.
    pub required: bool,
}

/// A named raw-HID operation captured on one logical channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidCassette {
    /// Cassette schema version.
    pub schema_version: u32,
    /// Human-readable operation name.
    pub name: String,
    /// Logical channel identifier referenced by replay topology.
    pub channel: String,
    /// HID++ report widths exposed by the recorded channel.
    pub report_support: ReportSupport,
    /// Request-keyed exchanges. Repeated keys are consumed FIFO.
    pub exchanges: Vec<CassetteExchange>,
}

impl HidCassette {
    /// Validate schema version, report framing, and HID++ 2.0 correlation.
    pub fn validate(&self) -> Result<(), FixtureError> {
        validate_version("HID cassette", self.schema_version)?;
        validate_name("HID cassette", "name", &self.name)?;
        validate_name("HID cassette", "channel", &self.channel)?;
        if self.exchanges.is_empty() {
            return Err(FixtureError::invalid(
                "HID cassette",
                "exchanges must not be empty",
            ));
        }
        for (index, exchange) in self.exchanges.iter().enumerate() {
            validate_report(&exchange.request, self.report_support).map_err(|message| {
                FixtureError::invalid(
                    "HID cassette",
                    format!("exchange {index} request {message}"),
                )
            })?;
            if let Some(response) = &exchange.response {
                validate_report(response, self.report_support).map_err(|message| {
                    FixtureError::invalid(
                        "HID cassette",
                        format!("exchange {index} response {message}"),
                    )
                })?;
            }
            if exchange.request_match == RequestMatch::Hidpp20 {
                validate_hidpp20(exchange, index)?;
            }
        }
        Ok(())
    }
}

fn validate_version(asset: &'static str, actual: u32) -> Result<(), FixtureError> {
    if actual == FIXTURE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(FixtureError::UnsupportedSchema {
            asset,
            actual,
            supported: FIXTURE_SCHEMA_VERSION,
        })
    }
}

fn validate_name(asset: &'static str, field: &str, value: &str) -> Result<(), FixtureError> {
    if value.trim().is_empty() {
        Err(FixtureError::invalid(
            asset,
            format!("{field} must not be empty"),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_report(report: &[u8], support: ReportSupport) -> Result<(), String> {
    let expected = match report.first() {
        Some(0x10) => 7,
        Some(0x11) => 20,
        Some(0x12) => 64,
        Some(id) => return Err(format!("uses unsupported report id 0x{id:02x}")),
        None => return Err("is empty".to_string()),
    };
    if report.len() != expected {
        return Err(format!(
            "has length {}, expected {expected} for report id 0x{:02x}",
            report.len(),
            report[0]
        ));
    }
    if support == ReportSupport::LongOnly && report[0] == 0x10 {
        return Err("uses a short report on a long-only channel".to_string());
    }
    Ok(())
}

fn validate_hidpp20(exchange: &CassetteExchange, index: usize) -> Result<(), FixtureError> {
    let request = &exchange.request;
    if !matches!(request[0], 0x10 | 0x11) {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} applies HID++ 2.0 matching to a non-HID++ report"),
        ));
    }
    let Some(response) = exchange.response.as_deref() else {
        return Ok(());
    };
    if !matches!(response[0], 0x10 | 0x11) {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} has a non-HID++ 2.0 response"),
        ));
    }
    if response[1] != request[1] {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} response changes the device index"),
        ));
    }
    let correlated = if response[2] == 0xff {
        response[3] == request[2] && response[4] & 0xf0 == request[3] & 0xf0
    } else {
        response[2] == request[2] && response[3] & 0xf0 == request[3] & 0xf0
    };
    if !correlated {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} response is not correlated to its request"),
        ));
    }
    Ok(())
}

pub(super) fn normalize_hidpp20(request: &[u8]) -> Vec<u8> {
    let mut normalized = request.to_vec();
    if normalized.len() >= 4 && matches!(normalized[0], 0x10 | 0x11) {
        normalized[3] &= 0xf0;
    }
    normalized
}

pub(super) fn format_hex(report: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut formatted = String::with_capacity(report.len() * 2);
    for &byte in report {
        formatted.push(char::from(HEX[usize::from(byte >> 4)]));
        formatted.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    formatted
}

fn parse_hex(encoded: &str) -> Result<Vec<u8>, &'static str> {
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("hex report must contain an even number of digits");
    }
    pairs
        .iter()
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok(high << 4 | low)
        })
        .collect()
}

fn hex_digit(digit: u8) -> Result<u8, &'static str> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err("hex report must use lowercase hexadecimal digits"),
    }
}

mod hex_report {
    use serde::{Deserialize, Deserializer, Serializer, de};

    use super::{format_hex, parse_hex};

    pub fn serialize<S>(report: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_hex(report))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        parse_hex(&encoded).map_err(de::Error::custom)
    }
}

mod optional_hex_report {
    use serde::{Deserialize, Deserializer, Serializer, de};

    use super::{format_hex, parse_hex};

    #[expect(
        clippy::ref_option,
        reason = "serde field serializers must receive the field by reference"
    )]
    pub fn serialize<S>(report: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match report {
            Some(report) => serializer.serialize_some(&format_hex(report)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| parse_hex(&encoded).map_err(de::Error::custom))
            .transpose()
    }
}
