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
    /// Answer as a `0x2110` legacy wheel instead of `0x2111`.
    legacy: bool,
    /// Applied after each `setRatchetControlMode`, before the reply: how
    /// the firmware settles the requested state.
    firmware: fn(&mut Wheel),
}

impl Wheel {
    fn new(capabilities: SmartShiftEnhancedCapabilities, torque: u8) -> Self {
        Self {
            capabilities,
            mode: SmartShiftMode::Ratchet,
            auto_disengage: 10,
            torque,
            fail_capabilities: false,
            legacy: false,
            firmware: |_| {},
        }
    }

    /// A `0x2110` wheel; its third status byte is the threshold default.
    fn legacy() -> Self {
        Self {
            legacy: true,
            torque: 0x0a,
            ..Self::new(SmartShiftEnhancedCapabilities::empty(), 0)
        }
    }

    fn status(self) -> [u8; 3] {
        [u8::from(self.mode), self.auto_disengage, self.torque]
    }

    /// The function numbers of the getter and the setter.
    fn functions(self) -> (u8, u8) {
        if self.legacy {
            (0, 1)
        } else {
            (1, SET_RATCHET_CONTROL_MODE)
        }
    }

    /// Answer one request, applying a `setRatchetControlMode` the way the
    /// firmware does: a zero parameter leaves that field unchanged.
    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let function = request[3] >> 4;
        let (get, set) = self.functions();
        let feature = if self.legacy { 0x2110 } else { 0x2111 };
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                let feature_id = u16::from_be_bytes([request[4], request[5]]);
                payload[0] = if feature_id == feature {
                    SMARTSHIFT_INDEX
                } else {
                    0
                };
            }
            (SMARTSHIFT_INDEX, GET_CAPABILITIES) if !self.legacy && self.fail_capabilities => {
                return Some(feature_error(request, INVALID_FUNCTION_ID));
            }
            (SMARTSHIFT_INDEX, GET_CAPABILITIES) if !self.legacy => {
                payload[..4].copy_from_slice(&[self.capabilities.bits(), 0x1e, 50, 0x40]);
            }
            (SMARTSHIFT_INDEX, f) if f == get => payload[..3].copy_from_slice(&self.status()),
            (SMARTSHIFT_INDEX, f) if f == set => {
                if let Ok(mode) = SmartShiftMode::try_from(request[4]) {
                    self.mode = mode;
                }
                if request[5] != 0 {
                    self.auto_disengage = request[5];
                }
                if !self.legacy && request[6] != 0 {
                    self.torque = request[6];
                }
                (self.firmware)(self);
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
        self.requests_to(self.wheel().functions().1)
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
async fn a_toggle_retry_resends_its_target_instead_of_toggling_back() {
    // The firmware takes the new mode but settles every write on its own
    // threshold, so the reply never confirms the 10 the toggle kept.
    let harness = Harness::new(Wheel {
        firmware: |w| w.auto_disengage = 20,
        ..Wheel::new(SmartShiftEnhancedCapabilities::empty(), 50)
    })
    .await;

    let result = toggle_smartshift_on(&harness.shared).await;

    assert!(
        matches!(
            result,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteSmartShift,
                feature_hex: 0x2111,
            })
        ),
        "unexpected result: {result:?}"
    );
    // A re-read after the first write would flip free/20 back to ratchet.
    assert_eq!(harness.writes(), [[FREE, 10, 0], [FREE, 10, 0]]);
    assert_eq!(harness.wheel().mode, SmartShiftMode::Free);
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

#[tokio::test]
async fn status_read_reports_a_zero_torque_byte_as_absent() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(SmartShiftEnhancedCapabilities::empty(), 0)).await;

    let status = get_smartshift_status_on(&harness.shared).await?;

    assert_eq!(status.tunable_torque, None);
    Ok(())
}

#[tokio::test]
async fn status_read_rejects_a_torque_above_one_hundred_percent() {
    let harness = Harness::new(Wheel::new(
        SmartShiftEnhancedCapabilities::TUNABLE_TORQUE,
        200,
    ))
    .await;

    let err = get_smartshift_status_on(&harness.shared)
        .await
        .expect_err("an undefined torque level must not be dropped silently");

    assert!(
        matches!(
            err,
            WriteError::UnsupportedResponse {
                operation: HidppOperation::ReadSmartShift,
                feature_hex: 0x2111,
            }
        ),
        "unexpected error: {err:?}"
    );
}

/// Write `status` to a wheel whose firmware settles each write with
/// `firmware`, returning the result and the writes sent.
async fn write_through(
    wheel: Wheel,
    firmware: fn(&mut Wheel),
    status: SmartShiftStatus,
) -> (Result<(), WriteError>, Vec<[u8; 3]>) {
    let harness = Harness::new(Wheel { firmware, ..wheel }).await;
    let result = set_smartshift_on(&harness.shared, status).await;
    (result, harness.writes())
}

fn refused(result: &Result<(), WriteError>, feature: u16) {
    assert!(
        matches!(
            result,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteSmartShift,
                feature_hex,
            }) if *feature_hex == feature
        ),
        "unexpected result: {result:?}"
    );
}

#[tokio::test]
async fn a_write_fails_when_the_reply_shows_another_state() {
    let free = SmartShiftStatus {
        mode: SmartShiftMode::Free,
        auto_disengage: threshold(30),
        tunable_torque: Some(torque(40)),
    };
    let enhanced = Wheel::new(SmartShiftEnhancedCapabilities::TUNABLE_TORQUE, 75);

    // The firmware keeps ratchet mode; the one transient retry fails alike.
    let (result, writes) =
        write_through(enhanced, |w| w.mode = SmartShiftMode::Ratchet, free).await;
    refused(&result, 0x2111);
    assert_eq!(writes, [[FREE, 30, 40], [FREE, 30, 40]]);

    // It ignores the torque it advertised as tunable.
    let (result, _) = write_through(enhanced, |w| w.torque = 75, free).await;
    refused(&result, 0x2111);

    // It clamps the threshold on a legacy wheel.
    let (result, writes) = write_through(Wheel::legacy(), |w| w.auto_disengage = 20, free).await;
    refused(&result, 0x2110);
    assert_eq!(writes, [[FREE, 30, 0], [FREE, 30, 0]]);
}

#[tokio::test]
async fn a_write_holds_when_the_reply_carries_what_it_wrote() -> Result<(), WriteError> {
    let free = SmartShiftStatus {
        mode: SmartShiftMode::Free,
        auto_disengage: threshold(30),
        tunable_torque: Some(torque(40)),
    };
    let (result, writes) = write_through(Wheel::legacy(), |_| {}, free).await;
    result?;
    assert_eq!(writes, [[FREE, 30, 0]]);

    // Without the capability the torque is not written, so a reply carrying
    // another torque byte is no mismatch.
    let plain = Wheel::new(SmartShiftEnhancedCapabilities::empty(), 50);
    let (result, writes) = write_through(plain, |w| w.torque = 51, free).await;
    result?;
    assert_eq!(writes, [[FREE, 30, 0]]);
    Ok(())
}

/// Open the scripted wheel the way the route-based writes do.
async fn open_wheel(harness: &Harness) -> Result<SmartShift, WriteError> {
    let mut device = Device::new(
        Arc::clone(harness.shared.channel()),
        harness.shared.device_index(),
    )
    .await
    .expect("scripted device answers the root ping");
    SmartShift::open(&mut device).await
}

fn torque_unsupported(result: &Result<(), WriteError>) {
    assert!(
        matches!(
            result,
            Err(WriteError::FeatureUnsupported {
                feature_hex: 0x2111
            })
        ),
        "unexpected result: {result:?}"
    );
}

#[tokio::test]
async fn a_torque_write_sends_only_the_torque_when_advertised() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(
        SmartShiftEnhancedCapabilities::TUNABLE_TORQUE,
        50,
    ))
    .await;
    let smartshift = open_wheel(&harness).await?;

    smartshift.set_torque(torque(65)).await?;

    // Mode and threshold go out as the "do not change" zero.
    assert_eq!(harness.writes(), [[0, 0, 65]]);
    assert_eq!(harness.capability_reads(), 1);
    let wheel = harness.wheel();
    assert_eq!(
        (wheel.mode, wheel.auto_disengage, wheel.torque),
        (SmartShiftMode::Ratchet, 10, 65)
    );
    Ok(())
}

#[tokio::test]
async fn a_torque_write_is_refused_without_the_capability() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::new(SmartShiftEnhancedCapabilities::empty(), 50)).await;
    let smartshift = open_wheel(&harness).await?;

    torque_unsupported(&smartshift.set_torque(torque(65)).await);

    assert!(harness.writes().is_empty());
    assert_eq!(harness.wheel().torque, 50);
    Ok(())
}

#[tokio::test]
async fn a_torque_write_is_refused_on_a_legacy_wheel() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel::legacy()).await;
    let smartshift = open_wheel(&harness).await?;

    torque_unsupported(&smartshift.set_torque(torque(65)).await);

    assert!(harness.writes().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_torque_write_fails_when_capabilities_cannot_be_read() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel {
        fail_capabilities: true,
        ..Wheel::new(SmartShiftEnhancedCapabilities::TUNABLE_TORQUE, 50)
    })
    .await;
    let smartshift = open_wheel(&harness).await?;

    let result = smartshift.set_torque(torque(65)).await;

    assert!(
        matches!(
            result,
            Err(WriteError::HidppFeature {
                operation: HidppOperation::ReadSmartShift,
                feature_hex: 0x2111,
                kind: HidppFeatureErrorKind::InvalidFunctionId,
            })
        ),
        "unexpected result: {result:?}"
    );
    assert!(harness.writes().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_torque_write_fails_when_the_reply_keeps_another_torque() -> Result<(), WriteError> {
    let harness = Harness::new(Wheel {
        firmware: |w| w.torque = 50,
        ..Wheel::new(SmartShiftEnhancedCapabilities::TUNABLE_TORQUE, 50)
    })
    .await;
    let smartshift = open_wheel(&harness).await?;

    refused(&smartshift.set_torque(torque(65)).await, 0x2111);
    Ok(())
}
