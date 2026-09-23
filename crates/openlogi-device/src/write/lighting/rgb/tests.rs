use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openlogi_core::color::Rgb;

use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::{
    HidppFeatureErrorKind, HidppOperation, LightingMethod, LightingWrite, WriteError,
};
use crate::{DeviceRoute, SharedChannel};

fn write() -> LightingWrite {
    LightingWrite {
        method: LightingMethod::Auto,
        color: Rgb::new(0x17, 0x82, 0xc4),
    }
}

async fn device(
    respond: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> (SharedChannel, crate::replay::ReplayChannelHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(respond);
    let channel = scripted_channel(raw).await;
    (
        SharedChannel::new(
            channel,
            DeviceRoute::Unifying {
                receiver_uid: "rgb-test".into(),
                slot: 3,
            },
        ),
        handle,
    )
}

// Nonzero pre-existing control/event flags catch restoring zero, clearing
// unrelated bits on claim, or mistakenly treating the claim as the snapshot.
const PREVIOUS: [u8; 3] = [1, 0x82, 0x45];
const CLAIM: [u8; 3] = [1, 0x83, 0x45];

fn is_control(request: &[u8], payload: [u8; 3]) -> bool {
    request[2] == 8 && request[3] >> 4 == 5 && request[4..7] == payload
}

fn is_effect(request: &[u8]) -> bool {
    request[2] == 8 && request[3] >> 4 == 1
}

fn control_writes(reports: &[Vec<u8>]) -> Vec<&[u8]> {
    reports
        .iter()
        .filter(|r| r[2] == 8 && r[3] >> 4 == 5 && r[4] == 1)
        .map(|r| &r[4..7])
        .collect()
}

fn assert_no_fallback(reports: &[Vec<u8>]) {
    assert!(
        reports
            .iter()
            .all(|r| !(r[2] == 0 && r[3] >> 4 == 0 && r[4..6] == [0x80, 0x81])),
        "must not fall back after a claim"
    );
}

#[tokio::test]
async fn rgb_success_preflights_every_cluster_and_keeps_software_control() {
    let (shared, handle) = device(response).await;
    write().apply_on(&shared, || false, || true).await.unwrap();
    let reports = handle.written_reports();
    assert!(
        reports.iter().all(|r| r[1] == 3),
        "receiver slot is not the direct index"
    );
    assert_eq!(control_writes(&reports), [CLAIM.as_slice()]);
    let claim = reports.iter().position(|r| is_control(r, CLAIM)).unwrap();
    let last_info = reports
        .iter()
        .rposition(|r| r[2] == 8 && r[3] >> 4 == 0)
        .unwrap();
    assert!(
        last_info < claim,
        "all discovery precedes the first mutation"
    );
    let effects: Vec<_> = reports.iter().filter(|r| is_effect(r)).collect();
    assert_eq!(effects.len(), 2);
    for (report, (cluster, effect)) in effects.iter().zip([(0, 1), (1, 0)]) {
        assert_eq!(&report[4..9], &[cluster, effect, 0x17, 0x82, 0xc4]);
        assert_eq!(report[16], 1, "volatile, full-power effects only");
    }
    assert_no_fallback(&reports);
}

#[tokio::test]
async fn rgb_partial_support_and_empty_device_fall_back_without_claiming() {
    for empty in [false, true] {
        let (shared, handle) = device(move |request| {
            let mut reply = response(request)?;
            if request[2] == 8 && request[3] >> 4 == 0 {
                if empty && request[4..6] == [0xff, 0xff] {
                    reply[6] = 0;
                }
                if !empty && request[4..6] == [1, 0] {
                    reply[6..8].copy_from_slice(&2u16.to_be_bytes());
                }
            }
            Some(reply)
        })
        .await;
        write().apply_on(&shared, || false, || true).await.unwrap();
        let reports = handle.written_reports();
        assert!(control_writes(&reports).is_empty());
        assert!(reports.iter().all(|r| !is_effect(r)));
        let paints: Vec<_> = reports
            .iter()
            .filter(|r| r[2] == 7 && r[3] >> 4 == 6)
            .collect();
        assert_eq!(paints.len(), 1);
        assert_eq!(&paints[0][4..9], &[0x17, 0x82, 0xc4, 1, 4]);
        assert!(
            reports
                .iter()
                .any(|r| r[2] == 7 && r[3] >> 4 == 7 && r[4..7] == [0, 0, 0]),
            "0x8081 frameEnd uses persistence 0 for RAM only"
        );
    }
}

#[tokio::test]
async fn rgb_cancellation_after_claim_or_last_effect_restores_the_snapshot() {
    for after_claim in [true, false] {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let (shared, handle) = device(move |request| {
            if (after_claim && is_control(request, CLAIM))
                || (!after_claim && is_effect(request) && request[4] == 1)
            {
                trigger.store(true, Ordering::Release);
            }
            response(request)
        })
        .await;
        let error = write()
            .apply_on(&shared, || cancel.load(Ordering::Acquire), || true)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            WriteError::RequestTimedOut {
                operation: HidppOperation::Lighting
            }
        );
        let reports = handle.written_reports();
        assert_eq!(
            control_writes(&reports),
            [CLAIM.as_slice(), PREVIOUS.as_slice()]
        );
        assert_no_fallback(&reports);
    }
}

#[tokio::test]
async fn rgb_lost_claim_reply_still_requires_rollback() {
    let (shared, handle) = device(|request| {
        if is_control(request, CLAIM) {
            None
        } else {
            response(request)
        }
    })
    .await;
    let error = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WriteError::RequestTimedOut {
            operation: HidppOperation::Lighting
        }
    );
    let reports = handle.written_reports();
    assert_eq!(
        control_writes(&reports),
        [CLAIM.as_slice(), PREVIOUS.as_slice()]
    );
    assert!(reports.iter().all(|r| !is_effect(r)));
    assert_no_fallback(&reports);
}

#[tokio::test]
async fn rgb_write_failure_restores_and_cleanup_failure_is_not_hidden() {
    for restore_fails in [false, true] {
        let (shared, handle) = device(move |request| {
            if is_effect(request) {
                return Some(feature_error(request, 9));
            }
            if restore_fails && is_control(request, PREVIOUS) {
                return Some(feature_error(request, 8));
            }
            response(request)
        })
        .await;
        let error = write()
            .apply_on(&shared, || false, || true)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            WriteError::HidppFeature {
                operation: HidppOperation::Lighting,
                feature_hex: 0x8071,
                kind: if restore_fails {
                    HidppFeatureErrorKind::Busy
                } else {
                    HidppFeatureErrorKind::Unsupported
                },
            }
        );
        let reports = handle.written_reports();
        assert_eq!(
            control_writes(&reports),
            [CLAIM.as_slice(), PREVIOUS.as_slice()]
        );
        assert_no_fallback(&reports);
    }
}

#[tokio::test]
async fn rgb_retirement_stops_further_writes_including_stale_cleanup() {
    let current = Arc::new(AtomicBool::new(true));
    let retire = Arc::clone(&current);
    let (shared, handle) = device(move |request| {
        if is_effect(request) {
            retire.store(false, Ordering::Release);
        }
        response(request)
    })
    .await;
    let error = write()
        .apply_on(&shared, || false, || current.load(Ordering::Acquire))
        .await
        .unwrap_err();
    assert_eq!(error, WriteError::DeviceNotFound);
    let reports = handle.written_reports();
    assert_eq!(control_writes(&reports), [CLAIM.as_slice()]);
    assert_eq!(reports.iter().filter(|r| is_effect(r)).count(), 1);
    assert_no_fallback(&reports);
}

fn response(request: &[u8]) -> Option<Vec<u8>> {
    let mut reply = vec![0; 20];
    reply[..4].copy_from_slice(&request[..4]);
    reply[0] = 0x11;
    match (request[2], request[3] >> 4) {
        (0, 1) => reply[4] = 4,
        (0, 0) => {
            reply[4] = match &request[4..6] {
                [0x80, 0x71] => 8,
                [0x80, 0x81] => 7,
                _ => 0,
            }
        }
        (8, 0) => {
            reply[4..7].copy_from_slice(&request[4..7]);
            match (request[4], request[5]) {
                (0xff, 0xff) => reply[6] = 2,
                (0, 0xff) => reply[8] = 2,
                (1, 0xff) => reply[8] = 1,
                (cluster @ (0 | 1), effect) => {
                    let id: u16 = if (cluster, effect) == (0, 1) || (cluster, effect) == (1, 0) {
                        1
                    } else {
                        2
                    };
                    reply[6..8].copy_from_slice(&id.to_be_bytes());
                }
                _ => return None,
            }
        }
        (8, 5) => {
            if request[4] == 0 {
                reply[4..7].copy_from_slice(&[0, 0x82, 0x45]);
            } else {
                reply[4..7].copy_from_slice(&request[4..7]);
            }
        }
        // setRgbClusterEffect returns no payload (v4 spec, p. 18).
        (8, 1) => {}
        (7, 0) => {
            if request[5] == 0 {
                reply[6] = 0b0001_0010;
            }
        }
        (7, 6 | 7) => reply[4..].copy_from_slice(&request[4..]),
        _ => return None,
    }
    Some(reply)
}
