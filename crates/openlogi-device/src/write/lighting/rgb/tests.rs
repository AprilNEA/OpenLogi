use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openlogi_core::color::Rgb;

use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::lighting::rgb::RgbControlLease;
use crate::write::{
    HidppFeatureErrorKind, HidppOperation, LightingMethod, LightingWrite, WriteError,
    release_rgb_control,
};
use crate::{DeviceRoute, SharedChannel};
use hidpp::feature::rgb_effects::{EventsNotificationFlags, SwControlFlags};

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

fn is_paint(request: &[u8]) -> bool {
    request[2] == 7 && request[3] >> 4 == 6
}

fn is_frame_end(request: &[u8]) -> bool {
    request[2] == 7 && request[3] >> 4 == 7
}

/// The unit ID [`response`] reports through `0x0003`.
const UNIT: [u8; 4] = [0xa1, 0xb2, 0xc3, 0xd4];
/// The unit ID of another device paired to the same receiver slot.
const OTHER_UNIT: [u8; 4] = [1, 2, 3, 4];

fn lease(control: u8, device: Option<[u8; 4]>) -> RgbControlLease {
    RgbControlLease {
        control: SwControlFlags::from_bits_retain(control),
        events: EventsNotificationFlags::from_bits_retain(PREVIOUS[2]),
        device,
    }
}

fn is_unit_id_read(request: &[u8]) -> bool {
    request[2] == 9 && request[3] >> 4 == 0
}

/// `respond`, but the device reports `unit` as its unit ID, or has no
/// `0x0003` at all with `None`.
fn unit(
    respond: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    unit: Option<[u8; 4]>,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        let mut reply = respond(request)?;
        match unit {
            None if request[2] == 0 && request[3] >> 4 == 0 && request[4..6] == [0, 3] => {
                reply[4] = 0;
            }
            Some(unit) if is_unit_id_read(request) => reply[5..9].copy_from_slice(&unit),
            _ => {}
        }
        Some(reply)
    }
}

/// Cluster 1 offers no Static effect, so `0x8071` alone cannot colour the
/// device; with `empty`, the device reports no clusters at all.
fn partial(empty: bool) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
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
    }
}

/// `respond`, but the device already reports cluster control as held.
fn held(
    respond: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    move |request| {
        let mut reply = respond(request)?;
        if request[2] == 8 && request[3] >> 4 == 5 && request[4] == 0 {
            reply[5] |= 1;
        }
        Some(reply)
    }
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
async fn rgb_per_key_paints_under_a_compensated_claim_with_a_static_base() {
    for (method, empty) in [
        (LightingMethod::Auto, false),
        (LightingMethod::Auto, true),
        (LightingMethod::PerKeyV2, false),
    ] {
        let (shared, handle) = device(partial(empty)).await;
        let lease = LightingWrite { method, ..write() }
            .apply_on(&shared, || false, || true)
            .await
            .unwrap();
        assert_eq!(lease, Some(self::lease(PREVIOUS[1], Some(UNIT))));
        let reports = handle.written_reports();
        assert_eq!(control_writes(&reports), [CLAIM.as_slice()]);
        let claim = reports.iter().position(|r| is_control(r, CLAIM)).unwrap();
        let last_read = reports
            .iter()
            .rposition(|r| r[3] >> 4 == 0 && matches!(r[2], 7 | 8))
            .unwrap();
        assert!(
            last_read < claim,
            "zone and cluster discovery precede the claim"
        );

        // The Static base goes only on the cluster that offers it.
        let effects: Vec<_> = reports.iter().filter(|r| is_effect(r)).collect();
        let expected: &[[u8; 5]] = if empty {
            &[]
        } else {
            &[[0, 1, 0x17, 0x82, 0xc4]]
        };
        assert_eq!(
            effects.iter().map(|r| &r[4..9]).collect::<Vec<_>>(),
            expected.iter().map(<[u8; 5]>::as_slice).collect::<Vec<_>>()
        );

        // A fresh claim replays the first frame once.
        let paints: Vec<_> = reports.iter().filter(|r| is_paint(r)).collect();
        assert_eq!(paints.len(), 2);
        assert!(paints.iter().all(|r| r[4..9] == [0x17, 0x82, 0xc4, 1, 4]));
        let frame_ends: Vec<_> = reports.iter().filter(|r| is_frame_end(r)).collect();
        assert_eq!(frame_ends.len(), 2);
        assert!(
            frame_ends.iter().all(|r| r[4..7] == [0, 0, 0]),
            "0x8081 frameEnd uses persistence 0 for RAM only"
        );
        let first_paint = reports.iter().position(|r| is_paint(r)).unwrap();
        assert!(claim < first_paint, "per-key frames follow the claim");
    }
}

#[tokio::test]
async fn rgb_per_key_under_held_control_paints_once() {
    let (shared, handle) = device(held(partial(false))).await;
    let lease = write().apply_on(&shared, || false, || true).await.unwrap();
    assert_eq!(lease, Some(self::lease(CLAIM[1], Some(UNIT))));
    let reports = handle.written_reports();
    assert_eq!(control_writes(&reports), [CLAIM.as_slice()]);
    assert_eq!(reports.iter().filter(|r| is_paint(r)).count(), 1);
    assert_eq!(reports.iter().filter(|r| is_frame_end(r)).count(), 1);
}

#[tokio::test]
async fn rgb_per_key_failure_restores_the_snapshot() {
    let respond = partial(false);
    let (shared, handle) = device(move |request| {
        if is_paint(request) {
            return Some(feature_error(request, 9));
        }
        respond(request)
    })
    .await;
    let error = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            WriteError::HidppFeature {
                feature_hex: 0x8081,
                ..
            }
        ),
        "{error:?}"
    );
    let reports = handle.written_reports();
    assert_eq!(
        control_writes(&reports),
        [CLAIM.as_slice(), PREVIOUS.as_slice()]
    );
    assert!(
        reports
            .iter()
            .all(|r| !(r[2] == 0 && r[4..6] == [0x80, 0x80])),
        "a failure after the claim must not fall back to 0x8080"
    );
}

#[tokio::test]
async fn rgb_per_key_without_an_effect_engine_never_claims() {
    let (shared, handle) = device(|request| {
        let mut reply = response(request)?;
        if request[2] == 0 && request[3] >> 4 == 0 && request[4..6] == [0x80, 0x71] {
            reply[4] = 0;
        }
        Some(reply)
    })
    .await;
    let lease = write().apply_on(&shared, || false, || true).await.unwrap();
    assert_eq!(lease, None);
    let reports = handle.written_reports();
    assert!(
        reports.iter().all(|r| r[2] != 8),
        "no 0x8071 request at all"
    );
    assert_eq!(reports.iter().filter(|r| is_paint(r)).count(), 1);
}

#[tokio::test]
async fn rgb_per_key_retirement_stops_the_frame_between_requests() {
    for engine in [true, false] {
        let current = Arc::new(AtomicBool::new(true));
        let retire = Arc::clone(&current);
        let respond = partial(false);
        let (shared, handle) = device(move |request| {
            if is_paint(request) {
                retire.store(false, Ordering::Release);
            }
            let mut reply = respond(request)?;
            if !engine && request[2] == 0 && request[3] >> 4 == 0 && request[4..6] == [0x80, 0x71] {
                reply[4] = 0;
            }
            Some(reply)
        })
        .await;
        let error = write()
            .apply_on(&shared, || false, || current.load(Ordering::Acquire))
            .await
            .unwrap_err();
        assert_eq!(error, WriteError::DeviceNotFound);
        let reports = handle.written_reports();
        assert_eq!(reports.iter().filter(|r| is_paint(r)).count(), 1);
        assert!(
            !reports.iter().any(|r| is_frame_end(r)),
            "no request after retirement, engine={engine}"
        );
        let claims: &[&[u8]] = if engine { &[&CLAIM] } else { &[] };
        assert_eq!(
            control_writes(&reports),
            claims,
            "no cleanup through a retired channel"
        );
    }
}

#[tokio::test]
async fn rgb_partial_engine_without_per_key_falls_back_before_claiming() {
    let respond = partial(false);
    let (shared, handle) = device(move |request| {
        let mut reply = respond(request)?;
        if request[2] == 0 && request[3] >> 4 == 0 && request[4..6] == [0x80, 0x81] {
            reply[4] = 0;
        }
        Some(reply)
    })
    .await;
    let error = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WriteError::FeatureUnsupported {
            feature_hex: 0x8080
        }
    );
    let reports = handle.written_reports();
    assert!(control_writes(&reports).is_empty());
    assert!(reports.iter().all(|r| !is_effect(r)));
}

#[tokio::test]
async fn rgb_release_clears_only_cluster_control_and_keeps_the_events() {
    for already_held in [false, true] {
        let (shared, handle) = if already_held {
            device(held(response)).await
        } else {
            device(response).await
        };
        let lease = write()
            .apply_on(&shared, || false, || true)
            .await
            .unwrap()
            .unwrap();
        // Every write identifies the device, held control or not.
        let previous = if already_held { CLAIM[1] } else { PREVIOUS[1] };
        assert_eq!(lease, self::lease(previous, Some(UNIT)));

        release_rgb_control(&shared, lease, || true).await.unwrap();
        let reports = handle.written_reports();
        assert_eq!(
            control_writes(&reports),
            [CLAIM.as_slice(), PREVIOUS.as_slice()],
            "a lease taken over a stale claim still hands the clusters back"
        );
    }
}

#[tokio::test]
async fn rgb_release_never_writes_through_a_retired_channel() {
    let (shared, handle) = device(response).await;
    let lease = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap()
        .unwrap();
    let error = release_rgb_control(&shared, lease, || false)
        .await
        .unwrap_err();
    assert_eq!(error, WriteError::DeviceNotFound);
    assert_eq!(
        control_writes(&handle.written_reports()),
        [CLAIM.as_slice()]
    );
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
                [0x00, 0x03] => 9,
                _ => 0,
            }
        }
        (9, 0) => {
            reply[4] = 1;
            reply[5..9].copy_from_slice(&UNIT);
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

#[tokio::test]
async fn rgb_release_leaves_another_device_on_the_route_alone() {
    // Since the claim, another device was paired to the receiver slot behind
    // the same connection: one with another unit ID, one with an ID where the
    // claimed device had none, and one with none where it had UNIT.
    for (claimed, answering) in [
        (Some(UNIT), Some(OTHER_UNIT)),
        (None, Some(UNIT)),
        (Some(UNIT), None),
    ] {
        let (shared, handle) = device(unit(response, answering)).await;
        let error = release_rgb_control(&shared, lease(PREVIOUS[1], claimed), || true)
            .await
            .unwrap_err();

        assert_eq!(error, WriteError::DeviceNotFound);
        assert!(
            control_writes(&handle.written_reports()).is_empty(),
            "control is never handed back on a device the lease did not claim"
        );
    }
}

#[tokio::test]
async fn rgb_held_writes_identify_the_device_they_reached() {
    // A device paired into the slot since the session's first claim, and
    // already under software control: the write must not pass for a
    // continuation of the departed device's claim.
    let (shared, handle) = device(held(unit(response, Some(OTHER_UNIT)))).await;
    let lease = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(lease, self::lease(CLAIM[1], Some(OTHER_UNIT)));
    assert_eq!(
        handle
            .written_reports()
            .iter()
            .filter(|r| is_unit_id_read(r))
            .count(),
        1
    );
    assert!(!lease.continues(&self::lease(PREVIOUS[1], Some(UNIT))));
    assert!(lease.continues(&self::lease(PREVIOUS[1], Some(OTHER_UNIT))));

    // Its own lease hands it back.
    release_rgb_control(&shared, lease, || true).await.unwrap();
    assert_eq!(
        control_writes(&handle.written_reports()),
        [CLAIM.as_slice(), PREVIOUS.as_slice()]
    );
}

#[tokio::test]
async fn rgb_without_device_information_claims_and_releases_unchecked() {
    let (shared, handle) = device(unit(response, None)).await;
    let lease = write()
        .apply_on(&shared, || false, || true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease, self::lease(PREVIOUS[1], None));
    // Nor does it pass for a continuation of a device that had a unit ID.
    assert!(!lease.continues(&self::lease(PREVIOUS[1], Some(UNIT))));

    release_rgb_control(&shared, lease, || true).await.unwrap();
    assert_eq!(
        control_writes(&handle.written_reports()),
        [CLAIM.as_slice(), PREVIOUS.as_slice()]
    );
}

#[tokio::test]
async fn rgb_unreadable_unit_id_fails_before_claiming() {
    for already_held in [false, true] {
        let unreadable = |request: &[u8]| {
            if is_unit_id_read(request) {
                return Some(feature_error(request, 0x05));
            }
            response(request)
        };
        let (shared, handle) = if already_held {
            device(held(unreadable)).await
        } else {
            device(unreadable).await
        };
        let error = write()
            .apply_on(&shared, || false, || true)
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                WriteError::HidppFeature {
                    operation: HidppOperation::Lighting,
                    feature_hex: 0x0003,
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
        let reports = handle.written_reports();
        assert!(control_writes(&reports).is_empty(), "nothing was claimed");
        assert!(reports.iter().all(|r| !is_effect(r)));
    }
}
