use std::sync::{Arc, Mutex, PoisonError};

use hidpp::feature::smartshift_enhanced::SmartShiftEnhancedCapabilities;

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

const SMARTSHIFT_INDEX: u8 = 0x06;
const GET_CAPABILITIES: u8 = 0;
const SET_RATCHET_CONTROL_MODE: u8 = 2;
/// HID++ 2.0 `InvalidFunctionId`, a permanent (non-retried) feature error.
const INVALID_FUNCTION_ID: u8 = 0x07;

/// The firmware-side state of a scripted `0x2111` wheel.
#[derive(Clone, Copy)]
struct Wheel {
    capabilities: SmartShiftEnhancedCapabilities,
    mode: SmartShiftMode,
    auto_disengage: u8,
    torque: u8,
    fail_capabilities: bool,
}

impl Wheel {
    fn new(capabilities: SmartShiftEnhancedCapabilities, torque: u8) -> Self {
        Self {
            capabilities,
            mode: SmartShiftMode::Ratchet,
            auto_disengage: 10,
            torque,
            fail_capabilities: false,
        }
    }

    fn status(self) -> [u8; 3] {
        [u8::from(self.mode), self.auto_disengage, self.torque]
    }

    /// Answer one request, applying a `setRatchetControlMode` the way the
    /// firmware does: a zero parameter leaves that field unchanged.
    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let function = request[3] >> 4;
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                let feature_id = u16::from_be_bytes([request[4], request[5]]);
                payload[0] = if feature_id == 0x2111 {
                    SMARTSHIFT_INDEX
                } else {
                    0
                };
            }
            (SMARTSHIFT_INDEX, GET_CAPABILITIES) if self.fail_capabilities => {
                return Some(feature_error(request, INVALID_FUNCTION_ID));
            }
            (SMARTSHIFT_INDEX, GET_CAPABILITIES) => {
                payload[..4].copy_from_slice(&[self.capabilities.bits(), 0x1e, 50, 0x40]);
            }
            (SMARTSHIFT_INDEX, 1) => payload[..3].copy_from_slice(&self.status()),
            (SMARTSHIFT_INDEX, SET_RATCHET_CONTROL_MODE) => {
                if let Ok(mode) = SmartShiftMode::try_from(request[4]) {
                    self.mode = mode;
                }
                if request[5] != 0 {
                    self.auto_disengage = request[5];
                }
                if request[6] != 0 {
                    self.torque = request[6];
                }
                payload[..3].copy_from_slice(&self.status());
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

struct Harness {
    shared: SharedChannel,
    wheel: Arc<Mutex<Wheel>>,
    written: Box<dyn Fn() -> Vec<Vec<u8>>>,
}

impl Harness {
    async fn new(wheel: Wheel) -> Self {
        let wheel = Arc::new(Mutex::new(wheel));
        let responder_wheel = Arc::clone(&wheel);
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
                return None;
            }
            responder_wheel
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .respond(request)
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
            wheel,
            written: Box::new(move || handle.written_reports()),
        }
    }

    fn wheel(&self) -> Wheel {
        *self.wheel.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn requests_to(&self, function: u8) -> Vec<[u8; 3]> {
        (self.written)()
            .iter()
            .filter(|report| report[2] == SMARTSHIFT_INDEX && report[3] >> 4 == function)
            .map(|report| [report[4], report[5], report[6]])
            .collect()
    }

    /// The parameter bytes of every `setRatchetControlMode` request.
    fn writes(&self) -> Vec<[u8; 3]> {
        self.requests_to(SET_RATCHET_CONTROL_MODE)
    }

    fn capability_reads(&self) -> usize {
        self.requests_to(GET_CAPABILITIES).len()
    }
}

fn torque(value: u8) -> TunableTorque {
    TunableTorque::try_new(value).expect("valid test torque")
}

fn threshold(value: u8) -> SmartShiftAutoDisengage {
    SmartShiftAutoDisengage::try_from(value).expect("valid test threshold")
}

const RATCHET: u8 = 2;
const FREE: u8 = 1;

#[tokio::test]
async fn toggle_leaves_the_torque_to_the_device() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(
        SmartShiftEnhancedCapabilities::TUNABLE_TORQUE,
        75,
    ))
    .await;

    assert_eq!(
        toggle_smartshift_on(&harness.shared).await?,
        SmartShiftMode::Free
    );

    assert_eq!(harness.writes(), [[FREE, 10, 0]]);
    assert_eq!(harness.capability_reads(), 0);
    assert_eq!(harness.wheel().torque, 75);
    Ok(())
}

#[tokio::test]
async fn sensitivity_write_leaves_the_torque_to_the_device() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(SmartShiftEnhancedCapabilities::empty(), 50)).await;
    let mut device = Device::new(
        Arc::clone(harness.shared.channel()),
        harness.shared.device_index(),
    )
    .await
    .expect("scripted device answers the root ping");
    let smartshift = SmartShift::open(&mut device).await?;

    smartshift.set_sensitivity(threshold(30)).await?;

    assert_eq!(harness.writes(), [[RATCHET, 30, 0]]);
    assert_eq!(harness.capability_reads(), 0);
    Ok(())
}

#[tokio::test]
async fn reapply_writes_a_changed_torque_when_the_device_advertises_it() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(
        SmartShiftEnhancedCapabilities::TUNABLE_TORQUE,
        75,
    ))
    .await;

    set_smartshift_on(
        &harness.shared,
        SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: threshold(10),
            tunable_torque: Some(torque(40)),
        },
    )
    .await?;

    assert_eq!(harness.capability_reads(), 1);
    assert_eq!(harness.writes(), [[RATCHET, 10, 40]]);
    assert_eq!(harness.wheel().torque, 40);
    Ok(())
}

#[tokio::test]
async fn reapply_leaves_the_torque_alone_without_the_capability() -> Result<(), WriteError> {
    // The status reply carries a non-zero torque byte even though the
    // capabilities bit is clear — the byte alone must not be treated as a
    // tunable-torque wheel.
    let harness = Harness::new(Wheel::new(SmartShiftEnhancedCapabilities::empty(), 50)).await;

    set_smartshift_on(
        &harness.shared,
        SmartShiftStatus {
            mode: SmartShiftMode::Free,
            auto_disengage: threshold(10),
            tunable_torque: Some(torque(40)),
        },
    )
    .await?;

    assert_eq!(harness.capability_reads(), 1);
    assert_eq!(harness.writes(), [[FREE, 10, 0]]);
    assert_eq!(harness.wheel().torque, 50);
    Ok(())
}

#[tokio::test]
async fn reapply_does_not_rewrite_an_unchanged_torque() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(
        SmartShiftEnhancedCapabilities::TUNABLE_TORQUE,
        75,
    ))
    .await;

    set_smartshift_on(
        &harness.shared,
        SmartShiftStatus {
            mode: SmartShiftMode::Free,
            auto_disengage: threshold(10),
            tunable_torque: Some(torque(75)),
        },
    )
    .await?;

    assert_eq!(harness.capability_reads(), 0);
    assert_eq!(harness.writes(), [[FREE, 10, 0]]);
    Ok(())
}

#[tokio::test]
async fn reapply_fails_without_writing_when_capabilities_cannot_be_read() {
    let mut wheel = Wheel::new(SmartShiftEnhancedCapabilities::TUNABLE_TORQUE, 75);
    wheel.fail_capabilities = true;
    let harness = Harness::new(wheel).await;

    let err = set_smartshift_on(
        &harness.shared,
        SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: threshold(10),
            tunable_torque: Some(torque(40)),
        },
    )
    .await
    .expect_err("an unreadable capability must not be guessed");

    assert!(
        matches!(
            err,
            WriteError::HidppFeature {
                operation: HidppOperation::ReadSmartShift,
                feature_hex: 0x2111,
                kind: HidppFeatureErrorKind::InvalidFunctionId,
            }
        ),
        "unexpected error: {err:?}"
    );
    assert!(harness.writes().is_empty());
    assert_eq!(harness.wheel().torque, 75);
}
