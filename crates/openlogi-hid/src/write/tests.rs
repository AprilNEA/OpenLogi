use std::assert_matches;
use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use hidpp::channel::{HidppChannel, RawHidChannel};
use hidpp::feature::smartshift::WheelMode;
use openlogi_core::config::LightSettings;
use openlogi_core::device::{LightCapabilities, LightValueRange, LightValueUnit};
use tokio::sync::mpsc;

use crate::SmartShiftMode;
use crate::SmartShiftStatus;
use crate::write::lighting::per_key_reports;
use crate::write::smartshift::{
    is_missing_enhanced, is_transient_smartshift_error, smartshift_to_wheel,
    status_matches_desired, wheel_mode_to_smartshift,
};
use crate::write::{HidppFeatureErrorKind, HidppOperation};

#[test]
fn light_settings_expand_only_to_advertised_controls() {
    let Ok(brightness) = LightValueRange::new(0, 100, 1, LightValueUnit::Percent) else {
        panic!("valid brightness fixture");
    };
    let settings = LightSettings::new(false, 37, Some(4600));
    let commands = commands_for_light_settings(
        settings,
        LightCapabilities {
            brightness: Some(brightness),
            ..LightCapabilities::default()
        },
    );

    assert_eq!(commands, vec![LightCommand::BrightnessPercent(37)]);
}

#[test]
fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

    assert_eq!(caps.values(), [400, 800, 1600]);
    assert_eq!(caps.min(), 400);
    assert_eq!(caps.max(), 1600);
    Ok(())
}

#[test]
fn capabilities_reject_empty_list() {
    assert_matches!(
        DpiCapabilities::new(Vec::new()),
        Err(WriteError::EmptyDpiList)
    );
}

#[test]
fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.nearest(390), 400);
    assert_eq!(caps.nearest(1000), 800);
    assert_eq!(caps.nearest(2000), 1600);
    Ok(())
}

#[test]
fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

    assert_eq!(caps.step_hint(), 400);
    Ok(())
}

#[test]
fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.adjacent_test_target(400), Some(800));
    assert_eq!(caps.adjacent_test_target(800), Some(1600));
    assert_eq!(caps.adjacent_test_target(1600), Some(800));
    Ok(())
}

#[test]
fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
    let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

    assert_eq!(caps.adjacent_test_target(1000), Some(1600));
    assert_eq!(caps.adjacent_test_target(2000), Some(1600));
    Ok(())
}

#[test]
fn smartshift_and_wheel_mode_byte_encodings_match() {
    // The whole design relies on 0x2110 WheelMode and 0x2111
    // SmartShiftMode sharing one wire encoding (Free/Freespin = 1,
    // Ratchet = 2). If the fork ever renumbers WheelMode this fails loudly.
    assert_eq!(
        u8::from(SmartShiftMode::Free),
        u8::from(WheelMode::Freespin)
    );
    assert_eq!(
        u8::from(SmartShiftMode::Ratchet),
        u8::from(WheelMode::Ratchet)
    );
}

#[test]
fn wheel_mode_maps_to_smartshift_mode() {
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Freespin),
        SmartShiftMode::Free
    );
    assert_eq!(
        wheel_mode_to_smartshift(WheelMode::Ratchet),
        SmartShiftMode::Ratchet
    );
}

#[test]
fn smartshift_to_wheel_round_trips() {
    // smartshift_to_wheel is the inverse of wheel_mode_to_smartshift.
    for mode in [SmartShiftMode::Free, SmartShiftMode::Ratchet] {
        assert_eq!(wheel_mode_to_smartshift(smartshift_to_wheel(mode)), mode);
    }
}

#[test]
fn missing_enhanced_triggers_fallback() {
    assert!(is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2111,
    }));
}

#[test]
fn missing_legacy_does_not_trigger_fallback() {
    // A device missing 0x2110 must NOT loop back — it genuinely has no
    // SmartShift.
    assert!(!is_missing_enhanced(&WriteError::FeatureUnsupported {
        feature_hex: 0x2110,
    }));
}

#[test]
fn transport_errors_do_not_trigger_fallback() {
    // Real failures must propagate, not be masked by a fallback attempt.
    assert!(!is_missing_enhanced(&WriteError::DeviceUnreachable {
        index: 0xff,
    }));
    assert!(!is_missing_enhanced(&WriteError::Hidpp("boom".into())));
}

#[test]
fn transient_smartshift_errors_are_retryable() {
    assert!(is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2111,
        kind: HidppFeatureErrorKind::InvalidArgument,
    }));
    assert!(is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2110,
        kind: HidppFeatureErrorKind::Busy,
    }));
    assert!(is_transient_smartshift_error(
        &WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadSmartShift,
            feature_hex: 0x2110,
        }
    ));
}

#[test]
fn permanent_smartshift_errors_are_not_retryable() {
    assert!(!is_transient_smartshift_error(
        &WriteError::FeatureUnsupported {
            feature_hex: 0x2111,
        }
    ));
    assert!(!is_transient_smartshift_error(&WriteError::HidppFeature {
        operation: HidppOperation::WriteSmartShift,
        feature_hex: 0x2111,
        kind: HidppFeatureErrorKind::InvalidFunctionId,
    }));
}

#[test]
fn status_match_ignores_zero_preserve_fields() {
    let current = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: 10,
        tunable_torque: 33,
    };
    let desired = SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: 10,
        // 0 = "do not change" on the write path — already-matched for reapply.
        tunable_torque: 0,
    };
    assert!(status_matches_desired(current, desired));
    assert!(!status_matches_desired(
        current,
        SmartShiftStatus {
            mode: SmartShiftMode::Free,
            ..desired
        }
    ));
}

#[test]
fn per_key_lighting_builds_only_very_long_frames_then_one_long_commit() {
    let reports = per_key_reports(0x03, 0x27, 0x11, 0x22, 0x33);
    let (commit, frames) = reports
        .split_last()
        .unwrap_or_else(|| panic!("per-key lighting must emit a commit"));

    assert_eq!(frames.len(), 17);
    assert!(frames.iter().all(|report| report.len() == 64));
    assert!(frames.iter().all(|report| report[0] == 0x12));
    assert!(frames.iter().all(|report| report[1] == 0x03));
    assert!(frames.iter().all(|report| report[2] == 0x27));
    assert!(frames.iter().all(|report| report[3] == 0x3a));
    assert!(frames.iter().all(|report| report[5] == 0x01));
    assert!(frames.iter().all(|report| report[7] == 0x0e));

    let entries: Vec<_> = frames
        .iter()
        .flat_map(|report| report[8..64].chunks_exact(4))
        .take(0xe9)
        .map(|entry| (entry[0], entry[1], entry[2], entry[3]))
        .collect();
    assert_eq!(entries.len(), 0xe9);
    for (key, entry) in (0x00u8..=0xe8).zip(entries) {
        assert_eq!(entry, (key, 0x11, 0x22, 0x33));
    }

    assert_eq!(commit.len(), 20);
    assert_eq!(&commit[..4], &[0x11, 0x03, 0x27, 0x5a]);
    assert!(commit[4..].iter().all(|byte| *byte == 0));
}

#[tokio::test]
async fn shared_read_and_lighting_apis_use_the_supplied_channel() -> Result<(), WriteError> {
    let (raw, handle) = ScriptedRawHidChannel::new();
    let channel = Arc::new(
        HidppChannel::from_raw_channel(raw)
            .await
            .unwrap_or_else(|error| panic!("scripted HID++ channel must open: {error:?}")),
    );
    let shared = SharedChannel::new(
        channel,
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        },
    );

    let dpi = get_dpi_info_on(&shared).await?;
    assert_eq!(dpi.current, 800);
    assert_eq!(dpi.capabilities.values(), [400, 800, 1600]);

    let smartshift = get_smartshift_status_on(&shared).await?;
    assert_eq!(smartshift.mode, SmartShiftMode::Ratchet);
    assert_eq!(smartshift.auto_disengage, 10);
    assert_eq!(smartshift.tunable_torque, 33);

    // The scripted device reports no 0x8070 effect engine, so Auto must fall
    // back to 0x8080 without opening a second transport.
    set_keyboard_color_on(&shared, 0x11, 0x22, 0x33).await?;

    let written = handle.written_reports();
    let very_long: Vec<_> = written
        .iter()
        .filter(|report| report.first() == Some(&0x12))
        .collect();
    assert_eq!(very_long.len(), 17);
    assert!(very_long.iter().all(|report| report.len() == 64));
    assert!(written.iter().any(|report| {
        report.len() == 20
            && report[0] == 0x11
            && report[1] == 0xff
            && report[2] == 0x07
            && report[3] >> 4 == 0x05
    }));
    Ok(())
}

#[derive(Clone)]
struct ScriptedRawHidHandle {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ScriptedRawHidHandle {
    fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct ScriptedRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ScriptedRawHidChannel {
    fn new() -> (Self, ScriptedRawHidHandle) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written: Arc::clone(&written),
            },
            ScriptedRawHidHandle { written },
        )
    }
}

#[hidpp::async_trait]
impl RawHidChannel for ScriptedRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xb35b
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(src.to_vec());
        if let Some(response) = scripted_response(src) {
            self.incoming_tx.send(response).map_err(|_| mock_error())?;
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(mock_error());
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        unreachable!("scripted channel declares HID++ support")
    }
}

fn scripted_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let feature_index = request[2];
    let function = request[3] >> 4;
    let mut payload = [0u8; 16];
    let long = match (feature_index, function) {
        // Root ping used by Device::new.
        (0x00, 0x01) => {
            payload[0] = 4;
            false
        }
        // Root feature lookup.
        (0x00, 0x00) => {
            let feature_id = u16::from_be_bytes([request[4], request[5]]);
            payload[0] = match feature_id {
                0x2201 => 0x05,
                0x2111 => 0x06,
                0x8080 => 0x07,
                _ => 0x00,
            };
            false
        }
        // AdjustableDpi sensor count/current/list.
        (0x05, 0x00) => {
            payload[0] = 1;
            false
        }
        (0x05, 0x02) => {
            payload[1..3].copy_from_slice(&800u16.to_be_bytes());
            false
        }
        (0x05, 0x01) => {
            payload[..8].copy_from_slice(&[0, 0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0]);
            true
        }
        // Enhanced SmartShift status.
        (0x06, 0x01) => {
            payload[..3].copy_from_slice(&[u8::from(WheelMode::Ratchet), 10, 33]);
            false
        }
        // Raw per-key frame commit expects no reply.
        _ => return None,
    };

    let mut response = vec![0u8; if long { 20 } else { 7 }];
    response[0] = if long { 0x11 } else { 0x10 };
    response[1..4].copy_from_slice(&request[1..4]);
    let payload_len = response.len() - 4;
    response[4..].copy_from_slice(&payload[..payload_len]);
    Some(response)
}

fn mock_error() -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "scripted HID channel closed",
    ))
}
