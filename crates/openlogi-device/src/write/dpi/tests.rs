use std::sync::{Mutex, PoisonError};

use hidpp::feature::extended_dpi::{Lod, SensorCapabilities};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

const ADJUSTABLE_INDEX: u8 = 0x04;
const GET_DPI_LIST: u8 = 1;
const GET_DPI: u8 = 2;
const SET_DPI: u8 = 3;

const EXTENDED_INDEX: u8 = 0x05;
const GET_SENSOR_COUNT: u8 = 0;
const GET_CAPABILITIES: u8 = 1;
const GET_RANGES: u8 = 2;
const GET_PARAMETERS: u8 = 5;
const SET_PARAMETERS: u8 = 6;

/// The firmware side of a scripted DPI feature.
trait Sensor: Send + 'static {
    /// HID++ feature ID the sensor's feature lookup answers for.
    const FEATURE_ID: u16;
    /// Feature index that lookup returns.
    const INDEX: u8;

    /// The payload answering one request to [`Self::INDEX`].
    fn respond(&mut self, request: &[u8]) -> Option<[u8; 16]>;
}

/// A `0x2201` sensor that supports 800 and 1600 DPI.
#[derive(Clone, Copy)]
struct AdjustableSensor {
    current: u16,
    /// When set, every write is acknowledged but the sensor keeps this value.
    stuck_at: Option<u16>,
}

impl Sensor for AdjustableSensor {
    const FEATURE_ID: u16 = 0x2201;
    const INDEX: u8 = ADJUSTABLE_INDEX;

    fn respond(&mut self, request: &[u8]) -> Option<[u8; 16]> {
        let mut payload = [0u8; 16];
        payload[0] = request[4];
        match request[3] >> 4 {
            GET_SENSOR_COUNT => payload[0] = 1,
            GET_DPI_LIST => payload[1..7].copy_from_slice(&[0x03, 0x20, 0x06, 0x40, 0x00, 0x00]),
            GET_DPI => payload[1..3].copy_from_slice(&self.current.to_be_bytes()),
            SET_DPI => {
                self.current = self
                    .stuck_at
                    .unwrap_or_else(|| u16::from_be_bytes([request[5], request[6]]));
                payload[..3].copy_from_slice(&request[4..7]);
            }
            _ => return None,
        }
        Some(payload)
    }
}

/// A `0x2202` sensor that supports 400..=1600 DPI in steps of 400.
///
/// Its parameters read always reports a non-zero `dpiY` and a real lift-off
/// distance, whatever the capability bits say, so a write that carries Y or
/// LOD only because a read returned them is visible on the wire.
#[derive(Clone, Copy)]
struct ExtendedSensor {
    capabilities: SensorCapabilities,
    /// Sensor index echoed by the `getSensorCapabilities` reply.
    capability_echo: u8,
    dpi_x: u16,
    dpi_y: u16,
    lod: u8,
}

impl ExtendedSensor {
    fn new(capabilities: SensorCapabilities) -> Self {
        Self {
            capabilities,
            capability_echo: SENSOR,
            dpi_x: 800,
            dpi_y: 800,
            lod: u8::from(Lod::Medium),
        }
    }
}

impl Sensor for ExtendedSensor {
    const FEATURE_ID: u16 = 0x2202;
    const INDEX: u8 = EXTENDED_INDEX;

    fn respond(&mut self, request: &[u8]) -> Option<[u8; 16]> {
        let mut payload = [0u8; 16];
        match request[3] >> 4 {
            GET_SENSOR_COUNT => payload[0] = 1,
            GET_CAPABILITIES => {
                payload[0] = self.capability_echo;
                payload[2] = self.capabilities.bits();
            }
            GET_RANGES => {
                payload[..3].copy_from_slice(&request[4..7]);
                payload[3..11].copy_from_slice(&[0x01, 0x90, 0xe1, 0x90, 0x06, 0x40, 0x00, 0x00]);
            }
            GET_PARAMETERS => {
                payload[0] = request[4];
                payload[1..3].copy_from_slice(&self.dpi_x.to_be_bytes());
                payload[3..5].copy_from_slice(&800u16.to_be_bytes());
                payload[5..7].copy_from_slice(&self.dpi_y.to_be_bytes());
                payload[7..9].copy_from_slice(&800u16.to_be_bytes());
                payload[9] = self.lod;
            }
            SET_PARAMETERS => {
                self.dpi_x = u16::from_be_bytes([request[5], request[6]]);
                self.dpi_y = u16::from_be_bytes([request[7], request[8]]);
                self.lod = request[9];
                payload[..6].copy_from_slice(&request[4..10]);
            }
            _ => return None,
        }
        Some(payload)
    }
}

/// Answer the root ping and feature lookup, and hand everything addressed to
/// the sensor's feature index to the sensor.
fn respond<S: Sensor>(sensor: &mut S, request: &[u8]) -> Option<Vec<u8>> {
    let mut payload = [0u8; 16];
    match (request[2], request[3] >> 4) {
        (0x00, 0x01) => payload[0] = 4,
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = if feature_id == S::FEATURE_ID {
                S::INDEX
            } else {
                0
            };
        }
        (index, _) if index == S::INDEX => payload = sensor.respond(request)?,
        _ => return None,
    }
    let mut response = vec![0u8; 20];
    response[0] = 0x11;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload);
    Some(response)
}

struct Harness {
    shared: SharedChannel,
    written: Box<dyn Fn() -> Vec<Vec<u8>>>,
}

impl Harness {
    async fn new<S: Sensor>(sensor: S) -> Self {
        let sensor = Mutex::new(sensor);
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
                return None;
            }
            respond(
                &mut *sensor.lock().unwrap_or_else(PoisonError::into_inner),
                request,
            )
        });
        let shared = SharedChannel::new(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb034,
            },
        );
        Self {
            shared,
            written: Box::new(move || handle.written_reports()),
        }
    }

    fn requests_to(&self, index: u8, function: u8) -> Vec<Vec<u8>> {
        (self.written)()
            .into_iter()
            .filter(|report| report[2] == index && report[3] >> 4 == function)
            .collect()
    }

    /// The DPI of every `0x2201 setSensorDpi` request.
    fn dpi_writes(&self) -> Vec<u16> {
        self.requests_to(ADJUSTABLE_INDEX, SET_DPI)
            .iter()
            .map(|report| u16::from_be_bytes([report[5], report[6]]))
            .collect()
    }

    /// The `(dpiX, dpiY, lod)` of every `0x2202 setSensorDpiParameters`
    /// request.
    fn parameter_writes(&self) -> Vec<(u16, u16, u8)> {
        self.requests_to(EXTENDED_INDEX, SET_PARAMETERS)
            .iter()
            .map(|report| {
                (
                    u16::from_be_bytes([report[5], report[6]]),
                    u16::from_be_bytes([report[7], report[8]]),
                    report[9],
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn extended_writes_carry_y_and_lod_only_when_the_sensor_advertises_them()
-> Result<(), WriteError> {
    let medium = u8::from(Lod::Medium);
    let not_supported = u8::from(Lod::NotSupported);
    let cases = [
        (SensorCapabilities::empty(), 0, not_supported),
        (SensorCapabilities::DPI_Y, 1200, not_supported),
        (SensorCapabilities::LOD, 0, medium),
        (
            SensorCapabilities::DPI_Y | SensorCapabilities::LOD,
            1200,
            medium,
        ),
    ];
    for (capabilities, dpi_y, lod) in cases {
        let harness = Harness::new(ExtendedSensor::new(capabilities)).await;

        set_dpi_on(&harness.shared, Dpi::new(1200)).await?;

        assert_eq!(
            harness.parameter_writes(),
            [(1200, dpi_y, lod)],
            "{capabilities:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_capability_reply_for_another_sensor_blocks_the_write() {
    let mut sensor = ExtendedSensor::new(SensorCapabilities::DPI_Y | SensorCapabilities::LOD);
    sensor.capability_echo = 1;
    let harness = Harness::new(sensor).await;

    let result = set_dpi_on(&harness.shared, Dpi::new(1200)).await;

    assert_eq!(
        result,
        Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteDpi,
            feature_hex: 0x2202,
        })
    );
    assert!(harness.parameter_writes().is_empty());
}

#[tokio::test]
async fn a_dpi_outside_the_reported_list_is_rejected_before_any_write() {
    let unsupported = Err(WriteError::DpiUnsupported {
        requested: Dpi::new(1000),
    });

    let adjustable = Harness::new(AdjustableSensor {
        current: 800,
        stuck_at: None,
    })
    .await;
    assert_eq!(
        set_dpi_on(&adjustable.shared, Dpi::new(1000)).await,
        unsupported
    );
    assert!(adjustable.dpi_writes().is_empty());

    let extended = Harness::new(ExtendedSensor::new(SensorCapabilities::LOD)).await;
    assert_eq!(
        set_dpi_on(&extended.shared, Dpi::new(1000)).await,
        unsupported
    );
    assert!(extended.parameter_writes().is_empty());
}

#[tokio::test]
async fn a_dpi_the_device_keeps_after_one_retry_is_reported_as_not_applied() {
    let harness = Harness::new(AdjustableSensor {
        current: 800,
        stuck_at: Some(800),
    })
    .await;

    let result = set_dpi_on(&harness.shared, Dpi::new(1600)).await;

    assert_eq!(
        result,
        Err(WriteError::DpiNotApplied {
            requested: Dpi::new(1600),
            actual: Dpi::new(800),
        })
    );
    assert_eq!(harness.dpi_writes(), [1600, 1600]);
}

#[tokio::test]
async fn a_supported_dpi_the_device_applies_is_written_once() -> Result<(), WriteError> {
    let harness = Harness::new(AdjustableSensor {
        current: 800,
        stuck_at: None,
    })
    .await;

    set_dpi_on(&harness.shared, Dpi::new(1600)).await?;

    assert_eq!(harness.dpi_writes(), [1600]);
    Ok(())
}
