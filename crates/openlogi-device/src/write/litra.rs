//! Driver for Logitech Litra lights.
//!
//! Litra is deliberately implemented beside, not inside, the HID++ feature
//! writers. The shared device registry owns product matching; this driver owns
//! semantic-range conversion and the two ways of reaching the light, while the
//! generic transport only owns enumeration and opening the selected HID node.
//!
//! A command first goes to HID++ `0x1990 Illumination` on the light's node,
//! resolved through the root feature and read back ([`illumination`]). A light
//! that does not answer there gets the fixed-index raw report the driver has
//! always sent, which carries no read-back; that answer is remembered for the
//! node, so later commands go straight to the raw report.
//!
//! A light that answers HID++ stays open ([`link`]): commands reuse the
//! channel, and [`watch`] reports changes made on the light itself.

mod illumination;
mod link;

#[cfg(test)]
mod hidpp_tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, PoisonError};
use std::time::Duration;

use openlogi_core::device::{LightCapabilities, LightState, LightValueRange, LightValueUnit};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info};

use crate::backend::{HidBackend, NodeId, NodeInfo};
use crate::channel::route::{DeviceRoute, find_raw_route_node};

use self::illumination::RESOLVE_TIMEOUT;
use self::link::{LinkKey, LiveLink};
use super::WriteError;

pub use self::link::{LightChange, LightControls, LightWatch};

// LightCommand is pure IPC wire data with no HID++ I/O, so it lives in
// `openlogi_core::hid::light`; the Litra writes name it through here.
pub use openlogi_core::hid::light::LightCommand;
pub use openlogi_device_registry::litra::{
    LITRA_BEAM_PRODUCT_ID, LITRA_GLOW_PRODUCT_ID, LitraDescriptor, LitraModel, find_litra,
    matches_litra,
};

const REPORT_LEN: usize = 20;
const REPORT_ID: u8 = 0x11;
const REPORT_PREFIX: [u8; 2] = [0xff, 0x04];
const COMMAND_POWER: u8 = 0x1c;
const COMMAND_BRIGHTNESS: u8 = 0x4c;
const COMMAND_TEMPERATURE: u8 = 0x9c;
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

// Native brightness per model, as litra-rs (`timrogers/litra-rs`, `src/lib.rs`)
// bounds it: the Beam is a brighter panel than the Glow, so the two cannot share
// one range — percent mapping and native-value checks both read it.
const GLOW_BRIGHTNESS_RANGE: LightValueRange = validated_range(20, 250, 1, LightValueUnit::Lumens);
const BEAM_BRIGHTNESS_RANGE: LightValueRange = validated_range(30, 400, 1, LightValueUnit::Lumens);
const TEMPERATURE_RANGE: LightValueRange = validated_range(
    MIN_TEMPERATURE_KELVIN,
    MAX_TEMPERATURE_KELVIN,
    TEMPERATURE_STEP_KELVIN,
    LightValueUnit::Kelvin,
);

/// Resolve a model only when the complete raw-HID route matches a registered
/// Litra interface.
#[must_use]
pub fn litra_model_for_route(route: &DeviceRoute) -> Option<LitraModel> {
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
    find_litra(*vendor_id, *product_id, *usage_page, *usage_id).map(|device| device.model)
}

pub(crate) const fn litra_capabilities(model: LitraModel) -> LightCapabilities {
    let brightness = match model {
        LitraModel::Glow => GLOW_BRIGHTNESS_RANGE,
        LitraModel::Beam => BEAM_BRIGHTNESS_RANGE,
    };
    LightCapabilities {
        power: true,
        brightness: Some(brightness),
        temperature: Some(TEMPERATURE_RANGE),
        color: false,
        zones: false,
    }
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
            let range = litra_capabilities(model)
                .brightness
                .ok_or_else(|| unsupported("brightness"))?;
            let lumens = percent_to_native(percent, range)?;
            report[4..6].copy_from_slice(&lumens.to_be_bytes());
        }
        LightCommand::TemperatureKelvin(kelvin) => {
            report[3] = COMMAND_TEMPERATURE;
            let range = litra_capabilities(model)
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
            let range = litra_capabilities(model)
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

/// Litra nodes that did not answer HID++ `0x1990`, by route and OS node.
///
/// Asking again would hold every command for up to [`RESOLVE_TIMEOUT`]. The
/// answer belongs to the OS node, so a light that comes back as a different
/// node is asked again; it lasts until the process exits otherwise.
static RAW_ONLY: LazyLock<std::sync::Mutex<HashSet<(String, NodeId)>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// How the light on a route is reached.
enum Link {
    /// HID++ `0x1990`, with read-back; `state` was just read.
    Illumination {
        link: Arc<LiveLink>,
        state: LightState,
    },
    /// Only the fixed-index raw report, on this node. `refused` holds the
    /// error when the backend refused the HID++ open this time only.
    Raw {
        node: NodeInfo,
        refused: Option<crate::backend::BackendError>,
    },
}

/// Find the light `route` addresses and work out how to reach it.
///
/// Opening the node as HID++ can fail for reasons that say nothing about
/// the light — the backend refused it this time — so that failure falls back
/// for this command only. A light that opened but did not answer the root
/// ping, list `0x1990` or have its state read is remembered in [`RAW_ONLY`].
///
/// A link kept from an earlier command is read again before it is used; one
/// that no longer answers is retired and the light is opened afresh.
async fn connect(backend: &dyn HidBackend, route: &DeviceRoute) -> Result<Link, WriteError> {
    let Some(node) = find_raw_route_node(backend, route).await? else {
        return Err(WriteError::DeviceNotFound);
    };
    let key: LinkKey = (route.to_string(), node.id.clone());
    if RAW_ONLY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&key)
    {
        return Ok(Link::Raw {
            node,
            refused: None,
        });
    }
    if let Some(link) = link::kept(&key) {
        match tokio::time::timeout(RESOLVE_TIMEOUT, link.light.read_state()).await {
            Ok(Ok(state)) => return Ok(Link::Illumination { link, state }),
            Ok(Err(error)) => {
                debug!(route = %route, %error, "the open Litra link failed — opening the light again");
            }
            Err(_elapsed) => {
                debug!(route = %route, "the open Litra link did not answer — opening the light again");
            }
        }
        link::retire(&key);
    }
    let channel = match backend.open_hidpp(&node).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            remember_raw_only(key, route, &"no HID++ collection");
            return Ok(Link::Raw {
                node,
                refused: None,
            });
        }
        Err(error) => {
            debug!(route = %route, %error, "could not open the Litra as HID++ — sending its raw report");
            return Ok(Link::Raw {
                node,
                refused: Some(error),
            });
        }
    };
    match tokio::time::timeout(RESOLVE_TIMEOUT, illumination::resolve(channel)).await {
        Ok(Ok((light, state))) => Ok(Link::Illumination {
            link: link::keep(key, light, state),
            state,
        }),
        Ok(Err(unresolved)) => {
            remember_raw_only(key, route, &unresolved);
            Ok(Link::Raw {
                node,
                refused: None,
            })
        }
        Err(_elapsed) => {
            remember_raw_only(key, route, &"no answer");
            Ok(Link::Raw {
                node,
                refused: None,
            })
        }
    }
}

fn remember_raw_only(key: (String, NodeId), route: &DeviceRoute, reason: &dyn std::fmt::Display) {
    info!(route = %route, %reason, "Litra does not answer HID++ 0x1990 — using its fixed raw report");
    RAW_ONLY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key);
}

/// Whether `route` names `model`'s registered Litra interface.
fn check_route(route: &DeviceRoute, model: LitraModel) -> Result<(), WriteError> {
    let Some(route_model) = litra_model_for_route(route) else {
        return Err(unsupported("raw_hid_route"));
    };
    if route_model != model {
        return Err(unsupported("litra_model"));
    }
    Ok(())
}

/// Apply a semantic Litra command to the light on `route`.
///
/// Returns the state the light reports afterwards, or `None` when it is
/// reached only through the raw report, which reads nothing back.
pub async fn apply(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    model: LitraModel,
    command: LightCommand,
) -> Result<Option<LightState>, WriteError> {
    check_route(route, model)?;
    let _guard = device_lock(route).await;
    let node = match connect(backend, route).await? {
        Link::Illumination { link, state } => {
            // The command reached the light over HID++ from here on, so a
            // failure is returned rather than sent again as a raw report.
            let state = link.light.apply(state, command).await?;
            link.note_written(command, &state);
            debug!(route = %route, ?state, "applied Litra command over HID++ 0x1990");
            return Ok(Some(state));
        }
        Link::Raw { node, .. } => node,
    };
    let report = encode_command(model, command)?;
    let mut writer = backend.open_raw_writer(&node).await?;
    tokio::time::timeout(RAW_WRITE_TIMEOUT, writer.write_output_report(&report))
        .await
        .map_err(|_| WriteError::RequestTimedOut {
            operation: super::HidppOperation::Light,
        })??;
    debug!(route = %route, "applied raw Litra command");
    Ok(None)
}

/// Read the state of the light on `route`, or `None` when it is reached only
/// through the raw report, which reads nothing back.
pub async fn read_state(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    model: LitraModel,
) -> Result<Option<LightState>, WriteError> {
    check_route(route, model)?;
    let _guard = device_lock(route).await;
    match connect(backend, route).await? {
        Link::Illumination { state, .. } => Ok(Some(state)),
        Link::Raw { .. } => Ok(None),
    }
}

/// Watch the light on `route` for changes made on the light itself.
///
/// Returns `None` for a light reached only through the raw report, which
/// announces nothing. A HID++ open the backend refused this time is an
/// error, so the caller can try again later.
pub async fn watch(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    model: LitraModel,
) -> Result<Option<LightWatch>, WriteError> {
    check_route(route, model)?;
    let _guard = device_lock(route).await;
    match connect(backend, route).await? {
        Link::Illumination { link, state } => Ok(Some(LightWatch::new(route.clone(), link, state))),
        Link::Raw {
            refused: Some(error),
            ..
        } => Err(error.into()),
        Link::Raw { refused: None, .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{
        COMMAND_BRIGHTNESS, COMMAND_POWER, COMMAND_TEMPERATURE, LightCommand, LitraModel,
        REPORT_ID, encode_command, litra_capabilities, litra_model_for_route,
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

    /// Beam is brighter than Glow: 30–400 lm (litra-rs `src/lib.rs`), so the
    /// percent mapping and the native check must use its own range.
    #[test]
    fn beam_brightness_uses_its_own_native_range() {
        let range = litra_capabilities(LitraModel::Beam)
            .brightness
            .expect("Beam advertises brightness");
        assert_eq!((range.min(), range.max()), (30, 400));

        let minimum =
            encode_command(LitraModel::Beam, LightCommand::BrightnessPercent(0)).expect("valid");
        let maximum =
            encode_command(LitraModel::Beam, LightCommand::BrightnessPercent(100)).expect("valid");
        assert_eq!(&minimum[3..6], &[COMMAND_BRIGHTNESS, 0, 30]);
        assert_eq!(&maximum[3..6], &[COMMAND_BRIGHTNESS, 0x01, 0x90]);

        for accepted in [30, 251, 400] {
            let report = encode_command(LitraModel::Beam, LightCommand::BrightnessNative(accepted))
                .expect("inside the Beam range");
            assert_eq!(&report[4..6], &accepted.to_be_bytes());
        }
        for rejected in [29, 401] {
            assert_matches!(
                encode_command(LitraModel::Beam, LightCommand::BrightnessNative(rejected)),
                Err(WriteError::InvalidLightValue { .. })
            );
        }
    }

    #[test]
    fn beam_shares_the_glow_temperature_range() {
        assert_eq!(
            litra_capabilities(LitraModel::Beam).temperature,
            litra_capabilities(LitraModel::Glow).temperature
        );
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

        assert_eq!(litra_model_for_route(&valid), Some(LitraModel::Glow));
        assert_eq!(litra_model_for_route(&wrong_usage), None);
        assert_eq!(
            litra_model_for_route(&DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc900,
            }),
            None
        );
    }
}
