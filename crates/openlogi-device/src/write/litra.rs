//! Raw HID driver for Logitech Litra lights.
//!
//! Litra is deliberately implemented beside, not inside, the HID++ feature
//! writers. The driver owns product matching, semantic-range conversion, and
//! the fixed report encoding; the generic transport only owns enumeration and
//! opening the selected raw HID node.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use openlogi_core::device::{LightCapabilities, LightValueRange, LightValueUnit};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::debug;

use crate::LOGITECH_VENDOR_ID;
use crate::backend::HidBackend;
use crate::channel::route::{DeviceRoute, open_route_writer};

use super::WriteError;

// LightCommand is pure IPC wire data with no HID++ I/O, so it lives in
// `openlogi_core::hid::light`; re-exported here unchanged so this module's
// own API surface doesn't churn.
pub use openlogi_core::hid::light::LightCommand;

/// Stable driver-family identifier carried by standalone inventory records.
pub const LITRA_DRIVER_ID: &str = "litra";
/// Litra Glow product ID.
pub const LITRA_GLOW_PRODUCT_ID: u16 = 0xc900;
/// Litra Beam product ID.
pub const LITRA_BEAM_PRODUCT_ID: u16 = 0xc901;
/// Litra vendor usage page.
pub const LITRA_USAGE_PAGE: u16 = 0xff43;
/// Litra Glow usage ID.
pub const LITRA_USAGE_ID: u16 = 0x0202;

const REPORT_LEN: usize = 20;
const REPORT_ID: u8 = 0x11;
const REPORT_PREFIX: [u8; 2] = [0xff, 0x04];
const COMMAND_POWER: u8 = 0x1c;
const COMMAND_BRIGHTNESS: u8 = 0x4c;
const COMMAND_TEMPERATURE: u8 = 0x9c;
const MIN_BRIGHTNESS_LUMENS: u16 = 20;
const MAX_BRIGHTNESS_LUMENS: u16 = 250;
const MIN_TEMPERATURE_KELVIN: u16 = 2700;
const MAX_TEMPERATURE_KELVIN: u16 = 6500;
const TEMPERATURE_STEP_KELVIN: u16 = 100;
const RAW_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

const fn validated_range(min: u16, max: u16, step: u16, unit: LightValueUnit) -> LightValueRange {
    match LightValueRange::new(min, max, step, unit) {
        Ok(range) => range,
        Err(_) => panic!("invalid static Litra capability range"),
    }
}

const GLOW_BRIGHTNESS_RANGE: LightValueRange = validated_range(
    MIN_BRIGHTNESS_LUMENS,
    MAX_BRIGHTNESS_LUMENS,
    1,
    LightValueUnit::Lumens,
);
const GLOW_TEMPERATURE_RANGE: LightValueRange = validated_range(
    MIN_TEMPERATURE_KELVIN,
    MAX_TEMPERATURE_KELVIN,
    TEMPERATURE_STEP_KELVIN,
    LightValueUnit::Kelvin,
);

/// A supported Litra product family variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LitraModel {
    /// Logitech Litra Glow.
    Glow,
    /// Logitech Litra Beam.
    Beam,
}

impl LitraModel {
    /// Resolve a Litra model from its USB product ID.
    #[must_use]
    pub const fn from_product_id(product_id: u16) -> Option<Self> {
        match product_id {
            LITRA_GLOW_PRODUCT_ID => Some(Self::Glow),
            LITRA_BEAM_PRODUCT_ID => Some(Self::Beam),
            _ => None,
        }
    }

    /// Resolve a model only when the complete raw-HID route matches a known
    /// Litra interface. Product ID alone is insufficient protection against
    /// writing a vendor report to an unrelated HID collection.
    #[must_use]
    pub fn from_route(route: &DeviceRoute) -> Option<Self> {
        let DeviceRoute::RawHid {
            vendor_id,
            product_id,
            usage_page,
            usage_id,
            ..
        } = route
        else {
            return None;
        };
        matches_litra(*vendor_id, *product_id, *usage_page, *usage_id)
            .then(|| Self::from_product_id(*product_id))
            .flatten()
    }

    /// Stable driver-family identifier for this model.
    #[must_use]
    pub const fn driver_id(self) -> &'static str {
        LITRA_DRIVER_ID
    }

    /// Exact model identifier used by the OpenLogi asset registry.
    #[must_use]
    pub const fn registry_model_id(self) -> Option<&'static str> {
        match self {
            Self::Glow => Some("8c900"),
            Self::Beam => Some("8c901"),
        }
    }

    /// Static capabilities exposed by the model.
    #[must_use]
    pub const fn capabilities(self) -> LightCapabilities {
        match self {
            Self::Glow | Self::Beam => LightCapabilities {
                power: true,
                brightness: Some(GLOW_BRIGHTNESS_RANGE),
                temperature: Some(GLOW_TEMPERATURE_RANGE),
                color: false,
                zones: false,
            },
        }
    }
}

/// Whether an HID descriptor identifies a supported Litra interface.
#[must_use]
pub fn matches_litra(vendor_id: u16, product_id: u16, usage_page: u16, usage_id: u16) -> bool {
    vendor_id == LOGITECH_VENDOR_ID
        && usage_page == LITRA_USAGE_PAGE
        && usage_id == LITRA_USAGE_ID
        && LitraModel::from_product_id(product_id).is_some()
}

/// Encode a semantic command into the exact fixed-width Litra report.
pub fn encode_command(
    model: LitraModel,
    command: LightCommand,
) -> Result<[u8; REPORT_LEN], WriteError> {
    let mut report = [0; REPORT_LEN];
    report[0] = REPORT_ID;
    report[1..3].copy_from_slice(&REPORT_PREFIX);
    match command {
        LightCommand::Power(enabled) => {
            report[3] = COMMAND_POWER;
            report[4] = u8::from(enabled);
        }
        LightCommand::BrightnessPercent(percent) => {
            report[3] = COMMAND_BRIGHTNESS;
            let range = model
                .capabilities()
                .brightness
                .ok_or_else(|| unsupported("brightness"))?;
            let lumens = percent_to_native(percent, range)?;
            report[4..6].copy_from_slice(&lumens.to_be_bytes());
        }
        LightCommand::TemperatureKelvin(kelvin) => {
            report[3] = COMMAND_TEMPERATURE;
            let range = model
                .capabilities()
                .temperature
                .ok_or_else(|| unsupported("temperature"))?;
            if !range.contains(kelvin) {
                return Err(WriteError::InvalidLightValue {
                    control: "temperature_kelvin".into(),
                    value: kelvin,
                });
            }
            report[4..6].copy_from_slice(&kelvin.to_be_bytes());
        }
        LightCommand::BrightnessNative(value) => {
            report[3] = COMMAND_BRIGHTNESS;
            let range = model
                .capabilities()
                .brightness
                .ok_or_else(|| unsupported("brightness"))?;
            if !range.contains(value) {
                return Err(WriteError::InvalidLightValue {
                    control: "brightness_native".into(),
                    value,
                });
            }
            report[4..6].copy_from_slice(&value.to_be_bytes());
        }
    }
    Ok(report)
}

fn percent_to_native(percent: u8, range: LightValueRange) -> Result<u16, WriteError> {
    if percent > 100 {
        return Err(WriteError::InvalidLightValue {
            control: "brightness_percent".into(),
            value: u16::from(percent),
        });
    }
    range
        .native_for_percent(percent)
        .ok_or_else(|| WriteError::InvalidLightValue {
            control: "brightness_percent".into(),
            value: percent.into(),
        })
}

fn unsupported(control: &str) -> WriteError {
    WriteError::LightUnsupported {
        control: control.into(),
    }
}

static DEVICE_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn device_lock(route: &DeviceRoute) -> OwnedMutexGuard<()> {
    let key = route.to_string();
    let lock = {
        let mut locks = DEVICE_LOCKS.lock().await;
        Arc::clone(locks.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
    };
    lock.lock_owned().await
}

/// Apply a semantic Litra command through a raw HID route.
pub async fn apply(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    model: LitraModel,
    command: LightCommand,
) -> Result<(), WriteError> {
    let Some(route_model) = LitraModel::from_route(route) else {
        return Err(unsupported("raw_hid_route"));
    };
    if route_model != model {
        return Err(unsupported("litra_model"));
    }
    let report = encode_command(model, command)?;
    let _guard = device_lock(route).await;
    let Some(mut writer) = open_route_writer(backend, route).await? else {
        return Err(WriteError::DeviceNotFound);
    };
    tokio::time::timeout(RAW_WRITE_TIMEOUT, writer.write_output_report(&report))
        .await
        .map_err(|_| WriteError::RequestTimedOut {
            operation: super::HidppOperation::Light,
        })??;
    debug!(route = %route, "applied raw Litra command");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{
        COMMAND_BRIGHTNESS, COMMAND_POWER, COMMAND_TEMPERATURE, LITRA_BEAM_PRODUCT_ID,
        LightCommand, LitraModel, REPORT_ID, encode_command, matches_litra,
    };
    use crate::{DeviceRoute, WriteError};

    #[test]
    fn glow_power_reports_are_fixed_width() {
        let on = encode_command(LitraModel::Glow, LightCommand::Power(true)).expect("valid");
        let off = encode_command(LitraModel::Glow, LightCommand::Power(false)).expect("valid");
        assert_eq!(&on[..5], &[REPORT_ID, 0xff, 0x04, COMMAND_POWER, 1]);
        assert_eq!(&off[..5], &[REPORT_ID, 0xff, 0x04, COMMAND_POWER, 0]);
        assert_eq!(on.len(), 20);
        assert!(on[5..].iter().all(|byte| *byte == 0));
        assert!(off[5..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn glow_brightness_uses_big_endian_native_lumens() {
        let report =
            encode_command(LitraModel::Glow, LightCommand::BrightnessPercent(50)).expect("valid");
        assert_eq!(&report[3..6], &[COMMAND_BRIGHTNESS, 0, 0x87]);
    }

    #[test]
    fn glow_brightness_maps_normalized_boundaries_to_native_range() {
        let minimum =
            encode_command(LitraModel::Glow, LightCommand::BrightnessPercent(0)).expect("valid");
        let maximum =
            encode_command(LitraModel::Glow, LightCommand::BrightnessPercent(100)).expect("valid");
        assert_eq!(&minimum[3..6], &[COMMAND_BRIGHTNESS, 0, 20]);
        assert_eq!(&maximum[3..6], &[COMMAND_BRIGHTNESS, 0, 250]);
    }

    #[test]
    fn glow_native_brightness_preserves_the_exact_requested_lumens() {
        let report =
            encode_command(LitraModel::Glow, LightCommand::BrightnessNative(136)).expect("valid");
        assert_eq!(&report[3..6], &[COMMAND_BRIGHTNESS, 0, 136]);
        assert_matches!(
            encode_command(LitraModel::Glow, LightCommand::BrightnessNative(251)),
            Err(WriteError::InvalidLightValue { .. })
        );
    }

    #[test]
    fn glow_temperature_uses_big_endian_kelvin() {
        let report =
            encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(4600)).expect("valid");
        assert_eq!(&report[3..6], &[COMMAND_TEMPERATURE, 0x11, 0xf8]);
    }

    #[test]
    fn glow_temperature_accepts_only_aligned_inclusive_boundaries() {
        let minimum = encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(2700))
            .expect("2700 K is the inclusive lower bound");
        let maximum = encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(6500))
            .expect("6500 K is the inclusive upper bound");
        assert_eq!(&minimum[3..6], &[COMMAND_TEMPERATURE, 0x0a, 0x8c]);
        assert_eq!(&maximum[3..6], &[COMMAND_TEMPERATURE, 0x19, 0x64]);
        for invalid in [2600, 2750, 6600] {
            assert_matches!(
                encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(invalid)),
                Err(WriteError::InvalidLightValue { .. })
            );
        }
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert_matches!(
            encode_command(LitraModel::Glow, LightCommand::BrightnessPercent(101)),
            Err(WriteError::InvalidLightValue { .. })
        );
        assert_matches!(
            encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(2750)),
            Err(WriteError::InvalidLightValue { .. })
        );
    }

    #[test]
    fn matcher_requires_the_full_identity_tuple() {
        assert!(matches_litra(0x046d, 0xc900, 0xff43, 0x0202));
        assert!(!matches_litra(0x046d, 0xc900, 0xff43, 0x0203));
        assert!(!matches_litra(0x046d, 0xc902, 0xff43, 0x0202));
        assert!(!matches_litra(0x1234, 0xc900, 0xff43, 0x0202));
    }

    #[test]
    fn glow_descriptor_exposes_driver_identity() {
        assert_eq!(LitraModel::Glow.driver_id(), "litra");
    }

    #[test]
    fn registry_model_ids_match_the_asset_registry() {
        assert_eq!(LitraModel::Glow.registry_model_id(), Some("8c900"));
        assert_eq!(LitraModel::Beam.registry_model_id(), Some("8c901"));
    }

    #[test]
    fn beam_is_matched_by_its_complete_route_tuple() {
        assert!(matches_litra(0x046d, 0xc901, 0xff43, 0x0202));
        assert_eq!(
            LitraModel::from_product_id(LITRA_BEAM_PRODUCT_ID),
            Some(LitraModel::Beam)
        );
    }

    #[test]
    fn model_resolution_requires_the_complete_raw_route_tuple() {
        let valid = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:test".into(),
        };
        let wrong_usage = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0203,
            identity: "serial:test".into(),
        };

        assert_eq!(LitraModel::from_route(&valid), Some(LitraModel::Glow));
        assert_eq!(LitraModel::from_route(&wrong_usage), None);
        assert_eq!(
            LitraModel::from_route(&DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc900,
            }),
            None
        );
    }
}
