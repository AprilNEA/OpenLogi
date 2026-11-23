//! Tests for the `0x1F20` power-off timeout reads and writes.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::HidppFeatureErrorKind;

/// The `0x1F20` feature index on a G933.
const ADC_INDEX: u8 = 0x08;
/// HID++ 2.0 `InvalidFunctionId`, what a device without functions `1`/`2`
/// answers.
const INVALID_FUNCTION_ID: u8 = 0x07;
/// HID++ 2.0 `HwError`.
const HW_ERROR: u8 = 0x04;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;
/// HID++ 2.0 `Unsupported`.
const UNSUPPORTED: u8 = 0x09;

/// A G933 as measured: it echoes each write and reads the new timeout back.
struct Headset {
    /// The stored timeout in minutes.
    minutes: u8,
    /// The error code function `1` answers instead of the timeout, for a
    /// device that lists `0x1F20` for its battery only.
    read_error: Option<u8>,
    /// Every timeout written.
    writes: Vec<u8>,
}

impl Headset {
    fn g933(minutes: u8) -> Self {
        Self {
            minutes,
            read_error: None,
            writes: Vec::new(),
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x1f20 {
                    payload[0] = ADC_INDEX;
                }
            }
            (ADC_INDEX, 0x01) => {
                if let Some(error) = self.read_error {
                    return Some(feature_error(request, error));
                }
                payload[0] = self.minutes;
            }
            (ADC_INDEX, 0x02) => {
                self.writes.push(request[4]);
                self.minutes = request[4];
                payload[0] = request[4];
            }
            _ => return None,
        }
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

async fn headset_channel(headset: &Arc<Mutex<Headset>>) -> SharedChannel {
    let headset = Arc::clone(headset);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        headset.lock().expect("headset lock").respond(request)
    });
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0x0a5b,
    };
    SharedChannel::new(scripted_channel(raw).await, route)
}

fn minutes(raw: u8) -> PowerOffTimeout {
    PowerOffTimeout::try_new(raw).expect("a timeout in range")
}

#[tokio::test]
async fn reads_the_timeout_the_headset_reports() {
    let headset = Arc::new(Mutex::new(Headset::g933(15)));
    let shared = headset_channel(&headset).await;

    assert_eq!(
        get_power_off_timeout_on(&shared).await.unwrap(),
        minutes(15)
    );
    assert!(
        headset.lock().unwrap().writes.is_empty(),
        "a read writes nothing"
    );
}

#[tokio::test]
async fn a_write_returns_the_echo_and_reads_back() {
    let headset = Arc::new(Mutex::new(Headset::g933(15)));
    let shared = headset_channel(&headset).await;

    assert_eq!(
        set_power_off_timeout_on(&shared, minutes(30))
            .await
            .unwrap(),
        minutes(30)
    );
    assert_eq!(
        get_power_off_timeout_on(&shared).await.unwrap(),
        minutes(30)
    );
    assert_eq!(headset.lock().unwrap().writes, [30]);
}

#[tokio::test]
async fn never_is_written_as_zero() {
    let headset = Arc::new(Mutex::new(Headset::g933(15)));
    let shared = headset_channel(&headset).await;

    assert_eq!(
        set_power_off_timeout_on(&shared, PowerOffTimeout::NEVER)
            .await
            .unwrap(),
        PowerOffTimeout::NEVER
    );
    assert_eq!(headset.lock().unwrap().writes, [0]);
}

#[tokio::test]
async fn a_refused_read_means_the_device_does_not_support_the_timeout() {
    let headset = Arc::new(Mutex::new(Headset {
        read_error: Some(INVALID_FUNCTION_ID),
        ..Headset::g933(0)
    }));
    let shared = headset_channel(&headset).await;

    assert_matches!(
        get_power_off_timeout_on(&shared).await,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x1f20
        })
    );
}

#[tokio::test]
async fn an_unsupported_answer_means_the_device_does_not_support_the_timeout() {
    let headset = Arc::new(Mutex::new(Headset {
        read_error: Some(UNSUPPORTED),
        ..Headset::g933(0)
    }));
    let shared = headset_channel(&headset).await;

    assert_matches!(
        get_power_off_timeout_on(&shared).await,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x1f20
        })
    );
}

/// A busy or faulting headset may answer next time. Reading it as
/// unsupported would hide the setting until it reconnects.
#[tokio::test]
async fn a_busy_or_faulting_read_stays_a_retryable_error() {
    for (code, kind) in [
        (BUSY, HidppFeatureErrorKind::Busy),
        (HW_ERROR, HidppFeatureErrorKind::HwError),
    ] {
        let headset = Arc::new(Mutex::new(Headset {
            read_error: Some(code),
            ..Headset::g933(0)
        }));
        let shared = headset_channel(&headset).await;

        let result = get_power_off_timeout_on(&shared).await;
        assert!(
            matches!(
                result,
                Err(WriteError::HidppFeature {
                    operation: HidppOperation::ReadPowerOffTimeout,
                    feature_hex: 0x1f20,
                    kind: found,
                }) if found == kind
            ),
            "error {code:#04x} gave {result:?}"
        );
    }
}

#[tokio::test]
async fn a_timeout_above_the_range_is_an_unsupported_response() {
    let headset = Arc::new(Mutex::new(Headset::g933(120)));
    let shared = headset_channel(&headset).await;

    assert_matches!(
        get_power_off_timeout_on(&shared).await,
        Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadPowerOffTimeout,
            feature_hex: 0x1f20
        })
    );
}
